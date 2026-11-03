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
