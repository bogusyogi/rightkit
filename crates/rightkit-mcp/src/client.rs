//! MCP client: stdio and streamable-HTTP transports with automatic reconnect.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::auth::AuthProvider;
use crate::error::{McpError, Result};
use crate::types::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerSpec {
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String>, cwd: Option<PathBuf> },
    Http { url: String },
}

#[derive(Clone)]
pub struct ClientOptions {
    pub request_timeout: Duration,
    pub auth: Option<Arc<dyn AuthProvider>>,
    pub client_name: String,
    pub client_version: String,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(60),
            auth: None,
            client_name: "rightkit-mcp".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value>>>>>;

struct StdioConn {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    next_id: AtomicU64,
}

struct HttpConn {
    agent: ureq::Agent,
    url: String,
    server: String,
    auth: Option<Arc<dyn AuthProvider>>,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    next_id: AtomicU64,
}

enum Conn {
    Stdio(StdioConn),
    Http(HttpConn),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl StdioConn {
    fn spawn(spec: &ServerSpec) -> Result<StdioConn> {
        let ServerSpec::Stdio { command, args, env, cwd } = spec else { unreachable!() };
        let mut cmd = Command::new(command);
        cmd.args(args).envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Some(c) = cwd {
            cmd.current_dir(c);
        }
        let mut child = cmd.spawn().map_err(|e| McpError::Transport(format!("spawn '{command}': {e}")))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().ok_or(McpError::Closed)?));
        let stdout = child.stdout.take().ok_or(McpError::Closed)?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (p2, a2, w2) = (pending.clone(), alive.clone(), stdin.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                let has_method = msg.get("method").is_some();
                match (msg.get("id").and_then(Value::as_u64), has_method) {
                    (Some(id), false) => {
                        let out = rpc_outcome(&msg);
                        if let Some(tx) = lock(&p2).remove(&id) {
                            let _ = tx.send(out);
                        }
                    }
                    (_, true) if msg.get("id").is_some() => {
                        // Server-to-client request: answer ping, refuse the rest.
                        let id = msg["id"].clone();
                        let reply = if msg["method"] == "ping" {
                            json!({"jsonrpc":"2.0","id":id,"result":{}})
                        } else {
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not supported by client"}})
                        };
                        let _ = writeln!(lock(&w2), "{reply}");
                    }
                    _ => {}
                }
            }
            a2.store(false, Ordering::SeqCst);
            for (_, tx) in lock(&p2).drain() {
                let _ = tx.send(Err(McpError::Closed));
            }
        });
        Ok(StdioConn { child: Mutex::new(child), stdin, pending, alive, next_id: AtomicU64::new(1) })
    }

    fn send(&self, msg: &Value) -> Result<()> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(McpError::Closed);
        }
        let mut w = lock(&self.stdin);
        writeln!(w, "{msg}").and_then(|_| w.flush()).map_err(|_| McpError::Closed)
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        lock(&self.pending).insert(id, tx);
        if let Err(e) = self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})) {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(_) => {
                lock(&self.pending).remove(&id);
                let _ = self.send(&json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}}));
                Err(McpError::Timeout)
            }
        }
    }
}

impl Drop for StdioConn {
    fn drop(&mut self) {
        let mut c = lock(&self.child);
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn rpc_outcome(msg: &Value) -> Result<Value> {
    if let Some(e) = msg.get("error") {
        return Err(McpError::Rpc {
            code: e.get("code").and_then(Value::as_i64).unwrap_or(-32603),
            message: e.get("message").and_then(Value::as_str).unwrap_or("").into(),
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
        let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if let Some(sid) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *lock(&self.session) = Some(sid.to_string());
        }
        let www = resp.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).map(str::to_string);
        let mut text = String::new();
        let _ = resp.body_mut().as_reader().take(16 * 1024 * 1024).read_to_string(&mut text);
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
            return Err(McpError::Closed); // session expired: caller re-initialises
        }
        Ok((status, text, ct))
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (status, text, ct) =
            self.post(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}), true)?;
        if !(200..300).contains(&status) {
            let snippet: String = text.chars().take(300).collect();
            return Err(McpError::Transport(format!("HTTP {status}: {snippet}")));
        }
        let msg = if ct.contains("text/event-stream") {
            sse_find(&text, id).ok_or_else(|| McpError::Protocol("no response in event stream".into()))?
        } else {
            serde_json::from_str::<Value>(&text).map_err(|e| McpError::Protocol(e.to_string()))?
        };
        rpc_outcome(&msg)
    }
}

fn sse_find(body: &str, id: u64) -> Option<Value> {
    let mut data = String::new();
    let mut found = None;
    let mut flush = |data: &mut String, found: &mut Option<Value>| {
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
        }
    }
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        match self {
            Conn::Stdio(s) => s.request(method, params, timeout),
            Conn::Http(h) => h.request(method, params),
        }
    }
    fn notify(&self, method: &str) -> Result<()> {
        let msg = json!({"jsonrpc":"2.0","method":method});
        match self {
            Conn::Stdio(s) => s.send(&msg),
            Conn::Http(h) => h.post(&msg, true).map(|_| ()),
        }
    }
}

pub struct McpClient {
    name: String,
    spec: ServerSpec,
    opts: ClientOptions,
    conn: Mutex<Option<Arc<Conn>>>,
    version: Mutex<String>,
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
        };
        c.ensure()?;
        Ok(c)
    }

    pub fn server_name(&self) -> &str {
        &self.name
    }

    pub fn protocol_version(&self) -> String {
        lock(&self.version).clone()
    }

    fn open(&self) -> Result<Arc<Conn>> {
        let conn = match &self.spec {
            ServerSpec::Stdio { .. } => Conn::Stdio(StdioConn::spawn(&self.spec)?),
            ServerSpec::Http { url } => Conn::Http(HttpConn::new(&self.name, url, &self.opts)),
        };
        let init = conn.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": self.opts.client_name, "version": self.opts.client_version},
            }),
            self.opts.request_timeout,
        )?;
        let v = init.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION).to_string();
        if !SUPPORTED_VERSIONS.contains(&v.as_str()) {
            return Err(McpError::Protocol(format!("unsupported protocolVersion {v}")));
        }
        if let Conn::Http(h) = &conn {
            *lock(&h.version) = Some(v.clone());
        }
        conn.notify("notifications/initialized")?;
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

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let c = self.ensure()?;
        match c.request(method, params.clone(), self.opts.request_timeout) {
            Err(McpError::Closed) => {
                *lock(&self.conn) = None;
                self.ensure()?.request(method, params, self.opts.request_timeout)
            }
            other => other,
        }
    }

    fn paged<T: serde::de::DeserializeOwned>(&self, method: &str, key: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map(|c| json!({"cursor": c})).unwrap_or_else(|| json!({}));
            let r = self.request(method, params)?;
            let items = r.get(key).cloned().unwrap_or_else(|| json!([]));
            out.extend(serde_json::from_value::<Vec<T>>(items).map_err(|e| McpError::Protocol(e.to_string()))?);
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
        let first = r.pointer("/contents/0").cloned().ok_or_else(|| McpError::ToolNotFound(uri.into()))?;
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
