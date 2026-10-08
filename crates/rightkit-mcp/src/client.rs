//! MCP client: stdio and streamable-HTTP transports with automatic reconnect.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::advanced_http::AdvancedHttp;
use crate::auth::AuthProvider;
use crate::client_features::*;
use crate::error::{McpError, Result};
use crate::events::{emit, next_id, EventSink, LifecycleKind, McpEvent};
use crate::types::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerSpec {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
    },
}

#[derive(Clone)]
pub struct ClientOptions {
    pub request_timeout: Duration,
    pub auth: Option<Arc<dyn AuthProvider>>,
    pub client_name: String,
    pub client_version: String,
    /// Receives start/stop/crash (connections) and denied (pool trust gate) events.
    pub on_event: Option<EventSink>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(60),
            auth: None,
            client_name: "rightkit-mcp".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            on_event: None,
        }
    }
}

impl ClientOptions {
    pub fn on_event(mut self, f: impl Fn(&McpEvent) + Send + Sync + 'static) -> Self {
        self.on_event = Some(Arc::new(f));
        self
    }
}

/// Identity a connection reports its lifecycle under.
#[derive(Clone)]
struct Lifecycle {
    /// Captured once per connection; reader and Drop retain this same identity.
    session_id: String,
    server: String,
    transport: &'static str,
    events: Option<EventSink>,
}

impl Lifecycle {
    fn new(server: &str, transport: &'static str, events: &Option<EventSink>) -> Self {
        Self {
            session_id: next_id("mcpc"),
            server: server.into(),
            transport,
            events: events.clone(),
        }
    }
    fn emit(&self, kind: LifecycleKind, reason: impl Into<String>, pid: Option<u32>) {
        emit(&self.events, || {
            let mut e = McpEvent::new(kind, &self.session_id, reason);
            e.server = Some(self.server.clone());
            e.transport = Some(self.transport);
            e.pid = pid;
            e
        });
    }
    fn emit_safely(&self, kind: LifecycleKind, reason: impl Into<String>, pid: Option<u32>) {
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| self.emit(kind, reason, pid))) {
            // Even a custom panic payload's destructor must not unwind from Drop.
            std::mem::forget(payload);
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value>>>>>;

struct StdioConn {
    child: Arc<Mutex<Box<dyn ProcessChild>>>,
    stdin: Arc<Mutex<Box<dyn Write + Send>>>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    /// Set before an intentional kill so the reader does not report a crash.
    closing: Arc<AtomicBool>,
    life: Lifecycle,
    pid: u32,
    next_id: AtomicU64,
    active: Arc<Mutex<HashMap<u64, String>>>,
    handlers: Arc<Mutex<HashMap<String, ElicitationHandler>>>,
}

struct HttpConn {
    agent: ureq::Agent,
    url: String,
    server: String,
    auth: Option<Arc<dyn AuthProvider>>,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    life: Lifecycle,
    started: AtomicBool,
    next_id: AtomicU64,
}

enum Conn {
    Stdio(StdioConn),
    Http(HttpConn),
    AdvancedHttp(AdvancedHttp),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read_stdio_frame(
    reader: &mut impl BufRead,
    limits: Option<ResponseLimits>,
) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let end = available.iter().position(|b| *b == b'\n').map(|p| p + 1);
        let count = end.unwrap_or(available.len());
        if let Some(limits) = limits {
            let limit = limits.max_frame_bytes.min(limits.max_response_bytes);
            if bytes.len().saturating_add(count) > limit {
                return Err(McpError::ResponseTooLarge { limit });
            }
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            break;
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| McpError::Protocol(e.to_string()))
}

fn client_reply(id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(McpError::Rpc { code, message }) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
        }
        Err(McpError::RpcWithData {
            code,
            message,
            data,
        }) => json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message,"data":data}}),
        Err(error) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":error.to_string()}})
        }
    }
}

impl StdioConn {
    fn spawn(
        server: &str,
        spec: &ServerSpec,
        events: &Option<EventSink>,
        extensions: Option<&ClientExtensions>,
    ) -> Result<StdioConn> {
        let ServerSpec::Stdio {
            command,
            args,
            env,
            cwd,
        } = spec
        else {
            unreachable!()
        };
        let launch = LaunchRequest {
            program: command.clone(),
            args: args.clone(),
            env: env.clone(),
            cwd: cwd.clone(),
        };
        let process = match extensions.and_then(|e| e.launcher.as_ref()) {
            Some(launcher) => launcher.launch(&launch)?,
            None => DefaultProcessLauncher.launch(&launch)?,
        };
        let LaunchedProcess {
            child,
            stdin,
            stdout,
        } = process;
        let pid = child.id();
        let life = Lifecycle::new(server, "stdio", events);
        life.emit(LifecycleKind::Start, "spawned stdio server", Some(pid));
        let stdin = Arc::new(Mutex::new(stdin));
        let child = Arc::new(Mutex::new(child));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let closing = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Mutex::new(HashMap::<u64, String>::new()));
        let handlers = Arc::new(Mutex::new(HashMap::new()));
        if let Some(callback) = extensions.and_then(|e| e.elicitation.clone()) {
            lock(&handlers).insert(life.session_id.clone(), callback);
        }
        let limits = extensions.map(|e| e.response_limits);
        let (active2, handlers2) = (active.clone(), handlers.clone());
        let (p2, a2, w2) = (pending.clone(), alive.clone(), stdin.clone());
        let (c2, k2, l2) = (child.clone(), closing.clone(), life.clone());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let line = match read_stdio_frame(&mut reader, limits) {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        if limits.is_some() {
                            for (_, tx) in lock(&p2).drain() {
                                let _ = tx.send(Err(error.clone()));
                            }
                        }
                        break;
                    }
                };
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let has_method = msg.get("method").is_some();
                match (msg.get("id").and_then(Value::as_u64), has_method) {
                    (Some(id), false) => {
                        let out = rpc_outcome(&msg);
                        if let Some(tx) = lock(&p2).remove(&id) {
                            let _ = tx.send(out);
                        }
                    }
                    (_, true) if msg.get("id").is_some() => {
                        if msg["method"] == "elicitation/create"
                            && (limits.is_some() || !lock(&handlers2).is_empty())
                        {
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            // A request association wins; otherwise only one distinct
                            // active session can safely receive an unassociated request.
                            let related = params
                                .pointer("/_meta/relatedRequestId")
                                .or_else(|| params.get("relatedRequestId"));
                            let related_id = related.and_then(|v| {
                                v.as_u64()
                                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                            });
                            let session = {
                                let calls = lock(&active2);
                                if related.is_some() {
                                    related_id.and_then(|id| calls.get(&id).cloned())
                                } else {
                                    let mut sessions = calls.values();
                                    sessions
                                        .next()
                                        .filter(|first| sessions.all(|s| s == *first))
                                        .cloned()
                                }
                            };
                            let callback = session
                                .as_ref()
                                .and_then(|sid| lock(&handlers2).get(sid).cloned());
                            let id = msg["id"].clone();
                            if let (Some(session_id), Some(callback)) = (session, callback) {
                                let (writer, calls) = (w2.clone(), active2.clone());
                                let context = ElicitationContext {
                                    server: l2.server.clone(),
                                    session_id,
                                };
                                std::thread::spawn(move || {
                                    let result = catch_unwind(AssertUnwindSafe(|| {
                                        callback(&context, &params)
                                    }))
                                    .unwrap_or_else(|_| {
                                        Err(McpError::Protocol(
                                            "elicitation callback panicked".into(),
                                        ))
                                    });
                                    let still_active = {
                                        let calls = lock(&calls);
                                        match related_id {
                                            Some(id) => calls.get(&id) == Some(&context.session_id),
                                            None => {
                                                calls.values().any(|sid| sid == &context.session_id)
                                            }
                                        }
                                    };
                                    let reply = if still_active {
                                        client_reply(id, result)
                                    } else {
                                        client_reply(id, Err(McpError::Cancelled))
                                    };
                                    let mut writer = lock(&writer);
                                    let _ =
                                        writeln!(writer, "{reply}").and_then(|_| writer.flush());
                                });
                            } else {
                                let reply = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"elicitation has no unambiguous active session"}});
                                let mut writer = lock(&w2);
                                let _ = writeln!(writer, "{reply}").and_then(|_| writer.flush());
                            }
                            continue;
                        }
                        // Server-to-client request: answer ping, refuse the rest.
                        let id = msg["id"].clone();
                        let reply = if msg["method"] == "ping" {
                            json!({"jsonrpc":"2.0","id":id,"result":{}})
                        } else {
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not supported by client"}})
                        };
                        let mut writer = lock(&w2);
                        let _ = writeln!(writer, "{reply}").and_then(|_| writer.flush());
                    }
                    _ => {}
                }
            }
            if !k2.load(Ordering::SeqCst) {
                // Unasked end of output: report a crash with the exit status when the OS has it.
                let mut status = None;
                for _ in 0..25 {
                    if k2.load(Ordering::SeqCst) {
                        break;
                    }
                    status = lock(&c2).try_wait().ok().flatten();
                    if status.is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                if !k2.load(Ordering::SeqCst) {
                    let reason = match status {
                        Some(s) => format!("server process exited unexpectedly ({s})"),
                        None => "server closed its output".to_string(),
                    };
                    l2.emit_safely(LifecycleKind::Crash, reason, Some(pid));
                }
            }
            // Publish this connection's crash before EOF wakes a caller that can reconnect.
            // A panicking sink must not prevent pending requests from being released.
            a2.store(false, Ordering::SeqCst);
            for (_, tx) in lock(&p2).drain() {
                let _ = tx.send(Err(McpError::Closed));
            }
        });
        Ok(StdioConn {
            child,
            stdin,
            pending,
            alive,
            closing,
            life,
            pid,
            next_id: AtomicU64::new(1),
            active,
            handlers,
        })
    }

    fn send(&self, msg: &Value) -> Result<()> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(McpError::Closed);
        }
        let mut w = lock(&self.stdin);
        writeln!(w, "{msg}")
            .and_then(|_| w.flush())
            .map_err(|_| McpError::Closed)
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.request_with_options(method, params, timeout, &CallOptions::default())
    }

    /// A blocked pipe write cannot be interrupted portably without retiring its
    /// owning process. Waiting for a response leaves shared stdio intact.
    fn send_cancellable(
        &self,
        message: Value,
        cancellation: &CancellationToken,
        timeout: Duration,
    ) -> Result<()> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(McpError::Closed);
        }
        let writer = self.stdin.clone();
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let mut writer = lock(&writer);
            let result = writeln!(writer, "{message}")
                .and_then(|_| writer.flush())
                .map_err(|_| McpError::Closed);
            let _ = sender.send(result);
        });
        let started = Instant::now();
        loop {
            match receiver.try_recv() {
                Ok(result) => return result,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Err(McpError::Closed),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            let error = if cancellation.is_cancelled() {
                Some(McpError::Cancelled)
            } else if started.elapsed() >= timeout {
                Some(McpError::Timeout)
            } else {
                None
            };
            if let Some(error) = error {
                self.closing.store(true, Ordering::SeqCst);
                self.alive.store(false, Ordering::SeqCst);
                let _ = lock(&self.child).kill();
                return Err(error);
            }
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(result) => return result,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(McpError::Closed)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn request_with_options(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        options: &CallOptions,
    ) -> Result<Value> {
        if options
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(McpError::Cancelled);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        lock(&self.pending).insert(id, tx);
        lock(&self.active).insert(
            id,
            options
                .session_id
                .clone()
                .unwrap_or_else(|| self.life.session_id.clone()),
        );
        let started = Instant::now();
        let message = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let sent = match &options.cancellation {
            Some(token) => self.send_cancellable(message, token, timeout),
            None => self.send(&message),
        };
        if let Err(e) = sent {
            lock(&self.pending).remove(&id);
            lock(&self.active).remove(&id);
            return Err(e);
        }
        let started = if options.cancellation.is_some() {
            started
        } else {
            Instant::now()
        };
        let result = loop {
            if options
                .cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                break Err(McpError::Cancelled);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break Err(McpError::Timeout);
            }
            match rx.recv_timeout(if options.cancellation.is_some() {
                remaining.min(Duration::from_millis(10))
            } else {
                remaining
            }) {
                Ok(result) => break result,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(McpError::Closed)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        };
        lock(&self.pending).remove(&id);
        lock(&self.active).remove(&id);
        if matches!(result, Err(McpError::Timeout | McpError::Cancelled))
            && (method != "initialize" || options.cancellation.is_none())
        {
            let notification = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}});
            if options.cancellation.is_some() {
                let _ = self.send_cancellable(
                    notification,
                    &CancellationToken::new(),
                    Duration::from_millis(200),
                );
            } else {
                let _ = self.send(&notification);
            }
        }
        result
    }
}

impl Drop for StdioConn {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        let mut c = lock(&self.child);
        let _ = c.kill();
        let _ = c.wait();
        drop(c);
        self.life
            .emit_safely(LifecycleKind::Stop, "closed", Some(self.pid));
    }
}

fn rpc_outcome(msg: &Value) -> Result<Value> {
    if let Some(e) = msg.get("error") {
        return Err(McpError::Rpc {
            code: e.get("code").and_then(Value::as_i64).unwrap_or(-32603),
            message: e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
        });
    }
    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
}

impl HttpConn {
    fn new(server: &str, url: &str, opts: &ClientOptions) -> HttpConn {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(opts.request_timeout))
            .max_redirects(0) // a redirect could carry the bearer token to another origin
            .http_status_as_error(false)
            .build()
            .into();
        HttpConn {
            agent,
            url: url.into(),
            server: server.into(),
            auth: opts.auth.clone(),
            session: Mutex::new(None),
            version: Mutex::new(None),
            life: Lifecycle::new(server, "http", &opts.on_event),
            started: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        }
    }

    /// POST one message. Returns (status, body, content-type, session header).
    fn post(&self, body: &Value, retry_auth: bool) -> Result<(u16, String, String)> {
        let mut req = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if let Some(s) = lock(&self.session).as_deref() {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(v) = lock(&self.version).as_deref() {
            req = req.header("MCP-Protocol-Version", v);
        }
        if let Some(a) = &self.auth {
            if let Some(tok) = a.bearer(&self.server)? {
                req = req.header("Authorization", &format!("Bearer {}", tok.expose()));
            }
        }
        let mut resp = req
            .send(body.to_string().as_str())
            .map_err(|e| McpError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *lock(&self.session) = Some(sid.to_string());
        }
        let www = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let mut text = String::new();
        let _ = resp
            .body_mut()
            .as_reader()
            .take(16 * 1024 * 1024)
            .read_to_string(&mut text);
        if status == 401 {
            if retry_auth {
                if let Some(a) = &self.auth {
                    if a.on_unauthorized(&self.server, www.as_deref())? {
                        return self.post(body, false);
                    }
                }
            }
            return Err(McpError::Unauthorized);
        }
        if status == 404 && lock(&self.session).is_some() {
            *lock(&self.session) = None;
            if self.started.swap(false, Ordering::SeqCst) {
                self.life
                    .emit(LifecycleKind::Stop, "server ended the session", None);
            }
            return Err(McpError::Closed); // session expired: caller re-initialises
        }
        Ok((status, text, ct))
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (status, text, ct) = self.post(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            true,
        )?;
        if !(200..300).contains(&status) {
            let snippet: String = text.chars().take(300).collect();
            return Err(McpError::Transport(format!("HTTP {status}: {snippet}")));
        }
        let msg = if ct.contains("text/event-stream") {
            sse_find(&text, id)
                .ok_or_else(|| McpError::Protocol("no response in event stream".into()))?
        } else {
            serde_json::from_str::<Value>(&text).map_err(|e| McpError::Protocol(e.to_string()))?
        };
        rpc_outcome(&msg)
    }
}

impl Drop for HttpConn {
    fn drop(&mut self) {
        if self.started.swap(false, Ordering::SeqCst) {
            self.life.emit_safely(LifecycleKind::Stop, "closed", None);
        }
    }
}

fn sse_find(body: &str, id: u64) -> Option<Value> {
    let mut data = String::new();
    let mut found = None;
    let flush = |data: &mut String, found: &mut Option<Value>| {
        if let Ok(v) = serde_json::from_str::<Value>(data) {
            if v.get("id").and_then(Value::as_u64) == Some(id) && v.get("method").is_none() {
                *found = Some(v);
            }
        }
        data.clear();
    };
    for line in body.lines() {
        if let Some(d) = line.strip_prefix("data:") {
            data.push_str(d.trim_start());
        } else if line.is_empty() && !data.is_empty() {
            flush(&mut data, &mut found);
        }
    }
    if !data.is_empty() {
        flush(&mut data, &mut found);
    }
    found
}

impl Conn {
    fn alive(&self) -> bool {
        match self {
            Conn::Stdio(s) => s.alive.load(Ordering::SeqCst),
            Conn::Http(_) => true,
            Conn::AdvancedHttp(h) => h.alive(),
        }
    }
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        match self {
            Conn::Stdio(s) => s.request(method, params, timeout),
            Conn::Http(h) => h.request(method, params),
            Conn::AdvancedHttp(h) => h.request(method, params, &CallOptions::default()),
        }
    }
    fn notify(&self, method: &str) -> Result<()> {
        let msg = json!({"jsonrpc":"2.0","method":method});
        match self {
            Conn::Stdio(s) => s.send(&msg),
            Conn::Http(h) => h.post(&msg, true).map(|_| ()),
            Conn::AdvancedHttp(h) => h.notify(method),
        }
    }
    fn request_with_options(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        options: &CallOptions,
    ) -> Result<Value> {
        match self {
            Conn::Stdio(s) => s.request_with_options(method, params, timeout, options),
            Conn::AdvancedHttp(h) => h.request(method, params, options),
            Conn::Http(_) => Err(McpError::Config(
                "per-call HTTP options require connect_with_extensions".into(),
            )),
        }
    }
    fn notify_with_options(
        &self,
        method: &str,
        timeout: Duration,
        options: &CallOptions,
    ) -> Result<()> {
        match self {
            Conn::Stdio(s) => match &options.cancellation {
                Some(token) => {
                    s.send_cancellable(json!({"jsonrpc":"2.0","method":method}), token, timeout)
                }
                None => s.send(&json!({"jsonrpc":"2.0","method":method})),
            },
            Conn::AdvancedHttp(h) => h.notify_with_options(method, options),
            Conn::Http(_) => self.notify(method),
        }
    }
}

pub struct McpClient {
    name: String,
    spec: ServerSpec,
    opts: ClientOptions,
    conn: Mutex<Option<Arc<Conn>>>,
    version: Mutex<String>,
    extensions: Option<ClientExtensions>,
}

impl McpClient {
    /// Low level and ungated: spawns/connects immediately. Prefer `ConnectionPool` for config-sourced servers.
    pub fn connect(server_name: &str, spec: ServerSpec, opts: ClientOptions) -> Result<McpClient> {
        let c = McpClient {
            name: server_name.into(),
            spec,
            opts,
            conn: Mutex::new(None),
            version: Mutex::new(String::new()),
            extensions: None,
        };
        c.ensure()?;
        Ok(c)
    }

    /// Opt into cancellable HTTP/SSE, custom launch, callbacks, versions & limits.
    /// Existing ClientOptions/ServerSpec literals remain valid.
    pub fn connect_with_extensions(
        server_name: &str,
        spec: ServerSpec,
        opts: ClientOptions,
        extensions: ClientExtensions,
    ) -> Result<McpClient> {
        if extensions
            .protocol_versions
            .as_ref()
            .is_some_and(|versions| versions.is_empty() || versions.iter().any(String::is_empty))
        {
            return Err(McpError::Config(
                "supported protocol versions must be nonempty".into(),
            ));
        }
        let client = McpClient {
            name: server_name.into(),
            spec,
            opts,
            extensions: Some(extensions),
            conn: Mutex::new(None),
            version: Mutex::new(String::new()),
        };
        client.ensure()?;
        Ok(client)
    }

    /// Current HTTP protocol session or stdio connection's local routing session.
    pub fn session_id(&self) -> Result<Option<String>> {
        let connection = self.ensure()?;
        Ok(match connection.as_ref() {
            Conn::Stdio(s) => Some(s.life.session_id.clone()),
            Conn::Http(h) => lock(&h.session).clone(),
            Conn::AdvancedHttp(h) => h.session_id(),
        })
    }

    pub fn set_elicitation_handler(
        &self,
        session_id: &str,
        handler: ElicitationHandler,
    ) -> Result<()> {
        let connection = self.ensure()?;
        match connection.as_ref() {
            Conn::Stdio(s) => {
                lock(&s.handlers).insert(session_id.into(), handler);
            }
            Conn::AdvancedHttp(h) => h.set_elicitation_handler(session_id, handler),
            Conn::Http(_) => {
                return Err(McpError::Config(
                    "HTTP elicitation requires connect_with_extensions".into(),
                ))
            }
        }
        Ok(())
    }

    pub fn clear_elicitation_handler(&self, session_id: &str) -> Result<()> {
        let connection = self.ensure()?;
        match connection.as_ref() {
            Conn::Stdio(s) => {
                lock(&s.handlers).remove(session_id);
            }
            Conn::AdvancedHttp(h) => h.clear_elicitation_handler(session_id),
            Conn::Http(_) => {}
        }
        Ok(())
    }

    pub fn server_name(&self) -> &str {
        &self.name
    }

    pub fn protocol_version(&self) -> String {
        lock(&self.version).clone()
    }

    fn open(&self) -> Result<Arc<Conn>> {
        self.open_with_cancellation(None)
    }

    fn open_with_cancellation(&self, cancellation: Option<CancellationToken>) -> Result<Arc<Conn>> {
        let conn = match &self.spec {
            ServerSpec::Stdio { .. } => Conn::Stdio(StdioConn::spawn(
                &self.name,
                &self.spec,
                &self.opts.on_event,
                self.extensions.as_ref(),
            )?),
            ServerSpec::Http { url } if self.extensions.is_some() => {
                Conn::AdvancedHttp(AdvancedHttp::new(
                    &self.name,
                    url,
                    &self.opts,
                    self.extensions.as_ref().unwrap(),
                )?)
            }
            ServerSpec::Http { url } => Conn::Http(HttpConn::new(&self.name, url, &self.opts)),
        };
        let configured = self
            .extensions
            .as_ref()
            .and_then(|e| e.protocol_versions.as_ref());
        let preferred = configured
            .and_then(|versions| versions.iter().max())
            .map(String::as_str)
            .unwrap_or(PROTOCOL_VERSION);
        let mut params = json!({
            "protocolVersion": preferred,
            "capabilities": {},
            "clientInfo": {"name": self.opts.client_name, "version": self.opts.client_version},
        });
        if let Some(versions) = configured {
            params["supportedProtocolVersions"] = json!(versions);
        }
        if self
            .extensions
            .as_ref()
            .is_some_and(|e| e.elicitation.is_some())
        {
            params["capabilities"]["elicitation"] = json!({"form":{},"url":{}});
        }
        let init_options = CallOptions {
            cancellation,
            ..Default::default()
        };
        let init = if init_options.cancellation.is_some() {
            conn.request_with_options(
                "initialize",
                params,
                self.opts.request_timeout,
                &init_options,
            )?
        } else {
            conn.request("initialize", params, self.opts.request_timeout)?
        };
        let v = init
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(preferred)
            .to_string();
        if !configured
            .map(|versions| versions.contains(&v))
            .unwrap_or_else(|| SUPPORTED_VERSIONS.contains(&v.as_str()))
        {
            return Err(McpError::Protocol(format!(
                "unsupported protocolVersion {v}"
            )));
        }
        if let Conn::AdvancedHttp(h) = &conn {
            h.set_version(v.clone());
            h.mark_started();
        }
        if let Conn::Http(h) = &conn {
            *lock(&h.version) = Some(v.clone());
            h.started.store(true, Ordering::SeqCst);
            h.life
                .emit(LifecycleKind::Start, "initialized http session", None);
        }
        if init_options.cancellation.is_some() {
            conn.notify_with_options(
                "notifications/initialized",
                self.opts.request_timeout,
                &init_options,
            )?;
        } else {
            conn.notify("notifications/initialized")?;
        }
        *lock(&self.version) = v;
        Ok(Arc::new(conn))
    }

    fn ensure(&self) -> Result<Arc<Conn>> {
        let mut g = lock(&self.conn);
        if let Some(c) = g.as_ref() {
            if c.alive() {
                return Ok(c.clone());
            }
        }
        *g = None; // drop (kills) any dead child before respawn
        let c = self.open()?;
        *g = Some(c.clone());
        Ok(c)
    }

    fn ensure_cancellable(&self, cancellation: &CancellationToken) -> Result<Arc<Conn>> {
        let mut state = loop {
            if cancellation.is_cancelled() {
                return Err(McpError::Cancelled);
            }
            match self.conn.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
            }
        };
        if let Some(connection) = state.as_ref().filter(|connection| connection.alive()) {
            return Ok(connection.clone());
        }
        *state = None;
        let connection = self.open_with_cancellation(Some(cancellation.clone()))?;
        *state = Some(connection.clone());
        Ok(connection)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let c = self.ensure()?;
        match c.request(method, params.clone(), self.opts.request_timeout) {
            Err(McpError::Closed) => {
                *lock(&self.conn) = None;
                self.ensure()?
                    .request(method, params, self.opts.request_timeout)
            }
            other => other,
        }
    }

    /// One raw JSON-RPC call with isolated headers/session & cancellation.
    /// HTTP advanced I/O is selected through connect_with_extensions.
    pub fn request_with_options(
        &self,
        method: &str,
        params: Value,
        options: &CallOptions,
    ) -> Result<Value> {
        if options
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(McpError::Cancelled);
        }
        let connection = match &options.cancellation {
            Some(token) => self.ensure_cancellable(token)?,
            None => self.ensure()?,
        };
        connection.request_with_options(method, params, self.opts.request_timeout, options)
    }

    pub fn call_tool_with_options(
        &self,
        name: &str,
        args: Value,
        options: &CallOptions,
    ) -> Result<ToolResult> {
        self.request_with_options("tools/call", json!({"name":name,"arguments":args}), options)
            .map(|result| ToolResult::from_value(&result))
    }

    fn paged<T: serde::de::DeserializeOwned>(&self, method: &str, key: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor
                .as_ref()
                .map(|c| json!({"cursor": c}))
                .unwrap_or_else(|| json!({}));
            let r = self.request(method, params)?;
            let items = r.get(key).cloned().unwrap_or_else(|| json!([]));
            out.extend(
                serde_json::from_value::<Vec<T>>(items)
                    .map_err(|e| McpError::Protocol(e.to_string()))?,
            );
            match r.get("nextCursor").and_then(Value::as_str) {
                Some(c) if !c.is_empty() => cursor = Some(c.into()),
                _ => return Ok(out),
            }
        }
    }

    pub fn list_tools(&self) -> Result<Vec<ToolDescriptor>> {
        self.paged("tools/list", "tools")
    }
    pub fn call_tool(&self, name: &str, args: Value) -> Result<ToolResult> {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}))?;
        Ok(ToolResult::from_value(&r))
    }
    pub fn list_resources(&self) -> Result<Vec<ResourceDescriptor>> {
        self.paged("resources/list", "resources")
    }
    pub fn read_resource(&self, uri: &str) -> Result<ResourceContents> {
        let r = self.request("resources/read", json!({"uri": uri}))?;
        let first = r
            .pointer("/contents/0")
            .cloned()
            .ok_or_else(|| McpError::ToolNotFound(uri.into()))?;
        serde_json::from_value(first).map_err(|e| McpError::Protocol(e.to_string()))
    }
    pub fn list_prompts(&self) -> Result<Vec<PromptDescriptor>> {
        self.paged("prompts/list", "prompts")
    }
    pub fn get_prompt(&self, name: &str, args: Value) -> Result<Vec<PromptMessage>> {
        let r = self.request("prompts/get", json!({"name": name, "arguments": args}))?;
        Ok(r.get("messages")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(PromptMessage::from_value).collect())
            .unwrap_or_default())
    }
    pub fn ping(&self) -> Result<()> {
        self.request("ping", json!({})).map(|_| ())
    }
    pub fn close(&self) {
        *lock(&self.conn) = None;
    }
}
