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
    /// Total spawn/connect & initialize budget; None retains request_timeout behavior.
    pub connect_timeout: Option<Duration>,
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
            connect_timeout: None,
            auth: None,
            client_name: "rightkit-mcp".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            on_event: None,
        }
    }
}

impl ClientOptions {
    /// Sets spawn/connect & handshake timeout independently of per-request timeout.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

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

struct ActiveCall {
    session_id: String,
    cancellation: CancellationToken,
}

// Own a launched child until its connection takes over, including abandoned launches.
struct PendingLaunch(Option<LaunchedProcess>);

impl Drop for PendingLaunch {
    fn drop(&mut self) {
        if let Some(mut process) = self.0.take() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
    }
}

fn launch_cancellable(
    request: LaunchRequest,
    launcher: Option<Arc<dyn ProcessLauncher>>,
    cancellation: &CancellationToken,
    timeout: Duration,
) -> Result<LaunchedProcess> {
    if cancellation.is_cancelled() {
        return Err(McpError::Cancelled);
    }
    if timeout.is_zero() {
        return Err(McpError::Timeout);
    }
    let (sender, receiver) = channel();
    let cancelled = cancellation.clone();
    std::thread::spawn(move || {
        let result = if cancelled.is_cancelled() {
            Err(McpError::Cancelled)
        } else {
            match launcher {
                Some(launcher) => launcher.launch(&request),
                None => DefaultProcessLauncher.launch(&request),
            }
            .map(|process| PendingLaunch(Some(process)))
        };
        // Failed send (or unread queued result) drops PendingLaunch & reaps child.
        let _ = sender.send(result);
    });
    let started = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(McpError::Timeout);
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(result) => {
                let mut process = result?;
                if cancellation.is_cancelled() {
                    return Err(McpError::Cancelled);
                }
                if started.elapsed() >= timeout {
                    return Err(McpError::Timeout);
                }
                return Ok(process.0.take().unwrap());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(McpError::Transport("process launcher stopped".into()));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

const STDIO_PREFIX_LIMIT: usize = 64 * 1024;

#[derive(Default)]
struct StdioStartup {
    seen_rpc: bool,
    skipped: usize,
}

impl StdioStartup {
    fn remaining_prefix(&self) -> Option<usize> {
        (!self.seen_rpc).then(|| STDIO_PREFIX_LIMIT - self.skipped)
    }

    fn parse(&mut self, line: &str) -> Result<Option<Value>> {
        let parsed = serde_json::from_str::<Value>(line);
        let (mut message, mut skipped) = match parsed {
            Ok(message) => (Some(message), 0),
            Err(_) if self.seen_rpc => return Ok(None),
            Err(_) => match line.find('{') {
                Some(offset) => match serde_json::from_str::<Value>(&line[offset..]) {
                    Ok(message) => (Some(message), offset),
                    Err(_) => (None, line.len()),
                },
                None => (None, line.len()),
            },
        };
        if !self.seen_rpc
            && message.as_ref().is_some_and(|message| {
                message.get("id").is_none() && message.get("method").is_none()
            })
        {
            message = None;
            skipped = line.len();
        }
        self.skipped = self.skipped.saturating_add(skipped);
        if self.skipped > STDIO_PREFIX_LIMIT {
            return Err(McpError::StdioPrefixTooLarge {
                limit: STDIO_PREFIX_LIMIT,
            });
        }
        if let Some(message) = &message {
            if message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && (message.get("method").and_then(Value::as_str).is_some()
                    || (message.get("id").is_some()
                        && (message.get("result").is_some() || message.get("error").is_some())))
            {
                self.seen_rpc = true;
            }
        }
        Ok(message)
    }
}

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
    active: Arc<Mutex<HashMap<u64, ActiveCall>>>,
    terminal_error: Arc<Mutex<Option<McpError>>>,
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
    prefix_remaining: Option<usize>,
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
        if let Some(remaining) = prefix_remaining {
            if !bytes.contains(&b'{') {
                let prefix = available[..count]
                    .iter()
                    .position(|b| *b == b'{')
                    .unwrap_or(count);
                if bytes.len().saturating_add(prefix) > remaining {
                    return Err(McpError::StdioPrefixTooLarge {
                        limit: STDIO_PREFIX_LIMIT,
                    });
                }
            }
        }
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
        cancellation: &CancellationToken,
        timeout: Duration,
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
        let process = launch_cancellable(
            launch,
            extensions.and_then(|e| e.launcher.clone()),
            cancellation,
            timeout,
        )?;
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
        let active = Arc::new(Mutex::new(HashMap::<u64, ActiveCall>::new()));
        let terminal_error = Arc::new(Mutex::new(None));
        let handlers = Arc::new(Mutex::new(HashMap::new()));
        if let Some(callback) = extensions.and_then(|e| e.elicitation.clone()) {
            lock(&handlers).insert(life.session_id.clone(), callback);
        }
        let limits = extensions.map(|e| e.response_limits);
        let (active2, handlers2) = (active.clone(), handlers.clone());
        let (p2, a2, w2) = (pending.clone(), alive.clone(), stdin.clone());
        let (c2, k2, l2) = (child.clone(), closing.clone(), life.clone());
        let terminal2 = terminal_error.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut startup = StdioStartup::default();
            loop {
                let message = read_stdio_frame(&mut reader, limits, startup.remaining_prefix())
                    .and_then(|line| match line {
                        Some(line) => startup.parse(&line).map(|message| (true, message)),
                        None => Ok((false, None)),
                    });
                let msg = match message {
                    Ok((true, Some(message))) => message,
                    Ok((true, None)) => continue,
                    Ok((false, _)) => break,
                    Err(error) => {
                        *lock(&terminal2) = Some(error.clone());
                        a2.store(false, Ordering::SeqCst);
                        for (_, tx) in lock(&p2).drain() {
                            let _ = tx.send(Err(error.clone()));
                        }
                        break;
                    }
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
                            let (session, cancellation) = {
                                let calls = lock(&active2);
                                if related.is_some() {
                                    match related_id.and_then(|id| calls.get(&id)) {
                                        Some(call) => (
                                            Some(call.session_id.clone()),
                                            Some(call.cancellation.clone()),
                                        ),
                                        None => (None, None),
                                    }
                                } else {
                                    let mut values = calls.values();
                                    match values.next() {
                                        Some(first)
                                            if values.all(|call| {
                                                call.session_id == first.session_id
                                            }) =>
                                        {
                                            (
                                                Some(first.session_id.clone()),
                                                (calls.len() == 1)
                                                    .then(|| first.cancellation.clone()),
                                            )
                                        }
                                        _ => (None, None),
                                    }
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
                                    wire_request_id: id.clone(),
                                    call_cancellation: cancellation,
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
                                            Some(id) => calls.get(&id).is_some_and(|call| {
                                                call.session_id == context.session_id
                                            }),
                                            None => calls
                                                .values()
                                                .any(|call| call.session_id == context.session_id),
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
            for call in lock(&active2).values() {
                call.cancellation.cancel();
            }
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
            terminal_error,
            handlers,
        })
    }

    fn send(&self, msg: &Value) -> Result<()> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(lock(&self.terminal_error)
                .clone()
                .unwrap_or(McpError::Closed));
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
            return Err(lock(&self.terminal_error)
                .clone()
                .unwrap_or(McpError::Closed));
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
            ActiveCall {
                session_id: options
                    .session_id
                    .clone()
                    .unwrap_or_else(|| self.life.session_id.clone()),
                cancellation: CancellationToken::new(),
            },
        );
        let started = Instant::now();
        let message = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let sent = match &options.cancellation {
            Some(token) => self.send_cancellable(message, token, timeout),
            None => self.send(&message),
        };
        if let Err(e) = sent {
            lock(&self.pending).remove(&id);
            if let Some(call) = lock(&self.active).remove(&id) {
                call.cancellation.cancel();
            }
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
        if let Some(call) = lock(&self.active).remove(&id) {
            if matches!(
                result,
                Err(McpError::Timeout | McpError::Cancelled | McpError::Closed)
            ) {
                call.cancellation.cancel();
            }
        }
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
        for call in lock(&self.active).values() {
            call.cancellation.cancel();
        }
        let mut c = lock(&self.child);
        let _ = c.kill();
        let _ = c.wait();
        drop(c);
        self.life
            .emit_safely(LifecycleKind::Stop, "closed", Some(self.pid));
    }
}

pub(crate) fn rpc_outcome(msg: &Value) -> Result<Value> {
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpError::Protocol("response jsonrpc must be 2.0".into()));
    }
    if let Some(e) = msg.get("error") {
        let code = e.get("code").and_then(Value::as_i64).unwrap_or(-32603);
        let message = e
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Err(match e.get("data") {
            Some(data) => McpError::RpcWithData {
                code,
                message,
                data: data.clone(),
            },
            None => McpError::Rpc { code, message },
        });
    }
    msg.get("result")
        .cloned()
        .ok_or_else(|| McpError::Protocol("response is missing result".into()))
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
    fn post(
        &self,
        body: &Value,
        retry_auth: bool,
        timeout: Duration,
    ) -> Result<(u16, String, String)> {
        if timeout.is_zero() {
            return Err(McpError::Timeout);
        }
        let started = Instant::now();
        let mut req = self
            .agent
            .post(&self.url)
            .config()
            .timeout_global(Some(timeout))
            .build()
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
        let mut resp = req.send(body.to_string().as_str()).map_err(|e| match e {
            ureq::Error::Timeout(_) => McpError::Timeout,
            other => McpError::Transport(other.to_string()),
        })?;
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
        resp.body_mut()
            .as_reader()
            .take(16 * 1024 * 1024)
            .read_to_string(&mut text)
            .map_err(|error| {
                if started.elapsed() >= timeout {
                    McpError::Timeout
                } else {
                    McpError::Transport(error.to_string())
                }
            })?;
        if status == 401 {
            if retry_auth {
                if let Some(a) = &self.auth {
                    if a.on_unauthorized(&self.server, www.as_deref())? {
                        return self.post(body, false, timeout.saturating_sub(started.elapsed()));
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

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (status, text, ct) = self.post(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            true,
            timeout,
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
            Conn::Http(h) => h.request(method, params, timeout),
            Conn::AdvancedHttp(h) => h.request(method, params, timeout, &CallOptions::default()),
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
            Conn::AdvancedHttp(h) => h.request(method, params, timeout, options),
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
            Conn::AdvancedHttp(h) => h.notify_with_options(method, timeout, options),
            Conn::Http(h) => h
                .post(&json!({"jsonrpc":"2.0","method":method}), true, timeout)
                .map(|_| ()),
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
    /// New options can be supplied through builders or Default.
    pub fn connect_with_extensions(
        server_name: &str,
        spec: ServerSpec,
        opts: ClientOptions,
        extensions: ClientExtensions,
    ) -> Result<McpClient> {
        Self::connect_with_extensions_cancellable(
            server_name,
            spec,
            opts,
            extensions,
            &CancellationToken::new(),
        )
    }

    /// Opens a cold connection with cancellation across launch, initialize & initialized; abort returns Cancelled & tears down transport.
    pub fn connect_with_extensions_cancellable(
        server_name: &str,
        spec: ServerSpec,
        opts: ClientOptions,
        extensions: ClientExtensions,
        cancellation: &CancellationToken,
    ) -> Result<McpClient> {
        if cancellation.is_cancelled() {
            return Err(McpError::Cancelled);
        }
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
        client.ensure_cancellable(cancellation)?;
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
        let cancellation = cancellation.unwrap_or_default();
        let started = Instant::now();
        let budget = self
            .opts
            .connect_timeout
            .unwrap_or(self.opts.request_timeout);
        let remaining = || -> Result<Duration> {
            if cancellation.is_cancelled() {
                return Err(McpError::Cancelled);
            }
            let timeout = budget.saturating_sub(started.elapsed());
            if timeout.is_zero() {
                return Err(McpError::Timeout);
            }
            Ok(timeout)
        };
        remaining()?;
        let conn = match &self.spec {
            ServerSpec::Stdio { .. } => Conn::Stdio(StdioConn::spawn(
                &self.name,
                &self.spec,
                &self.opts.on_event,
                self.extensions.as_ref(),
                &cancellation,
                remaining()?,
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
            cancellation: Some(cancellation.clone()),
            ..Default::default()
        };
        let init = if matches!(&conn, Conn::Http(_)) {
            conn.request("initialize", params, remaining()?)?
        } else {
            conn.request_with_options("initialize", params, remaining()?, &init_options)?
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
        conn.notify_with_options("notifications/initialized", remaining()?, &init_options)?;
        remaining()?;
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

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn startup_prefix_budget_is_cumulative_and_allows_exact_boundary() {
        let mut startup = StdioStartup::default();
        assert!(startup
            .parse(&"x".repeat(STDIO_PREFIX_LIMIT - 3))
            .unwrap()
            .is_none());
        let response = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        assert_eq!(
            startup.parse(&format!("abc{response}")).unwrap().unwrap()["id"],
            1
        );
        assert!(startup.seen_rpc);
        assert!(startup.parse(&format!("late{response}")).unwrap().is_none());

        let mut startup = StdioStartup::default();
        startup.parse(&"x".repeat(STDIO_PREFIX_LIMIT - 3)).unwrap();
        assert!(matches!(
            startup.parse(&format!("abcd{response}")),
            Err(McpError::StdioPrefixTooLarge {
                limit: STDIO_PREFIX_LIMIT
            })
        ));
    }

    #[test]
    fn unterminated_prefix_is_bounded_before_reading_whole_frame() {
        let mut input = Cursor::new(vec![b'x'; STDIO_PREFIX_LIMIT + 1]);
        assert!(matches!(
            read_stdio_frame(&mut input, None, Some(STDIO_PREFIX_LIMIT)),
            Err(McpError::StdioPrefixTooLarge {
                limit: STDIO_PREFIX_LIMIT
            })
        ));
    }

    #[test]
    fn startup_only_stops_recovery_after_valid_jsonrpc_message() {
        let mut startup = StdioStartup::default();
        startup.parse(r#"{"banner":"JSON but not RPC"}"#).unwrap();
        assert!(!startup.seen_rpc);
        let request = r#"{"jsonrpc":"2.0","id":"server-id","method":"ping"}"#;
        assert!(startup
            .parse(&format!("banner{request}"))
            .unwrap()
            .is_some());
        assert!(startup.seen_rpc);
    }

    #[test]
    fn response_decoding_checks_version_before_errors_and_preserves_null() {
        for response in [
            json!({"id":1,"result":null}),
            json!({"jsonrpc":"1.0","id":1,"result":null}),
            json!({"jsonrpc":2.0,"id":1,"result":null}),
            json!({"jsonrpc":"2.0","id":1}),
            json!({"id":1,"error":{"code":-32601,"message":"bad"}}),
        ] {
            assert!(matches!(rpc_outcome(&response), Err(McpError::Protocol(_))));
        }
        assert_eq!(
            rpc_outcome(&json!({"jsonrpc":"2.0","id":1,"result":null})).unwrap(),
            Value::Null
        );
        assert!(matches!(
            rpc_outcome(&json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"bad"}})),
            Err(McpError::Rpc { code: -32601, .. })
        ));
        assert!(matches!(
            rpc_outcome(
                &json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"bad","data":null}})
            ),
            Err(McpError::RpcWithData {
                data: Value::Null,
                ..
            })
        ));
    }
}
