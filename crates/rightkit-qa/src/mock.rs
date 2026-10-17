//! Local HTTP mock for PAID providers: paid-tier scenarios assert the exact
//! requests the product would send, and never make a billed call.
//!
//! Two ways to answer:
//! * [`MockServer::start`]: static [`Route`]s with JSON bodies (the declarative `mock_start` step).
//! * [`MockServer::start_with`]: a handler closure over the request and the mock's own base URL,
//!   returning a [`Reply`] with any status, content type, headers and binary body. Use it for
//!   stateful flows (poll until done, fail the Nth call) and generated media (PNG/MP3/MP4).
//!
//! * [`MockServer::start_static`]: static routes answered with full [`Reply`]s (binary files).
//!
//! **HTTP `Range`.** A `200` reply to a `GET`/`HEAD` honours a single byte range
//! (`bytes=a-b`, `bytes=a-`, suffix `bytes=-n`): `206` with `Content-Range`, or `416` with
//! `Content-Range: bytes */<len>` when unsatisfiable. A missing, malformed or multi-range
//! `Range`, or an `If-Range` that does not match the reply's `ETag`/`Last-Modified`, gets the
//! full `200` body. [`Reply::ignore_range`] always sends the full body (a server without
//! resume); [`Reply::cut_after`] drops the connection after N body bytes (a dropped download).
//!
//! Every connection is served on its own thread, so a slow handler or a client holding a
//! connection open never blocks the next request. Every request is recorded ([`MockServer::seen`]).
use crate::util::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Static route: first route whose method (if any) and path (query ignored) match answers.
#[derive(Debug, Clone)]
pub struct Route {
    pub method: Option<String>,
    pub path: String,
    pub status: u16,
    pub body: Value,
    pub content_type: Option<String>,
}

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    /// Request target as sent, including any query string.
    pub path: String,
    /// Header names as sent; use [`Seen::header`] for case-insensitive lookup.
    pub headers: Vec<(String, String)>,
    /// Body as (lossy) UTF-8 text.
    pub body: String,
    /// Raw body bytes (multipart uploads, binary posts).
    pub body_bytes: Vec<u8>,
}

/// The request a [`MockServer::start_with`] handler receives.
pub type Request = Seen;

impl Seen {
    /// Path without the query string.
    pub fn route(&self) -> &str {
        self.path.split('?').next().unwrap_or("")
    }
    /// The query string, without `?`.
    pub fn query(&self) -> Option<&str> {
        self.path.split_once('?').map(|(_, q)| q)
    }
    /// One query parameter (no percent-decoding).
    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query()?
            .split('&')
            .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v)
    }
    /// Case-insensitive header value.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    /// Body parsed as JSON, or `Null`.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body_bytes).unwrap_or(Value::Null)
    }
    pub fn is(&self, method: &str, route: &str) -> bool {
        self.method.eq_ignore_ascii_case(method) && self.route() == route
    }
    pub fn to_value(&self) -> Value {
        let headers: serde_json::Map<String, Value> = self
            .headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), json!(v)))
            .collect();
        json!({"method": self.method, "path": self.path, "headers": headers, "body": self.body,
               "json": self.json()})
    }
}

/// A handler's answer: any status, content type, extra headers and binary body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub content_type: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    /// 200 with a JSON body.
    pub fn json(v: Value) -> Self {
        Self::status(200, v)
    }
    /// Any status with a JSON body.
    pub fn status(status: u16, v: Value) -> Self {
        Self {
            status,
            content_type: "application/json".into(),
            headers: vec![],
            body: v.to_string().into_bytes(),
        }
    }
    /// 200 with a binary body (generated media: `image/png`, `audio/mpeg`, `video/mp4`, ...).
    pub fn bytes(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: content_type.into(),
            headers: vec![],
            body: body.into(),
        }
    }
    /// 200 with a file's bytes; an unreadable file is a 500 naming it.
    pub fn file(content_type: &str, path: &std::path::Path) -> Self {
        match std::fs::read(path) {
            Ok(b) => Self::bytes(content_type, b),
            Err(e) => Self::status(
                500,
                json!({"error": format!("mock could not read {}: {e}", path.display())}),
            ),
        }
    }
    /// 200 `text/plain`.
    pub fn text(body: &str) -> Self {
        Self::bytes("text/plain; charset=utf-8", body.as_bytes().to_vec())
    }
    /// 404 `{"error":"unmocked"}` (what an unmatched request gets).
    pub fn unmocked() -> Self {
        Self::status(404, json!({"error": "unmocked"}))
    }
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    /// Answer every request with the full body, ignoring any `Range` (a server
    /// that cannot resume). Recorded as the reserved [`IGNORE_RANGE_HEADER`],
    /// which the mock consumes and never sends.
    pub fn ignore_range(self) -> Self {
        self.with_header(IGNORE_RANGE_HEADER, "1")
    }
    /// Close the connection without writing any response: a half-accepted request (the
    /// client sent it, the server never answered). The request is still recorded and logged
    /// (status `0` in [`RequestLog`]). Recorded as the reserved [`DROP_HEADER`], which the
    /// mock consumes and never sends.
    pub fn drop_connection() -> Self {
        Self::bytes("application/octet-stream", Vec::new()).with_header(DROP_HEADER, "1")
    }
    /// True for a [`Reply::drop_connection`] reply.
    pub fn is_drop(&self) -> bool {
        self.headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case(DROP_HEADER))
    }
    /// Send the response head (with the full `Content-Length`), then only the first
    /// `bytes` body bytes, then close the connection: a download dropped mid-transfer.
    /// Applies after any `Range` slicing. Recorded as the reserved [`CUT_AFTER_HEADER`],
    /// which the mock consumes and never sends.
    pub fn cut_after(self, bytes: u64) -> Self {
        self.with_header(CUT_AFTER_HEADER, &bytes.to_string())
    }
}

/// Reserved reply header behind [`Reply::drop_connection`]; consumed, never sent.
pub const DROP_HEADER: &str = "X-RightKit-Mock-Drop";
/// Reserved reply header behind [`Reply::ignore_range`]; consumed, never sent.
pub const IGNORE_RANGE_HEADER: &str = "X-RightKit-Mock-Ignore-Range";
/// Reserved reply header behind [`Reply::cut_after`]; consumed, never sent.
pub const CUT_AFTER_HEADER: &str = "X-RightKit-Mock-Cut-After";

/// How a reply's body is sent for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BodyPlan {
    Full,
    /// Inclusive byte range of the body.
    Partial(u64, u64),
    Unsatisfiable,
}

/// Parse a single `bytes=` range against a body of `len` bytes. `None` means
/// "ignore the header" (absent, malformed or multi-range: serve the full body).
fn plan_range(range: Option<&str>, len: u64) -> Option<BodyPlan> {
    let spec = range?.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if a.is_empty() {
        if !digits(b) {
            return None;
        }
        let n: u64 = b.parse().ok()?;
        if n == 0 || len == 0 {
            return Some(BodyPlan::Unsatisfiable);
        }
        return Some(BodyPlan::Partial(len.saturating_sub(n), len - 1));
    }
    if !digits(a) || !(b.is_empty() || digits(b)) {
        return None;
    }
    let start: u64 = a.parse().ok()?;
    let end = if b.is_empty() {
        None
    } else {
        Some(b.parse::<u64>().ok()?)
    };
    if end.is_some_and(|e| e < start) {
        return None;
    }
    if start >= len {
        return Some(BodyPlan::Unsatisfiable);
    }
    Some(BodyPlan::Partial(
        start,
        end.map_or(len - 1, |e| e.min(len - 1)),
    ))
}

/// `If-Range` holds when it equals the reply's strong `ETag` or its `Last-Modified`.
fn if_range_holds(if_range: &str, headers: &[(String, String)]) -> bool {
    let v = if_range.trim();
    let get = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.trim())
    };
    if v.starts_with('"') || v.starts_with("W/") {
        !v.starts_with("W/") && get("etag").is_some_and(|e| e == v && !e.starts_with("W/"))
    } else {
        get("last-modified").is_some_and(|m| m == v)
    }
}

/// `None` from a handler means "not mocked" (404).
impl From<Option<Reply>> for Reply {
    fn from(r: Option<Reply>) -> Self {
        r.unwrap_or_else(Reply::unmocked)
    }
}

type Handler = dyn Fn(&Request, &str) -> Reply + Send + Sync;

/// One request as the per-request log hook ([`MockOptions::on_request`]) sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestLog {
    /// Empty when the request could not be read (limit hit, malformed, closed early).
    pub method: String,
    /// Request target as sent; empty when the request could not be read.
    pub path: String,
    /// Status written; `0` when the connection was dropped with no response.
    pub status: u16,
    /// Why the mock itself rejected the request (limit, `Transfer-Encoding`, connection cap),
    /// else `None`.
    pub rejected: Option<&'static str>,
}

type LogHook = dyn Fn(&RequestLog) + Send + Sync;

/// Extra behaviour for [`MockServer::start_opts`]. `MockOptions::default()` behaves exactly
/// like [`MockServer::start_with`]: ephemeral port, no limits, no cap, no linger.
#[derive(Clone, Default)]
pub struct MockOptions {
    /// Bind this port on `127.0.0.1` instead of an ephemeral one (`serve --port n`).
    pub port: Option<u16>,
    /// Request head larger than this gets `431`.
    pub max_header_bytes: Option<usize>,
    /// A `Content-Length` larger than this gets `413`.
    pub max_body_bytes: Option<usize>,
    /// Any `Transfer-Encoding` request header gets `501` (only `Content-Length` bodies are read).
    pub reject_transfer_encoding: bool,
    /// At most this many connections are served at once; extra ones get `503` and are closed.
    pub max_connections: Option<usize>,
    /// After a reply, shut down the write side, then read and discard the peer's remaining
    /// bytes for up to this long before closing, so a close with unread input cannot reset
    /// the connection and discard a reply the client has not read.
    pub linger: Option<Duration>,
    /// Called once per request, after it is answered (or dropped, or rejected).
    pub on_request: Option<Arc<LogHook>>,
}

impl MockOptions {
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }
    pub fn max_header_bytes(mut self, n: usize) -> Self {
        self.max_header_bytes = Some(n);
        self
    }
    pub fn max_body_bytes(mut self, n: usize) -> Self {
        self.max_body_bytes = Some(n);
        self
    }
    pub fn reject_transfer_encoding(mut self) -> Self {
        self.reject_transfer_encoding = true;
        self
    }
    pub fn max_connections(mut self, n: usize) -> Self {
        self.max_connections = Some(n);
        self
    }
    pub fn linger(mut self, d: Duration) -> Self {
        self.linger = Some(d);
        self
    }
    pub fn on_request(mut self, f: impl Fn(&RequestLog) + Send + Sync + 'static) -> Self {
        self.on_request = Some(Arc::new(f));
        self
    }
}

/// Cloneable, thread-safe handle that stops a [`MockServer`] (from a signal handler thread,
/// a stdin-close watcher, or a `/shutdown` route) and lets another thread wait for it.
#[derive(Clone)]
pub struct ShutdownHandle {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl ShutdownHandle {
    /// Stop accepting; in-flight connections finish.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
    pub fn is_shutdown(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
    /// Block until [`ShutdownHandle::shutdown`] is called (the main thread of a standalone mock).
    pub fn wait(&self) {
        while !self.is_shutdown() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

struct Limits {
    opts: MockOptions,
}

pub struct MockServer {
    pub base: String,
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
}

impl MockServer {
    /// Static JSON routes; unmatched requests get 404 `{"error":"unmocked"}`.
    pub fn start(routes: Vec<Route>) -> Result<Self> {
        Self::start_with(move |req: &Request, _base: &str| {
            routes
                .iter()
                .find(|r| {
                    r.path == req.route()
                        && r.method
                            .as_deref()
                            .map(|m| m.eq_ignore_ascii_case(&req.method))
                            .unwrap_or(true)
                })
                .map(|r| Reply {
                    status: r.status,
                    content_type: r
                        .content_type
                        .clone()
                        .unwrap_or_else(|| "application/json".into()),
                    headers: vec![],
                    body: r.body.to_string().into_bytes(),
                })
        })
    }

    /// Static routes answered with full [`Reply`]s (binary bodies, extra headers such as a
    /// strong `ETag`, [`Reply::ignore_range`], [`Reply::cut_after`]): `(method, path, reply)`,
    /// first match wins, `None` method matches any, query ignored; unmatched requests get 404.
    pub fn start_static(routes: Vec<(Option<String>, String, Reply)>) -> Result<Self> {
        Self::start_with(move |req: &Request, _base: &str| {
            routes
                .iter()
                .find(|(m, p, _)| {
                    *p == req.route()
                        && m.as_deref()
                            .is_none_or(|m| m.eq_ignore_ascii_case(&req.method))
                })
                .map(|(_, _, r)| r.clone())
        })
    }

    /// Dynamic handler: `handler(request, base_url)` answers every request. `base_url`
    /// (`http://127.0.0.1:<port>`) lets a reply point the client back at the mock (a job's
    /// result URL). Return a [`Reply`], or an `Option<Reply>` where `None` means 404. A
    /// panicking handler yields a 500 instead of killing the server.
    pub fn start_with<R: Into<Reply>>(
        handler: impl Fn(&Request, &str) -> R + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start_opts(MockOptions::default(), handler)
    }

    /// [`MockServer::start_with`] plus [`MockOptions`]: a chosen port, request limits
    /// (`431`/`413`/`501`), a connection cap (`503`), linger/drain, and a per-request log hook.
    /// Rejections are written by the mock; the handler never sees those requests.
    pub fn start_opts<R: Into<Reply>>(
        opts: MockOptions,
        handler: impl Fn(&Request, &str) -> R + Send + Sync + 'static,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", opts.port.unwrap_or(0)))?;
        let addr = listener.local_addr()?;
        let base = format!("http://{addr}");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(move |r: &Request, b: &str| handler(r, b).into());
        let (s2, st2, b2) = (seen.clone(), stop.clone(), base.clone());
        let limits = Arc::new(Limits { opts });
        let active = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                if st2.load(Ordering::SeqCst) {
                    break;
                }
                if limits
                    .opts
                    .max_connections
                    .is_some_and(|cap| active.load(Ordering::SeqCst) >= cap)
                {
                    reject(&mut conn, &limits, 503, "too many connections");
                    continue;
                }
                active.fetch_add(1, Ordering::SeqCst);
                let (s3, h, b, l, a) = (
                    s2.clone(),
                    handler.clone(),
                    b2.clone(),
                    limits.clone(),
                    active.clone(),
                );
                let spawned = std::thread::Builder::new().spawn(move || {
                    let _ = serve(conn, &s3, &b, &*h, &l);
                    a.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            }
        });
        Ok(Self {
            base,
            addr,
            seen,
            stop,
        })
    }

    /// A handle that stops this server from any thread.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            addr: self.addr,
            stop: self.stop.clone(),
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    /// `base + path`.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    pub fn close(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.close();
    }
}

fn serve(
    mut s: TcpStream,
    seen: &Mutex<Vec<Seen>>,
    base: &str,
    handler: &Handler,
    limits: &Limits,
) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let o = &limits.opts;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    let (head_end, content_len) = loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        let end = buf.windows(4).position(|w| w == b"\r\n\r\n");
        if let Some(max) = o.max_header_bytes {
            if end.unwrap_or(buf.len()) > max {
                reject(&mut s, limits, 431, "request headers are too large");
                return Ok(());
            }
        }
        if let Some(p) = end {
            let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
            if o.reject_transfer_encoding
                && head.lines().skip(1).any(|l| {
                    l.split_once(':')
                        .is_some_and(|(k, _)| k.trim() == "transfer-encoding")
                })
            {
                reject(
                    &mut s,
                    limits,
                    501,
                    "Transfer-Encoding is not supported; send Content-Length",
                );
                return Ok(());
            }
            let mut lens = head.lines().skip(1).filter_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.trim() == "content-length")
                    .map(|(_, v)| v.trim().to_string())
            });
            let len = match (lens.next(), lens.next()) {
                (None, _) => 0,
                (Some(v), None) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => {
                    match v.parse::<usize>() {
                        Ok(n) => n,
                        Err(_) => {
                            reject(&mut s, limits, 400, "invalid Content-Length");
                            return Ok(());
                        }
                    }
                }
                _ => {
                    reject(&mut s, limits, 400, "invalid or duplicate Content-Length");
                    return Ok(());
                }
            };
            if o.max_body_bytes.is_some_and(|max| len > max) {
                reject(&mut s, limits, 413, "request body is too large");
                return Ok(());
            }
            break (p + 4, len);
        }
    };
    let Some(total) = head_end.checked_add(content_len) else {
        reject(&mut s, limits, 400, "invalid Content-Length");
        return Ok(());
    };
    while buf.len() < total {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            reject(
                &mut s,
                limits,
                400,
                "request body ended before Content-Length",
            );
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, path) = (
        first.next().unwrap_or("").to_string(),
        first.next().unwrap_or("").to_string(),
    );
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let body_bytes = buf[head_end..total].to_vec();
    let req = Seen {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body_bytes).into_owned(),
        body_bytes,
    };
    seen.lock().unwrap().push(req.clone());
    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&req, base)))
        .unwrap_or_else(|_| Reply::status(500, json!({"error": "mock handler panicked"})));
    let log = |status: u16| {
        if let Some(f) = &o.on_request {
            f(&RequestLog {
                method: req.method.clone(),
                path: req.path.clone(),
                status,
                rejected: None,
            });
        }
    };
    if reply.is_drop() {
        log(0);
        return Ok(());
    }
    let status = reply.status;
    let r = write_reply(&mut s, &req, reply);
    log(status);
    linger(&mut s, o);
    r
}

/// Write a mock-generated JSON error, log it, and close.
fn reject(s: &mut TcpStream, limits: &Limits, status: u16, why: &'static str) {
    let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
    let req = Seen {
        method: String::new(),
        path: String::new(),
        headers: vec![],
        body: String::new(),
        body_bytes: vec![],
    };
    let body = json!({"error": {"code": "bad_request", "message": why}});
    let _ = write_reply(s, &req, Reply::status(status, body));
    if let Some(f) = &limits.opts.on_request {
        f(&RequestLog {
            method: String::new(),
            path: String::new(),
            status,
            rejected: Some(why),
        });
    }
    linger(s, &limits.opts);
}

/// Stop sending, then drain the peer's last bytes so the close is not a reset.
fn linger(s: &mut TcpStream, o: &MockOptions) {
    let Some(d) = o.linger else { return };
    let _ = s.shutdown(std::net::Shutdown::Write);
    let _ = s.set_read_timeout(Some(d));
    let mut sink = [0u8; 1024];
    let deadline = std::time::Instant::now() + d;
    while std::time::Instant::now() < deadline && matches!(s.read(&mut sink), Ok(n) if n > 0) {}
}

fn write_reply(s: &mut TcpStream, req: &Request, mut reply: Reply) -> std::io::Result<()> {
    let take = |reply: &mut Reply, name: &str| -> Option<String> {
        let i = reply
            .headers
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(name))?;
        Some(reply.headers.remove(i).1)
    };
    let ignore_range = take(&mut reply, IGNORE_RANGE_HEADER).is_some();
    let cut_after = take(&mut reply, CUT_AFTER_HEADER).and_then(|v| v.trim().parse::<u64>().ok());
    let len = reply.body.len() as u64;
    let ranged_method =
        req.method.eq_ignore_ascii_case("GET") || req.method.eq_ignore_ascii_case("HEAD");
    let plan = if reply.status == 200 && ranged_method && !ignore_range {
        let honoured = req
            .header("if-range")
            .is_none_or(|v| if_range_holds(v, &reply.headers));
        honoured
            .then(|| plan_range(req.header("range"), len))
            .flatten()
            .unwrap_or(BodyPlan::Full)
    } else {
        BodyPlan::Full
    };
    let (status, reason, body, extra): (u16, &str, &[u8], Option<String>) = match plan {
        BodyPlan::Full => (reply.status, "X", &reply.body[..], None),
        BodyPlan::Partial(a, b) => (
            206,
            "Partial Content",
            &reply.body[a as usize..=b as usize],
            Some(format!(
                "Content-Range: bytes {a}-{b}/{len}\r\nAccept-Ranges: bytes\r\n"
            )),
        ),
        BodyPlan::Unsatisfiable => (
            416,
            "Range Not Satisfiable",
            &[][..],
            Some(format!(
                "Content-Range: bytes */{len}\r\nAccept-Ranges: bytes\r\n"
            )),
        ),
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.content_type,
        body.len()
    );
    head.push_str(extra.as_deref().unwrap_or(""));
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    if !req.method.eq_ignore_ascii_case("HEAD") {
        let n = cut_after.map_or(body.len(), |c| (c as usize).min(body.len()));
        s.write_all(&body[..n])?;
        if n < body.len() {
            s.flush()?;
            return s.shutdown(std::net::Shutdown::Both);
        }
    }
    s.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::request;
    use std::sync::atomic::AtomicUsize;

    fn get(m: &MockServer, method: &str, path: &str, body: Option<&str>) -> crate::http::Response {
        request(
            m.addr,
            method,
            path,
            Some("Bearer k"),
            body,
            Duration::from_secs(5),
        )
        .unwrap()
    }

    #[test]
    fn static_routes_still_answer_json_and_404() {
        let m = MockServer::start(vec![Route {
            method: Some("POST".into()),
            path: "/v1/gen".into(),
            status: 201,
            body: json!({"id": "j1"}),
            content_type: None,
        }])
        .unwrap();
        let r = get(&m, "POST", "/v1/gen?x=1", Some(r#"{"prompt":"p"}"#));
        assert_eq!(r.status, 201);
        assert_eq!(r.content_type, "application/json");
        assert_eq!(
            serde_json::from_slice::<Value>(&r.body).unwrap()["id"],
            "j1"
        );
        assert_eq!(get(&m, "GET", "/v1/gen", None).status, 404);
        let seen = m.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].json()["prompt"], "p");
        assert_eq!(seen[0].header("authorization"), Some("Bearer k"));
        assert_eq!(seen[0].query_param("x"), Some("1"));
        assert_eq!(seen[0].to_value()["json"]["prompt"], "p");
    }

    #[test]
    fn dynamic_handler_is_stateful_binary_and_knows_its_base() {
        let png: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
        let polls = Arc::new(AtomicUsize::new(0));
        let (p2, png2) = (polls.clone(), png.clone());
        let m = MockServer::start_with(move |req: &Request, base: &str| {
            if req.is("POST", "/jobs") {
                Some(Reply::json(json!({"poll": format!("{base}/jobs/1")})))
            } else if req.is("GET", "/jobs/1") {
                let n = p2.fetch_add(1, Ordering::SeqCst);
                Some(if n < 2 {
                    Reply::json(json!({"status": "running"}))
                } else {
                    Reply::json(json!({"status": "done", "url": format!("{base}/out.png")}))
                })
            } else if req.is("GET", "/out.png") {
                Some(Reply::bytes("image/png", png2.clone()).with_header("X-Mock", "1"))
            } else if req.is("GET", "/boom") {
                panic!("handler bug")
            } else {
                None
            }
        })
        .unwrap();
        let first: Value =
            serde_json::from_slice(&get(&m, "POST", "/jobs", Some("{}")).body).unwrap();
        assert_eq!(first["poll"], m.url("/jobs/1"));
        let states: Vec<String> = (0..3)
            .map(|_| {
                serde_json::from_slice::<Value>(&get(&m, "GET", "/jobs/1", None).body).unwrap()
                    ["status"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(states, ["running", "running", "done"]);
        let media = get(&m, "GET", "/out.png", None);
        assert_eq!(media.status, 200);
        assert_eq!(media.content_type, "image/png");
        assert_eq!(media.body, png, "binary body survives byte-for-byte");
        assert_eq!(get(&m, "GET", "/boom", None).status, 500);
        assert_eq!(get(&m, "GET", "/nope", None).status, 404);
        assert_eq!(m.seen().len(), 7);
    }

    #[test]
    fn a_held_connection_does_not_block_other_requests() {
        let m = MockServer::start_with(|_: &Request, _: &str| Reply::text("ok")).unwrap();
        // A client that connects and never sends a request.
        let _idle = TcpStream::connect(m.addr).unwrap();
        let started = std::time::Instant::now();
        let r = get(&m, "GET", "/", None);
        assert_eq!(r.text(), "ok");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    /// Raw GET with extra headers: (status, lowercase headers, body, eof_reached_cleanly).
    fn raw(
        m: &MockServer,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut s = TcpStream::connect(m.addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
        for (k, v) in extra {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        s.write_all(head.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let text = String::from_utf8_lossy(&buf[..split]).to_string();
        let mut lines = text.lines();
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        (status, headers, buf[split + 4..].to_vec())
    }

    fn hdr<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
        h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn data() -> Vec<u8> {
        (0..1000u32).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn static_and_dynamic_binary_replies_serve_byte_ranges() {
        let body = data();
        let file =
            Reply::bytes("application/octet-stream", body.clone()).with_header("ETag", "\"abc\"");
        let st = MockServer::start_static(vec![
            (Some("GET".into()), "/f.bin".into(), file.clone()),
            (Some("HEAD".into()), "/f.bin".into(), file.clone()),
        ])
        .unwrap();
        let f2 = file.clone();
        let dy = MockServer::start_with(move |_: &Request, _: &str| f2.clone()).unwrap();
        for m in [&st, &dy] {
            let (s, h, b) = raw(m, "GET", "/f.bin", &[]);
            assert_eq!((s, b.as_slice()), (200, &body[..]), "no Range: full 200");
            assert_eq!(hdr(&h, "content-range"), None);

            let (s, h, b) = raw(m, "GET", "/f.bin", &[("Range", "bytes=10-19")]);
            assert_eq!(s, 206);
            assert_eq!(b, &body[10..20]);
            assert_eq!(hdr(&h, "content-range"), Some("bytes 10-19/1000"));
            assert_eq!(hdr(&h, "content-length"), Some("10"));
            assert_eq!(hdr(&h, "etag"), Some("\"abc\""));

            let (s, h, b) = raw(m, "GET", "/f.bin", &[("Range", "bytes=990-")]);
            assert_eq!((s, b.as_slice()), (206, &body[990..]));
            assert_eq!(hdr(&h, "content-range"), Some("bytes 990-999/1000"));

            let (s, h, b) = raw(m, "GET", "/f.bin", &[("Range", "bytes=-5")]);
            assert_eq!((s, b.as_slice()), (206, &body[995..]));
            assert_eq!(hdr(&h, "content-range"), Some("bytes 995-999/1000"));

            let (s, h, b) = raw(m, "GET", "/f.bin", &[("Range", "bytes=-5000")]);
            assert_eq!(
                (s, b.as_slice()),
                (206, &body[..]),
                "suffix longer than the body"
            );
            assert_eq!(hdr(&h, "content-range"), Some("bytes 0-999/1000"));

            let (s, h, b) = raw(m, "GET", "/f.bin", &[("Range", "bytes=995-5000")]);
            assert_eq!((s, b.as_slice()), (206, &body[995..]), "end clamped");
            assert_eq!(hdr(&h, "content-range"), Some("bytes 995-999/1000"));

            for unsat in ["bytes=1000-", "bytes=5000-6000", "bytes=-0"] {
                let (s, h, b) = raw(m, "GET", "/f.bin", &[("Range", unsat)]);
                assert_eq!(s, 416, "{unsat}");
                assert!(b.is_empty());
                assert_eq!(hdr(&h, "content-range"), Some("bytes */1000"));
            }
            for ignored in ["bytes=5-1", "bytes=0-1,5-6", "items=0-1", "bytes=x-"] {
                let (s, _, b) = raw(m, "GET", "/f.bin", &[("Range", ignored)]);
                assert_eq!((s, b.as_slice()), (200, &body[..]), "{ignored}");
            }
            // If-Range: matching ETag resumes, anything else gets the full body.
            let (s, _, b) = raw(
                m,
                "GET",
                "/f.bin",
                &[("Range", "bytes=10-"), ("If-Range", "\"abc\"")],
            );
            assert_eq!((s, b.as_slice()), (206, &body[10..]));
            let (s, _, b) = raw(
                m,
                "GET",
                "/f.bin",
                &[("Range", "bytes=10-"), ("If-Range", "\"old\"")],
            );
            assert_eq!((s, b.as_slice()), (200, &body[..]));
            // HEAD: the range head, no body.
            let (s, h, b) = raw(m, "HEAD", "/f.bin", &[("Range", "bytes=0-9")]);
            assert_eq!(s, 206);
            assert!(b.is_empty());
            assert_eq!(hdr(&h, "content-length"), Some("10"));
        }
        assert_eq!(raw(&st, "POST", "/f.bin", &[]).0, 404, "method filter");
        // Non-200 replies are never sliced.
        let err =
            MockServer::start_with(|_: &Request, _: &str| Reply::text("nope").with_status(500))
                .unwrap();
        let (s, _, b) = raw(&err, "GET", "/", &[("Range", "bytes=0-1")]);
        assert_eq!((s, b.as_slice()), (500, &b"nope"[..]));
    }

    #[test]
    fn ignore_range_and_cut_after_simulate_no_resume_and_dropped_connections() {
        let body = data();
        let b2 = body.clone();
        let m = MockServer::start_with(move |req: &Request, _: &str| {
            let r = Reply::bytes("application/octet-stream", b2.clone());
            match req.route() {
                "/norange" => r.ignore_range(),
                "/drop" => r.cut_after(100),
                _ => r,
            }
        })
        .unwrap();
        let (s, h, b) = raw(&m, "GET", "/norange", &[("Range", "bytes=10-")]);
        assert_eq!((s, b.as_slice()), (200, &body[..]));
        assert_eq!(hdr(&h, "content-range"), None);
        assert!(
            h.iter().all(|(k, _)| !k.starts_with("x-rightkit-mock")),
            "reserved headers are never sent: {h:?}"
        );

        let (s, h, b) = raw(&m, "GET", "/drop", &[]);
        assert_eq!(s, 200);
        assert_eq!(
            hdr(&h, "content-length"),
            Some("1000"),
            "head promises the full body"
        );
        assert_eq!(b, &body[..100], "then the connection drops");
        let (s, h, b) = raw(&m, "GET", "/drop", &[("Range", "bytes=500-")]);
        assert_eq!(s, 206);
        assert_eq!(hdr(&h, "content-length"), Some("500"));
        assert_eq!(b, &body[500..600], "cut applies after range slicing");
        // The reserved headers are consumed even on static replies.
        let st =
            MockServer::start_static(vec![(None, "/x".into(), Reply::text("hello").cut_after(2))])
                .unwrap();
        let (_, h, b) = raw(&st, "GET", "/x", &[]);
        assert_eq!(b, b"he");
        assert!(h.iter().all(|(k, _)| !k.starts_with("x-rightkit-mock")));
    }

    fn exchange(addr: SocketAddr, req: &[u8]) -> String {
        let mut st = TcpStream::connect(addr).unwrap();
        st.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = st.write_all(req);
        let mut out = Vec::new();
        let _ = st.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn opts_bind_chosen_port_and_drop_connection_and_log() {
        let free = TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let log = Arc::new(Mutex::new(Vec::<RequestLog>::new()));
        let l2 = log.clone();
        let m = MockServer::start_opts(
            MockOptions::default()
                .port(free)
                .on_request(move |r| l2.lock().unwrap().push(r.clone())),
            |req: &Request, _| {
                if req.is("POST", "/half") {
                    Reply::drop_connection()
                } else {
                    Reply::text("ok")
                }
            },
        )
        .unwrap();
        assert_eq!(m.port(), free);
        assert!(exchange(m.addr, b"POST /half HTTP/1.1\r\nContent-Length: 0\r\n\r\n").is_empty());
        assert!(exchange(m.addr, b"GET /a HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 200"));
        assert_eq!(m.seen().len(), 2);
        let l = log.lock().unwrap();
        assert_eq!((l[0].path.as_str(), l[0].status), ("/half", 0));
        assert_eq!((l[1].path.as_str(), l[1].status), ("/a", 200));
    }

    #[test]
    fn malformed_framing_is_rejected_before_the_handler() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let m = MockServer::start_opts(
            MockOptions::default().linger(Duration::from_millis(50)),
            move |_: &Request, _| {
                c2.fetch_add(1, Ordering::SeqCst);
                Reply::text("ok")
            },
        )
        .unwrap();
        for bad in [
            "POST /x HTTP/1.1\r\nContent-Length: abc\r\n\r\n",
            "POST /x HTTP/1.1\r\nContent-Length: -1\r\n\r\n",
            "POST /x HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n",
            "POST /x HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nA",
        ] {
            assert!(
                exchange(m.addr, bad.as_bytes()).starts_with("HTTP/1.1 400"),
                "{bad}"
            );
        }
        // truncated body: the client half-closes before sending Content-Length bytes
        let mut st = TcpStream::connect(m.addr).unwrap();
        st.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        st.write_all(b"POST /x HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc")
            .unwrap();
        st.shutdown(std::net::Shutdown::Write).unwrap();
        let mut out = Vec::new();
        let _ = st.read_to_end(&mut out);
        assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 400"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(m.seen().is_empty());
        assert!(
            exchange(m.addr, b"POST /x HTTP/1.1\r\nContent-Length: 2\r\n\r\nab")
                .starts_with("HTTP/1.1 200")
        );
    }

    #[test]
    fn opts_limits_answer_431_413_501_without_calling_the_handler() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let m = MockServer::start_opts(
            MockOptions::default()
                .max_header_bytes(256)
                .max_body_bytes(10)
                .reject_transfer_encoding()
                .linger(Duration::from_millis(50)),
            move |_: &Request, _| {
                c2.fetch_add(1, Ordering::SeqCst);
                Reply::text("ok")
            },
        )
        .unwrap();
        let big = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(400));
        assert!(exchange(m.addr, big.as_bytes()).starts_with("HTTP/1.1 431"));
        let r = exchange(
            m.addr,
            b"POST / HTTP/1.1\r\nContent-Length: 11\r\n\r\nhello world",
        );
        assert!(r.starts_with("HTTP/1.1 413"), "{r}");
        let r = exchange(
            m.addr,
            b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        );
        assert!(r.starts_with("HTTP/1.1 501"), "{r}");
        assert!(
            exchange(m.addr, b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello")
                .starts_with("HTTP/1.1 200")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn opts_connection_cap_answers_503_and_shutdown_handle_stops() {
        let m = MockServer::start_opts(
            MockOptions::default().max_connections(1),
            |_: &Request, _| {
                std::thread::sleep(Duration::from_millis(400));
                Reply::text("slow")
            },
        )
        .unwrap();
        let addr = m.addr;
        let t = std::thread::spawn(move || exchange(addr, b"GET /s HTTP/1.1\r\n\r\n"));
        std::thread::sleep(Duration::from_millis(150));
        assert!(exchange(addr, b"GET /s HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503"));
        assert!(t.join().unwrap().starts_with("HTTP/1.1 200"));
        let h = m.shutdown_handle();
        let h2 = h.clone();
        let w = std::thread::spawn(move || h2.wait());
        h.shutdown();
        w.join().unwrap();
        assert!(h.is_shutdown());
    }
}
