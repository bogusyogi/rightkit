//! A tolerant, single-use loopback redirect listener (RFC 8252 section 7.3).
//!
//! [`CallbackListener::bind`] listens on `127.0.0.1` and/or `[::1]` only (unless the caller opts
//! in through [`CallbackBind::custom_bind_address`]), on an ephemeral port unless the provider registration fixes
//! one. [`CallbackListener::wait`] then serves
//! connections until exactly one request carries the expected `state` on the redirect path:
//! favicon fetches, CORS preflights, other methods and paths, malformed requests, stalled
//! connections and callbacks with a wrong or missing `state` are answered and ignored, so
//! none of them can end or poison the flow. `wait` consumes the listener, so it is single
//! use, and the sockets close when it returns.
//!
//! [`CallbackListener::wait_post_json`] serves the same single-use flow for a provider that
//! POSTs its result as CORS-preflighted JSON instead of redirecting the browser with a GET.

use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use rightkit_http::{percent_decode, CancelToken, Secret};
use serde_json::Value;

use crate::pkce::constant_time_eq;
use crate::OAuthError;

/// Longest wait [`CallbackListener::wait`] accepts.
pub const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
/// Shortest `state` a listener accepts, so an empty or trivially guessable value cannot be set.
pub const MIN_STATE_LEN: usize = 16;
/// Longest request line read from one connection.
const REQUEST_LINE_MAX: usize = 8 * 1024;
/// Longest header block [`CallbackListener::wait_post_json`] reads from one connection.
const HEADERS_MAX: usize = 16 * 1024;
/// Largest JSON body [`CallbackListener::wait_post_json`] accepts.
pub const MAX_POST_BODY: usize = 64 * 1024;
/// Most header lines one POST-mode request may carry.
const HEADER_LINES_MAX: usize = 64;
/// How long one connection may take to deliver its request line.
const CONNECTION_READ_BUDGET: Duration = Duration::from_secs(5);
/// Most connections held open at once; further ones are closed at once.
const MAX_PENDING: usize = 16;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const PAGE_COMPLETE: &str = "Authorization complete. You may close this window.";
const PAGE_NOT_COMPLETED: &str = "Sign-in was not completed. You may close this window.";

/// Which address the listener serves and `redirect_uri` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopbackHost {
    /// `127.0.0.1` (the default).
    #[default]
    V4,
    /// `[::1]`.
    V6,
    /// Listen on `127.0.0.1` and, when the host supports it, `[::1]` on the same port;
    /// `redirect_uri` names `127.0.0.1`.
    Both,
}

/// Listener settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackOptions {
    pub host: LoopbackHost,
    /// `0` picks an ephemeral port. A fixed port that is busy is an error.
    pub port: u16,
    /// The redirect path, starting with `/`.
    pub path: String,
}

impl Default for CallbackOptions {
    fn default() -> Self {
        Self {
            host: LoopbackHost::V4,
            port: 0,
            path: "/callback".into(),
        }
    }
}

/// [`CallbackOptions`] plus the opt-in extras, built with methods so adding one is not a
/// breaking change. Pass it to [`CallbackListener::bind_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackBind {
    options: CallbackOptions,
    redirect_host: Option<String>,
    custom_ip: Option<IpAddr>,
}

impl CallbackBind {
    pub fn new(options: CallbackOptions) -> Self {
        Self {
            options,
            redirect_host: None,
            custom_ip: None,
        }
    }

    /// How `redirect_uri` spells the host, when the provider registration needs a spelling
    /// other than the bound literal IP (OpenAI/ChatGPT registers `http://localhost:1455/...`).
    /// It changes only the text of `redirect_uri`, never what is bound. On a loopback listener
    /// it must be `localhost`, `127.0.0.1` or `[::1]`; with
    /// [`Self::custom_bind_address`] any host name or IP literal (`[v6]` bracketed). Without
    /// it `redirect_uri` names the bound address.
    pub fn redirect_host(mut self, host: impl Into<String>) -> Self {
        self.redirect_host = Some(host.into());
        self
    }

    /// An explicit opt-in to bind a different address (replacing `options.host`), for a host
    /// where the browser reaches the app through a forwarded or bridged interface (WSL, a
    /// container). **This can bind a non-loopback address**, so other machines may be able to
    /// reach the port: use it only when the environment requires it. The state check, the
    /// exact path and the single use still apply, but nothing keeps a remote peer from
    /// connecting and probing. Multicast addresses are refused; an unspecified address
    /// (`0.0.0.0`, `::`) needs [`Self::redirect_host`] because it is not a usable redirect
    /// host.
    pub fn custom_bind_address(mut self, ip: IpAddr) -> Self {
        self.custom_ip = Some(ip);
        self
    }
}

impl From<CallbackOptions> for CallbackBind {
    fn from(options: CallbackOptions) -> Self {
        Self::new(options)
    }
}

/// A bound loopback listener that has not yet been waited on.
pub struct CallbackListener {
    v4: Option<TcpListener>,
    v6: Option<TcpListener>,
    port: u16,
    host: String,
    redirect_host: String,
    path: String,
    state: String,
}

impl fmt::Debug for CallbackListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallbackListener")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("path", &self.path)
            .field("state", &"[redacted]")
            .finish()
    }
}

impl CallbackListener {
    /// Bind the listener. `state` is the value the callback must echo; use
    /// [`crate::pkce::generate_state`].
    pub fn bind(state: impl Into<String>, options: CallbackOptions) -> Result<Self, OAuthError> {
        Self::bind_with(state, CallbackBind::new(options))
    }

    /// [`Self::bind`] with the opt-in extras of [`CallbackBind`].
    pub fn bind_with(state: impl Into<String>, bind: CallbackBind) -> Result<Self, OAuthError> {
        let CallbackBind {
            options,
            redirect_host: requested_host,
            custom_ip,
        } = bind;
        let state = state.into();
        if state.len() < MIN_STATE_LEN || state.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
            return Err(OAuthError::InvalidConfig(format!(
                "callback state must be at least {MIN_STATE_LEN} visible ASCII characters"
            )));
        }
        let path = &options.path;
        if !path.starts_with('/')
            || path
                .bytes()
                .any(|b| b <= 0x20 || b == 0x7f || b == b'?' || b == b'#')
        {
            return Err(OAuthError::InvalidConfig(
                "callback path must start with '/' and hold no query, fragment or whitespace"
                    .into(),
            ));
        }
        let redirect_host = redirect_host_for(options.host, custom_ip, requested_host.as_deref())?;
        let bind_err = |what: &str, e: std::io::Error| {
            OAuthError::Callback(format!("bind {what} loopback listener: {e}"))
        };
        let (v4, v6) = match (custom_ip, options.host) {
            (Some(ip), _) => {
                let l = TcpListener::bind((ip, options.port)).map_err(|e| bind_err("custom", e))?;
                if ip.is_ipv4() {
                    (Some(l), None)
                } else {
                    (None, Some(l))
                }
            }
            (None, LoopbackHost::V4 | LoopbackHost::Both) => {
                let l = TcpListener::bind((Ipv4Addr::LOCALHOST, options.port))
                    .map_err(|e| bind_err("IPv4", e))?;
                let bound = l.local_addr().map_err(|e| bind_err("IPv4", e))?.port();
                // Best effort: a host without IPv6 loopback still serves IPv4.
                let v6 = if options.host == LoopbackHost::Both {
                    TcpListener::bind((Ipv6Addr::LOCALHOST, bound)).ok()
                } else {
                    None
                };
                (Some(l), v6)
            }
            (None, LoopbackHost::V6) => {
                let l = TcpListener::bind((Ipv6Addr::LOCALHOST, options.port))
                    .map_err(|e| bind_err("IPv6", e))?;
                (None, Some(l))
            }
        };
        let port = v4
            .iter()
            .chain(v6.iter())
            .next()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port())
            .ok_or_else(|| OAuthError::Callback("loopback listener has no address".into()))?;
        let (mut v4, mut v6) = (v4, v6);
        for l in v4.iter_mut().chain(v6.iter_mut()) {
            l.set_nonblocking(true)
                .map_err(|e| OAuthError::Callback(format!("configure loopback listener: {e}")))?;
        }
        Ok(Self {
            v4,
            v6,
            port,
            host: match custom_ip {
                Some(ip) => format!("custom {ip}"),
                None => format!("{:?}", options.host),
            },
            redirect_host,
            path: options.path,
            state,
        })
    }

    /// The port actually bound.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `redirect_uri` to register in the authorization request: the literal IP per RFC 8252
    /// unless [`CallbackBind::redirect_host`] chose another spelling.
    pub fn redirect_uri(&self) -> String {
        format!("http://{}:{}{}", self.redirect_host, self.port, self.path)
    }

    /// Serve until one valid callback arrives, `timeout` passes, or `cancel` fires.
    ///
    /// Returns the authorization code. A callback with the correct `state` and an `error`
    /// parameter ends the wait with [`OAuthError::AccessDenied`] (for `access_denied`) or
    /// [`OAuthError::Callback`]. The listener is consumed, so a second callback cannot be
    /// served, and its sockets close on return.
    pub fn wait(
        self,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<Secret, OAuthError> {
        check_timeout(timeout)?;
        let deadline = Instant::now() + timeout;
        let mut pending: Vec<Pending> = Vec::new();
        loop {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return Err(OAuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(OAuthError::Callback(
                    "timed out waiting for browser callback".into(),
                ));
            }
            let mut busy = self.accept_pending(&mut pending, Pending::new);
            let mut i = 0;
            while i < pending.len() {
                match pending[i].poll() {
                    Poll::Wait => i += 1,
                    Poll::Drop => {
                        busy = true;
                        pending.swap_remove(i);
                    }
                    Poll::Line(line) => {
                        busy = true;
                        let conn = pending.swap_remove(i);
                        if let Some(outcome) = self.handle(conn.stream, &line) {
                            return outcome;
                        }
                    }
                }
            }
            if !busy {
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }

    /// Answer one request line. `Some` ends the flow; `None` means the request was ignored.
    fn handle(&self, stream: TcpStream, line: &str) -> Option<Result<Secret, OAuthError>> {
        let mut parts = line.split_ascii_whitespace();
        let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
            respond(stream, "400 Bad Request", "Bad request.", &[]);
            return None;
        };
        match method {
            "GET" => {}
            "OPTIONS" => {
                // A CORS preflight (or a probe): answer without granting any cross-origin access.
                respond(stream, "204 No Content", "", &[("Allow", "GET, OPTIONS")]);
                return None;
            }
            _ => {
                respond(
                    stream,
                    "405 Method Not Allowed",
                    "Method not allowed.",
                    &[("Allow", "GET, OPTIONS")],
                );
                return None;
            }
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != self.path {
            // /favicon.ico, probes and anything else a browser or scanner asks for.
            respond(stream, "404 Not Found", "Not found.", &[]);
            return None;
        }
        let mut state = None;
        let mut code = None;
        let mut error = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let slot = match percent_decode(key).as_str() {
                "state" => &mut state,
                "code" => &mut code,
                "error" => &mut error,
                _ => continue,
            };
            // The first occurrence wins, so a later duplicate cannot override it.
            if slot.is_none() {
                *slot = Some(percent_decode(value));
            }
        }
        let state_ok = state
            .as_deref()
            .is_some_and(|s| constant_time_eq(s.as_bytes(), self.state.as_bytes()));
        if !state_ok {
            // Not our authorization response: ignore it and keep waiting.
            respond(stream, "400 Bad Request", "Invalid or missing state.", &[]);
            return None;
        }
        if let Some(error) = error {
            respond(stream, "200 OK", PAGE_NOT_COMPLETED, &[]);
            return Some(Err(if error == "access_denied" {
                OAuthError::AccessDenied
            } else {
                OAuthError::Callback(format!("provider returned error {}", sanitize(&error)))
            }));
        }
        match code.filter(|c| !c.is_empty()) {
            Some(code) => {
                respond(stream, "200 OK", PAGE_COMPLETE, &[]);
                Some(Ok(Secret::new(code)))
            }
            None => {
                respond(
                    stream,
                    "400 Bad Request",
                    "Missing authorization code.",
                    &[],
                );
                Some(Err(OAuthError::Callback(
                    "callback did not contain a code".into(),
                )))
            }
        }
    }

    /// Accept every connection waiting on the listeners into `pending` (capped at
    /// [`MAX_PENDING`]; extras are closed at once). Returns whether any arrived.
    fn accept_pending<P>(&self, pending: &mut Vec<P>, make: impl Fn(TcpStream) -> P) -> bool {
        let mut busy = false;
        for listener in self.v4.iter().chain(self.v6.iter()) {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        busy = true;
                        if pending.len() < MAX_PENDING && stream.set_nonblocking(true).is_ok() {
                            pending.push(make(stream));
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    // WouldBlock, or a transient per-connection accept failure.
                    Err(_) => break,
                }
            }
        }
        busy
    }

    /// Like [`Self::wait`], for a provider that POSTs its result as JSON from a web page
    /// (a CORS-preflighted `fetch`) instead of redirecting the browser with a GET. Returns the
    /// JSON object the page posted.
    ///
    /// `allowed_origin` is the page's exact origin (`https://provider.example`, scheme and
    /// host and optional port, no path, never `*`). The same ignore-and-continue rules hold:
    /// only a `POST` on the exact path from `allowed_origin`, whose JSON object body carries
    /// a string `state` equal to the listener's (constant-time compare), ends the flow.
    /// Everything else is answered and ignored: a wrong, missing or other `Origin` (403), a
    /// wrong or missing `state`, a body that is not a JSON object, an oversized body
    /// ([`MAX_POST_BODY`]), chunked or length-less requests, other methods (405), other paths
    /// (404), malformed and stalled connections.
    ///
    /// An `OPTIONS` preflight from `allowed_origin` is answered `204` with
    /// `Access-Control-Allow-Origin: <allowed_origin>`, `Access-Control-Allow-Methods`,
    /// `Access-Control-Allow-Headers` (the requested ones) and, when the browser asks for
    /// private-network access, `Access-Control-Allow-Private-Network: true`; a preflight from
    /// any other origin gets no CORS headers. The success reply carries the allow-origin
    /// header too, so the page can read it. The listener is consumed and closes on return.
    pub fn wait_post_json(
        self,
        timeout: Duration,
        cancel: Option<&CancelToken>,
        allowed_origin: &str,
    ) -> Result<Value, OAuthError> {
        check_timeout(timeout)?;
        check_origin(allowed_origin)?;
        let deadline = Instant::now() + timeout;
        let mut pending: Vec<HttpPending> = Vec::new();
        loop {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return Err(OAuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(OAuthError::Callback(
                    "timed out waiting for browser callback".into(),
                ));
            }
            let mut busy = self.accept_pending(&mut pending, HttpPending::new);
            let mut i = 0;
            while i < pending.len() {
                match pending[i].poll() {
                    HttpPoll::Wait => i += 1,
                    HttpPoll::Drop => {
                        busy = true;
                        pending.swap_remove(i);
                    }
                    HttpPoll::Reject(status, body) => {
                        busy = true;
                        let conn = pending.swap_remove(i);
                        respond(conn.stream, status, body, &[]);
                    }
                    HttpPoll::Request(request) => {
                        busy = true;
                        let conn = pending.swap_remove(i);
                        let outcome = self.handle_post(conn.stream, &request, allowed_origin);
                        if let Some(outcome) = outcome {
                            return outcome;
                        }
                    }
                }
            }
            if !busy {
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }

    /// Answer one POST-mode request. `Some` ends the flow; `None` means it was ignored.
    fn handle_post(
        &self,
        stream: TcpStream,
        request: &HttpRequest,
        allowed_origin: &str,
    ) -> Option<Result<Value, OAuthError>> {
        let (path, _) = request
            .target
            .split_once('?')
            .unwrap_or((request.target.as_str(), ""));
        if path != self.path {
            respond(stream, "404 Not Found", "Not found.", &[]);
            return None;
        }
        if !matches!(request.method.as_str(), "POST" | "OPTIONS") {
            respond(
                stream,
                "405 Method Not Allowed",
                "Method not allowed.",
                &[("Allow", "POST, OPTIONS")],
            );
            return None;
        }
        // Exact match: a browser sends the origin it serialized, never a variant. A missing
        // Origin is not a browser page and is refused too.
        if request.header("origin") != Some(allowed_origin) {
            respond(stream, "403 Forbidden", "Origin not allowed.", &[]);
            return None;
        }
        let cors = [
            ("Access-Control-Allow-Origin", allowed_origin),
            ("Vary", "Origin"),
        ];
        if request.method == "OPTIONS" {
            if request
                .header("access-control-request-method")
                .is_some_and(|m| !m.eq_ignore_ascii_case("POST"))
            {
                respond(
                    stream,
                    "405 Method Not Allowed",
                    "Method not allowed.",
                    &[("Allow", "POST, OPTIONS")],
                );
                return None;
            }
            let allow_headers = request
                .header("access-control-request-headers")
                .map(sanitize_header_list)
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| "Content-Type".into());
            let mut extra: Vec<(&str, &str)> = cors.to_vec();
            extra.push(("Access-Control-Allow-Methods", "POST, OPTIONS"));
            extra.push(("Access-Control-Allow-Headers", allow_headers.as_str()));
            extra.push(("Access-Control-Max-Age", "600"));
            if request
                .header("access-control-request-private-network")
                .is_some_and(|v| v.eq_ignore_ascii_case("true"))
            {
                extra.push(("Access-Control-Allow-Private-Network", "true"));
            }
            respond(stream, "204 No Content", "", &extra);
            return None;
        }
        let value: Value = match serde_json::from_slice(&request.body) {
            Ok(v @ Value::Object(_)) => v,
            // Never echo the body: it may hold a credential.
            _ => {
                respond(stream, "400 Bad Request", "Expected a JSON object.", &cors);
                return None;
            }
        };
        let state_ok = value
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|s| constant_time_eq(s.as_bytes(), self.state.as_bytes()));
        if !state_ok {
            respond(
                stream,
                "400 Bad Request",
                "Invalid or missing state.",
                &cors,
            );
            return None;
        }
        respond_typed(stream, "200 OK", "application/json", "{\"ok\":true}", &cors);
        Some(Ok(value))
    }
}

/// A provider-supplied error name reduced to a short safe token for error text.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '))
        .take(64)
        .collect()
}

fn check_timeout(timeout: Duration) -> Result<(), OAuthError> {
    if timeout.is_zero() || timeout > MAX_WAIT {
        return Err(OAuthError::InvalidConfig(
            "callback timeout must be greater than zero and at most one hour".into(),
        ));
    }
    Ok(())
}

/// `allowed_origin` must be a bare origin: `http(s)://host[:port]`, never `*`, `null`, a path
/// or anything a header value could be confused by.
fn check_origin(origin: &str) -> Result<(), OAuthError> {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or("");
    if rest.is_empty()
        || rest.bytes().any(|b| {
            b <= 0x20 || b == 0x7f || matches!(b, b'/' | b'?' | b'#' | b'@' | b'\\' | b'*')
        })
    {
        return Err(OAuthError::InvalidConfig(
            "allowed origin must be http(s)://host[:port] with no path".into(),
        ));
    }
    Ok(())
}

/// The host text `redirect_uri` carries. Loopback binds may only be spelled as loopback, so a
/// registration mismatch cannot turn into a redirect to some other host.
fn redirect_host_for(
    host: LoopbackHost,
    custom_ip: Option<IpAddr>,
    requested: Option<&str>,
) -> Result<String, OAuthError> {
    let invalid =
        |m: &str| OAuthError::InvalidConfig(format!("callback redirect host invalid: {m}"));
    match custom_ip {
        Some(ip) => {
            if ip.is_multicast() {
                return Err(invalid("a multicast address cannot be bound"));
            }
            match requested {
                Some(h) => {
                    let ok = !h.is_empty()
                        && h.len() <= 253
                        && h.bytes().all(|b| {
                            b.is_ascii_alphanumeric()
                                || matches!(b, b'-' | b'.' | b':' | b'[' | b']')
                        });
                    if ok {
                        Ok(h.to_string())
                    } else {
                        Err(invalid("expected a host name or IP literal"))
                    }
                }
                None if ip.is_unspecified() => Err(invalid(
                    "an unspecified bind address needs an explicit redirect_host",
                )),
                None => Ok(match ip {
                    IpAddr::V4(v4) => v4.to_string(),
                    IpAddr::V6(v6) => format!("[{v6}]"),
                }),
            }
        }
        None => match requested {
            None => Ok(match host {
                LoopbackHost::V6 => "[::1]",
                _ => "127.0.0.1",
            }
            .to_string()),
            Some(h @ ("localhost" | "127.0.0.1" | "[::1]")) => Ok(h.to_string()),
            Some(_) => Err(invalid(
                "a loopback listener's redirect host must be localhost, 127.0.0.1 or [::1]",
            )),
        },
    }
}

/// Request headers echoed back as `Access-Control-Allow-Headers`: header-name characters only.
fn sanitize_header_list(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ',' | ' '))
        .take(256)
        .collect()
}

/// One parsed POST-mode request (headers lower-cased by name).
struct HttpRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

enum HttpPoll {
    Wait,
    Drop,
    /// Answer with this status and body, then close; the flow continues.
    Reject(&'static str, &'static str),
    Request(HttpRequest),
}

struct HttpPending {
    stream: TcpStream,
    buf: Vec<u8>,
    deadline: Instant,
}

impl HttpPending {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            deadline: Instant::now() + CONNECTION_READ_BUDGET,
        }
    }

    /// Read what has arrived without blocking, then try to complete the request.
    fn poll(&mut self) -> HttpPoll {
        let mut chunk = [0u8; 1024];
        let mut eof = false;
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if self.buf.len() > HEADERS_MAX + MAX_POST_BODY + 4 {
                        return HttpPoll::Drop;
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return HttpPoll::Drop,
            }
        }
        match self.parse() {
            Some(done) => done,
            None if eof || Instant::now() >= self.deadline => HttpPoll::Drop,
            None => HttpPoll::Wait,
        }
    }

    /// `None` while the request is still incomplete.
    fn parse(&self) -> Option<HttpPoll> {
        let Some(head_end) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            return (self.buf.len() > HEADERS_MAX).then_some(HttpPoll::Reject(
                "431 Request Header Fields Too Large",
                "Headers too large.",
            ));
        };
        if head_end > HEADERS_MAX {
            return Some(HttpPoll::Reject(
                "431 Request Header Fields Too Large",
                "Headers too large.",
            ));
        }
        let head = String::from_utf8_lossy(&self.buf[..head_end]);
        let mut lines = head.split("\r\n");
        let mut first = lines.next().unwrap_or("").split_ascii_whitespace();
        let (Some(method), Some(target)) = (first.next(), first.next()) else {
            return Some(HttpPoll::Reject("400 Bad Request", "Bad request."));
        };
        let mut headers: Vec<(String, String)> = Vec::new();
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                return Some(HttpPoll::Reject("400 Bad Request", "Bad request."));
            };
            if headers.len() >= HEADER_LINES_MAX {
                return Some(HttpPoll::Reject(
                    "431 Request Header Fields Too Large",
                    "Too many headers.",
                ));
            }
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
        let mut request = HttpRequest {
            method: method.to_string(),
            target: target.to_string(),
            headers,
            body: Vec::new(),
        };
        if request.method == "POST" {
            // No chunked decoding: a callback body is a few hundred bytes with a length.
            if request.header("transfer-encoding").is_some() {
                return Some(HttpPoll::Reject(
                    "400 Bad Request",
                    "Chunked bodies are refused.",
                ));
            }
            let mut lengths = request
                .headers
                .iter()
                .filter(|(k, _)| k == "content-length")
                .map(|(_, v)| v.as_str());
            let (Some(raw), None) = (lengths.next(), lengths.next()) else {
                return Some(HttpPoll::Reject(
                    "411 Length Required",
                    "Exactly one Content-Length is required.",
                ));
            };
            let Ok(len) = raw.parse::<usize>() else {
                return Some(HttpPoll::Reject("400 Bad Request", "Bad Content-Length."));
            };
            if len > MAX_POST_BODY {
                return Some(HttpPoll::Reject("413 Payload Too Large", "Body too large."));
            }
            let start = head_end + 4;
            if self.buf.len() < start + len {
                return None;
            }
            request.body = self.buf[start..start + len].to_vec();
        }
        Some(HttpPoll::Request(request))
    }
}

enum Poll {
    Wait,
    Drop,
    Line(String),
}

struct Pending {
    stream: TcpStream,
    buf: Vec<u8>,
    deadline: Instant,
}

impl Pending {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            deadline: Instant::now() + CONNECTION_READ_BUDGET,
        }
    }

    /// Read what has arrived without blocking.
    fn poll(&mut self) -> Poll {
        let mut chunk = [0u8; 512];
        loop {
            match self.stream.read(&mut chunk) {
                // Closed before a full request line.
                Ok(0) => return Poll::Drop,
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = self.buf.iter().position(|b| *b == b'\n') {
                        let line = String::from_utf8_lossy(&self.buf[..end])
                            .trim_end_matches('\r')
                            .to_string();
                        return Poll::Line(line);
                    }
                    if self.buf.len() > REQUEST_LINE_MAX {
                        return Poll::Drop;
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    return if Instant::now() >= self.deadline {
                        Poll::Drop
                    } else {
                        Poll::Wait
                    };
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Poll::Drop,
            }
        }
    }
}

/// Write a short plain-text reply. The peer may already be gone (a prefetching browser); a
/// failed write never matters to the flow.
fn respond(stream: TcpStream, status: &str, body: &str, extra: &[(&str, &str)]) {
    respond_typed(stream, status, "text/plain; charset=utf-8", body, extra);
}

fn respond_typed(
    mut stream: TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    extra: &[(&str, &str)],
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body.as_bytes()));
    let _ = stream.shutdown(Shutdown::Write);
}

impl CallbackListener {
    /// Addresses the listener serves, for diagnostics and tests.
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.v4
            .iter()
            .chain(self.v6.iter())
            .filter_map(|l| l.local_addr().ok())
            .collect()
    }
}
