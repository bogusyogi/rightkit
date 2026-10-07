//! Server host: serves a [`ToolProvider`] over stdio and loopback HTTP with per-tool admission.

use std::collections::HashSet;
use std::io::{BufRead, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{McpError, Result};
use crate::events::{emit, next_id, EventSink, LifecycleKind, McpEvent};
use crate::types::*;

/// The typed tool surface a host app (e.g. rightkit-agent-tools) plugs in.
pub trait ToolProvider: Send + Sync {
    fn list(&self) -> Vec<ToolDescriptor>;
    /// Unknown tool => `Err(McpError::ToolNotFound)`. `Ok(ToolResult::error(..))` is a tool-level failure.
    fn call(&self, name: &str, args: Value) -> Result<ToolResult>;
    fn resources(&self) -> Vec<ResourceDescriptor> {
        vec![]
    }
    fn read_resource(&self, uri: &str) -> Result<ResourceContents> {
        Err(McpError::ToolNotFound(uri.into()))
    }
    fn prompts(&self) -> Vec<PromptDescriptor> {
        vec![]
    }
    fn get_prompt(&self, name: &str, _args: Value) -> Result<Vec<PromptMessage>> {
        Err(McpError::ToolNotFound(name.into()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerTransport {
    Stdio,
    Http { peer: SocketAddr },
}

pub struct AdmissionRequest<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    pub descriptor: Option<&'a ToolDescriptor>,
    pub transport: ServerTransport,
    pub session: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Allow,
    Deny(String),
}

/// Runs before `ToolProvider::call`. A `Deny` means the provider is never invoked.
pub trait AdmissionHook: Send + Sync {
    fn admit(&self, req: &AdmissionRequest) -> Admission;
    fn expose(&self, _tool: &ToolDescriptor) -> bool {
        true
    }
}

/// Blocks listed tool names and (unless `allow_mutate`) every tool not annotated read-only.
pub struct ReadOnlyPolicy {
    pub allow_mutate: bool,
    pub blocked: Vec<String>,
}

impl AdmissionHook for ReadOnlyPolicy {
    fn admit(&self, req: &AdmissionRequest) -> Admission {
        if self.blocked.iter().any(|b| b == req.tool) {
            return Admission::Deny(format!("tool '{}' is not available through this server", req.tool));
        }
        let read_only = req.descriptor.and_then(|d| d.annotations.read_only) == Some(true);
        if !read_only && !self.allow_mutate {
            return Admission::Deny(format!("tool '{}' is mutating and mutation is not enabled", req.tool));
        }
        Admission::Allow
    }
    fn expose(&self, tool: &ToolDescriptor) -> bool {
        !self.blocked.contains(&tool.name)
    }
}

pub struct HttpServeOptions {
    pub port: u16,
    /// `None` => a random 192-bit token is generated.
    pub token: Option<String>,
}

struct Inner {
    name: String,
    version: String,
    instructions: Option<String>,
    provider: Arc<dyn ToolProvider>,
    admission: Option<Arc<dyn AdmissionHook>>,
    events: Option<EventSink>,
}

#[derive(Clone)]
pub struct ServerHost {
    inner: Arc<Inner>,
}

struct Ctx {
    transport: ServerTransport,
    session: Option<String>,
    /// Local id of this serving run (`mcps-N`), used for lifecycle events.
    run: String,
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn rpc_error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

impl ServerHost {
    pub fn new(name: &str, version: &str, provider: Arc<dyn ToolProvider>) -> Self {
        ServerHost {
            inner: Arc::new(Inner {
                name: name.into(),
                version: version.into(),
                instructions: None,
                provider,
                admission: None,
                events: None,
            }),
        }
    }

    fn rebuild(self, f: impl FnOnce(&mut Inner)) -> Self {
        let i = &*self.inner;
        let mut n = Inner {
            name: i.name.clone(),
            version: i.version.clone(),
            instructions: i.instructions.clone(),
            provider: i.provider.clone(),
            admission: i.admission.clone(),
            events: i.events.clone(),
        };
        f(&mut n);
        ServerHost { inner: Arc::new(n) }
    }

    pub fn with_admission(self, hook: Arc<dyn AdmissionHook>) -> Self {
        self.rebuild(|i| i.admission = Some(hook))
    }

    /// Receives start/stop/crash (serving runs) and denied (admission, HTTP gates) events.
    pub fn with_events(self, sink: EventSink) -> Self {
        self.rebuild(|i| i.events = Some(sink))
    }

    pub fn on_event(self, f: impl Fn(&McpEvent) + Send + Sync + 'static) -> Self {
        self.with_events(Arc::new(f))
    }

    fn emit(&self, kind: LifecycleKind, run: &str, transport: &'static str, reason: impl Into<String>, tool: Option<&str>) {
        emit(&self.inner.events, || {
            let mut e = McpEvent::new(kind, run, reason);
            e.server = Some(self.inner.name.clone());
            e.transport = Some(transport);
            e.tool = tool.map(str::to_string);
            e
        });
    }

    pub fn with_instructions(self, text: &str) -> Self {
        self.rebuild(|i| i.instructions = Some(text.into()))
    }

    /// Blocking loop on process stdin/stdout; returns at EOF.
    pub fn serve_stdio(&self) -> Result<()> {
        let stdin = std::io::stdin();
        self.serve_stdio_on(stdin.lock(), std::io::stdout())
    }

    pub fn serve_stdio_on(&self, reader: impl BufRead, writer: impl Write) -> Result<()> {
        let ctx = Ctx { transport: ServerTransport::Stdio, session: None, run: next_id("mcps") };
        self.emit(LifecycleKind::Start, &ctx.run, "stdio", "serving stdio", None);
        match self.stdio_loop(reader, writer, &ctx) {
            Ok(()) => {
                self.emit(LifecycleKind::Stop, &ctx.run, "stdio", "input closed", None);
                Ok(())
            }
            Err(e) => {
                self.emit(LifecycleKind::Crash, &ctx.run, "stdio", format!("serving failed: {e}"), None);
                Err(e)
            }
        }
    }

    fn stdio_loop(&self, reader: impl BufRead, mut writer: impl Write, ctx: &Ctx) -> Result<()> {
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => self.handle_any(msg, ctx).0,
                Err(e) => Some(rpc_error(&Value::Null, -32700, format!("parse error: {e}"))),
            };
            if let Some(r) = reply {
                writeln!(writer, "{r}")?;
                writer.flush()?;
            }
        }
        Ok(())
    }

    /// Returns (reply, issued-session-id-if-initialize).
    fn handle_any(&self, msg: Value, ctx: &Ctx) -> (Option<Value>, Option<String>) {
        if let Value::Array(items) = msg {
            let replies: Vec<Value> = items.into_iter().filter_map(|m| self.handle_one(&m, ctx)).collect();
            return (if replies.is_empty() { None } else { Some(Value::Array(replies)) }, None);
        }
        let is_init = msg.get("method").and_then(Value::as_str) == Some("initialize");
        let reply = self.handle_one(&msg, ctx);
        let sid = if is_init && reply.as_ref().is_some_and(|r| r.get("result").is_some()) {
            Some(random_hex(16))
        } else {
            None
        };
        (reply, sid)
    }

    fn handle_one(&self, msg: &Value, ctx: &Ctx) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str)?;
        let id = match msg.get("id") {
            Some(id) if !id.is_null() => id.clone(),
            _ => return None, // notification
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let out = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(&params, ctx),
            "resources/list" => Ok(json!({"resources": self.inner.provider.resources()})),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                self.inner.provider.read_resource(uri).map(|c| json!({"contents": [c]}))
            }
            "prompts/list" => Ok(json!({"prompts": self.inner.provider.prompts()})),
            "prompts/get" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                self.inner.provider.get_prompt(name, args).map(|m| {
                    json!({"messages": m.iter().map(PromptMessage::to_value).collect::<Vec<_>>()})
                })
            }
            _ => return Some(rpc_error(&id, -32601, format!("method not found: {method}"))),
        };
        Some(match out {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(McpError::ToolNotFound(n)) => rpc_error(&id, -32602, format!("unknown: {n}")),
            Err(McpError::Rpc { code, message }) => rpc_error(&id, code, message),
            Err(e) => rpc_error(&id, -32603, e.to_string()),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
        let version = if SUPPORTED_VERSIONS.contains(&requested) { requested } else { PROTOCOL_VERSION };
        let p = &self.inner.provider;
        let mut caps = json!({"tools": {}});
        if !p.resources().is_empty() {
            caps["resources"] = json!({});
        }
        if !p.prompts().is_empty() {
            caps["prompts"] = json!({});
        }
        let mut r = json!({
            "protocolVersion": version,
            "capabilities": caps,
            "serverInfo": {"name": self.inner.name, "version": self.inner.version},
        });
        if let Some(i) = &self.inner.instructions {
            r["instructions"] = json!(i);
        }
        r
    }

    fn tools_list(&self) -> Value {
        let tools: Vec<ToolDescriptor> = self
            .inner
            .provider
            .list()
            .into_iter()
            .filter(|t| self.inner.admission.as_ref().is_none_or(|a| a.expose(t)))
            .collect();
        json!({"tools": tools})
    }

    fn tools_call(&self, params: &Value, ctx: &Ctx) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::Rpc { code: -32602, message: "missing tool name".into() })?;
        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let descriptor = self.inner.provider.list().into_iter().find(|t| t.name == name);
        if let Some(hook) = &self.inner.admission {
            let hidden = descriptor.as_ref().is_some_and(|d| !hook.expose(d));
            let verdict = if hidden {
                Admission::Deny(format!("tool '{name}' is not exposed"))
            } else {
                hook.admit(&AdmissionRequest {
                    tool: name,
                    args: &args,
                    descriptor: descriptor.as_ref(),
                    transport: ctx.transport,
                    session: ctx.session.as_deref(),
                })
            };
            if let Admission::Deny(reason) = verdict {
                let transport = match ctx.transport {
                    ServerTransport::Stdio => "stdio",
                    ServerTransport::Http { .. } => "http",
                };
                self.emit(LifecycleKind::Denied, &ctx.run, transport, reason.clone(), Some(name));
                let mut r = ToolResult::error(format!("denied: {reason}"));
                r.structured = Some(json!({"denied": true, "reason": reason}));
                return Ok(r.to_value());
            }
        }
        if descriptor.is_none() {
            return Err(McpError::ToolNotFound(name.into()));
        }
        self.inner.provider.call(name, args).map(|r| r.to_value())
    }

    /// Loopback-only HTTP server on `POST /mcp`, bearer-token and Origin gated.
    pub fn serve_http(&self, opts: HttpServeOptions) -> Result<HttpServer> {
        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), opts.port))?;
        let addr = listener.local_addr()?;
        let token = opts.token.unwrap_or_else(|| random_hex(24));
        let stop = Arc::new(AtomicBool::new(false));
        let sessions: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let run = next_id("mcps");
        self.emit(LifecycleKind::Start, &run, "http", format!("serving http on {addr}"), None);
        let host = self.clone();
        let (stop2, token2, run2) = (stop.clone(), token.clone(), run.clone());
        let join = std::thread::spawn(move || {
            let mut failures = 0u32;
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    return;
                }
                let stream = match conn {
                    Ok(s) => {
                        failures = 0;
                        s
                    }
                    Err(e) => {
                        failures += 1;
                        if failures > 100 {
                            host.emit(LifecycleKind::Crash, &run2, "http", format!("listener failed: {e}"), None);
                            return;
                        }
                        continue;
                    }
                };
                let (h, t, s, r) = (host.clone(), token2.clone(), sessions.clone(), run2.clone());
                std::thread::spawn(move || {
                    let _ = handle_http(&h, stream, &t, &s, &r);
                });
            }
        });
        Ok(HttpServer { addr, token, stop, join: Some(join), host: self.clone(), run, stopped: false })
    }
}

pub struct HttpServer {
    addr: SocketAddr,
    token: String,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    host: ServerHost,
    run: String,
    stopped: bool,
}

impl HttpServer {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.addr.port())
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    /// Local id this serving run reports lifecycle events under.
    pub fn session_id(&self) -> &str {
        &self.run
    }
    pub fn shutdown(mut self) {
        self.stop_now();
    }
    fn stop_now(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(500));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if !std::mem::replace(&mut self.stopped, true) {
            self.host.emit(LifecycleKind::Stop, &self.run, "http", "shutdown", None);
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop_now();
    }
}

/// Origin/Host loopback check: exact host match (a substring check would accept `localhost.evil.com`).
pub(crate) fn host_is_loopback(value: &str, has_scheme: bool) -> bool {
    let rest = if has_scheme {
        match value.split_once("://") {
            Some((_, r)) => r,
            None => return false,
        }
    } else {
        value
    };
    let hp = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if hp.contains('@') {
        return false;
    }
    let host = if let Some(r) = hp.strip_prefix('[') {
        r.split(']').next().unwrap_or(r)
    } else {
        hp.rsplit_once(':').map(|(h, _)| h).unwrap_or(hp)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().min(b.len()) {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn respond(s: &mut TcpStream, status: &str, extra: &[(&str, String)], body: &str) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(body.as_bytes())?;
    s.flush()
}

const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 8 * 1024 * 1024;

fn handle_http(
    host: &ServerHost,
    mut s: TcpStream,
    token: &str,
    sessions: &Mutex<HashSet<String>>,
    run: &str,
) -> std::io::Result<()> {
    let deny = |why: &str| host.emit(LifecycleKind::Denied, run, "http", why, None);
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let peer = s.peer_addr()?;
    // Read head.
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > MAX_HEAD {
            return respond(&mut s, "431 Request Header Fields Too Large", &[], "{}");
        }
        let n = s.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let hdr = |name: &str| -> Option<String> {
        head.split("\r\n").skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    // Gate order: path, Host (rebinding), Origin, bearer.
    if path.split('?').next() != Some("/mcp") {
        return respond(&mut s, "404 Not Found", &[], "{}");
    }
    if hdr("host").is_some_and(|h| !host_is_loopback(&h, false)) {
        deny("non-loopback Host header");
        return respond(&mut s, "403 Forbidden", &[], r#"{"error":"bad host"}"#);
    }
    if hdr("origin").is_some_and(|o| !host_is_loopback(&o, true)) {
        deny("non-loopback Origin header");
        return respond(&mut s, "403 Forbidden", &[], r#"{"error":"bad origin"}"#);
    }
    let authed = hdr("authorization")
        .and_then(|a| a.strip_prefix("Bearer ").map(str::to_string))
        .is_some_and(|t| ct_eq(&t, token));
    if !authed {
        deny("missing or wrong bearer token");
        return respond(&mut s, "401 Unauthorized", &[("WWW-Authenticate", "Bearer".into())], r#"{"error":"unauthorized"}"#);
    }
    match method {
        "POST" => {}
        "DELETE" => {
            if let Some(sid) = hdr("mcp-session-id") {
                sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&sid);
            }
            return respond(&mut s, "200 OK", &[], "{}");
        }
        _ => return respond(&mut s, "405 Method Not Allowed", &[("Allow", "POST, DELETE".into())], "{}"),
    }
    let len: usize = hdr("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return respond(&mut s, "413 Payload Too Large", &[], "{}");
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    let session = hdr("mcp-session-id");
    if let Some(sid) = &session {
        if !sessions.lock().unwrap_or_else(|e| e.into_inner()).contains(sid) {
            return respond(&mut s, "404 Not Found", &[], r#"{"error":"unknown session"}"#);
        }
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let r = rpc_error(&Value::Null, -32700, format!("parse error: {e}"));
            return respond(&mut s, "400 Bad Request", &[], &r.to_string());
        }
    };
    let ctx = Ctx { transport: ServerTransport::Http { peer }, session, run: run.to_string() };
    let (reply, new_sid) = host.handle_any(msg, &ctx);
    match reply {
        None => respond(&mut s, "202 Accepted", &[], ""),
        Some(r) => {
            let mut extra = vec![];
            if let Some(sid) = new_sid {
                sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(sid.clone());
                extra.push(("Mcp-Session-Id", sid));
            }
            respond(&mut s, "200 OK", &extra, &r.to_string())
        }
    }
}
