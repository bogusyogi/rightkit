//! A tolerant, single-use loopback redirect listener (RFC 8252 section 7.3).
//!
//! [`CallbackListener::bind`] listens on `127.0.0.1` and/or `[::1]` only, on an ephemeral
//! port unless the provider registration fixes one. [`CallbackListener::wait`] then serves
//! connections until exactly one request carries the expected `state` on the redirect path:
//! favicon fetches, CORS preflights, other methods and paths, malformed requests, stalled
//! connections and callbacks with a wrong or missing `state` are answered and ignored, so
//! none of them can end or poison the flow. `wait` consumes the listener, so it is single
//! use, and the sockets close when it returns.

use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use rightkit_http::{percent_decode, CancelToken, Secret};

use crate::pkce::constant_time_eq;
use crate::OAuthError;

/// Longest wait [`CallbackListener::wait`] accepts.
pub const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
/// Shortest `state` a listener accepts, so an empty or trivially guessable value cannot be set.
pub const MIN_STATE_LEN: usize = 16;
/// Longest request line read from one connection.
const REQUEST_LINE_MAX: usize = 8 * 1024;
/// How long one connection may take to deliver its request line.
const CONNECTION_READ_BUDGET: Duration = Duration::from_secs(5);
/// Most connections held open at once; further ones are closed at once.
const MAX_PENDING: usize = 16;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const PAGE_COMPLETE: &str = "Authorization complete. You may close this window.";
const PAGE_NOT_COMPLETED: &str = "Sign-in was not completed. You may close this window.";

/// Which loopback address the listener serves and `redirect_uri` names.
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

/// A bound loopback listener that has not yet been waited on.
pub struct CallbackListener {
    v4: Option<TcpListener>,
    v6: Option<TcpListener>,
    port: u16,
    host: LoopbackHost,
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
        let bind_err = |what: &str, e: std::io::Error| {
            OAuthError::Callback(format!("bind {what} loopback listener: {e}"))
        };
        let (v4, v6) = match options.host {
            LoopbackHost::V4 | LoopbackHost::Both => {
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
            LoopbackHost::V6 => {
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
            host: options.host,
            path: options.path,
            state,
        })
    }

    /// The port actually bound.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `redirect_uri` to register in the authorization request, literal IP per RFC 8252.
    pub fn redirect_uri(&self) -> String {
        let host = match self.host {
            LoopbackHost::V6 => "[::1]",
            _ => "127.0.0.1",
        };
        format!("http://{host}:{}{}", self.port, self.path)
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
        if timeout.is_zero() || timeout > MAX_WAIT {
            return Err(OAuthError::InvalidConfig(
                "callback timeout must be greater than zero and at most one hour".into(),
            ));
        }
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
            let mut busy = false;
            for listener in self.v4.iter().chain(self.v6.iter()) {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            busy = true;
                            if pending.len() < MAX_PENDING && stream.set_nonblocking(true).is_ok() {
                                pending.push(Pending::new(stream));
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        // WouldBlock, or a transient per-connection accept failure.
                        Err(_) => break,
                    }
                }
            }
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
}

/// A provider-supplied error name reduced to a short safe token for error text.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '))
        .take(64)
        .collect()
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
fn respond(mut stream: TcpStream, status: &str, body: &str, extra: &[(&str, &str)]) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
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
