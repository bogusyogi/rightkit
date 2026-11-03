//! A bounded HTTP/1.1 request parser and response writer, std only.
//!
//! The parser reads one request at a time from a connection and refuses, before any handler
//! runs, everything that is not a plain, small, unambiguous request:
//!
//! * the head (request line plus headers) is at most [`Limits::max_head_bytes`], the request
//!   line at most [`Limits::max_request_line`], and there are at most [`Limits::max_headers`]
//!   headers; the whole head must arrive within [`Limits::head_timeout`] of its first byte (a
//!   trickling client cannot hold a thread), and an idle keep-alive connection ends after
//!   [`Limits::idle_timeout`];
//! * only `HTTP/1.1`, only the methods in [`Method`], only origin-form targets (`/path?query`),
//!   no encoded dot, slash, backslash or NUL in the path, no `..` or `.` segment;
//! * no obsolete line folding, no bare CR or LF, only ASCII, no control bytes in a value;
//! * no `Transfer-Encoding` and no `Expect` at all (so no chunked-request smuggling), and a
//!   duplicated `Host`, `Content-Length`, `Content-Type`, `Authorization`, `Origin` or `Cookie`
//!   (plus any the caller adds) is refused rather than resolved;
//! * the body is exactly `Content-Length` bytes, at most [`Limits::max_body_bytes`], read within
//!   [`Limits::body_timeout`]; a request with no `Content-Length` has no body.
//!
//! Nothing here authenticates anything.

use crate::error::ServiceError;
use serde_json::Value;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

/// The methods the host accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Options,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
        }
    }

    pub fn parse(text: &str) -> Option<Method> {
        Some(match text {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "OPTIONS" => Method::Options,
            _ => return None,
        })
    }

    /// Methods that change state: the ones a cross-site page must not be able to trigger.
    pub fn is_unsafe(self) -> bool {
        matches!(
            self,
            Method::Post | Method::Put | Method::Patch | Method::Delete
        )
    }
}

/// Bounds on one connection and one request.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Longest request line (method, target, version). Default 8 KiB.
    pub max_request_line: usize,
    /// Longest head (request line and headers, without the final blank line). Default 16 KiB.
    pub max_head_bytes: usize,
    /// Most headers. Default 64.
    pub max_headers: usize,
    /// Largest body. Default 1 MiB.
    pub max_body_bytes: usize,
    /// Time from a request's first byte to the end of its head. Default 10 s.
    pub head_timeout: Duration,
    /// Time to read the body once the head is in. Default 30 s.
    pub body_timeout: Duration,
    /// How long a connection may wait for the next request. Default 30 s.
    pub idle_timeout: Duration,
    /// Per write on a response. Default 30 s.
    pub write_timeout: Duration,
    /// Header names (lowercase) that may appear at most once, in addition to the built-in list.
    pub singleton_headers: Vec<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_request_line: 8 * 1024,
            max_head_bytes: 16 * 1024,
            max_headers: 64,
            max_body_bytes: 1024 * 1024,
            head_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(30),
            singleton_headers: Vec::new(),
        }
    }
}

const BUILT_IN_SINGLETONS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "authorization",
    "origin",
    "cookie",
    "sec-fetch-site",
    "transfer-encoding",
    "expect",
];

/// Why a request was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The peer closed (or let an idle connection lapse) between requests. No response.
    Closed,
    /// The head or the body did not arrive in time. No response worth sending.
    TimedOut,
    /// The request is malformed; the text names the first problem found. 400.
    Malformed(&'static str),
    /// The head is over the size bound, or has too many headers. 431.
    HeadTooLarge,
    /// The request line is over the bound. 414.
    TargetTooLong,
    /// The body is over the bound. 413.
    BodyTooLarge,
    /// Only `HTTP/1.1` is spoken. 505.
    UnsupportedVersion,
    /// A method outside [`Method`]. 405.
    UnsupportedMethod,
    /// `Transfer-Encoding` (501) or `Expect` (417) was sent.
    UnsupportedFeature(&'static str),
    /// The read failed.
    Io(io::ErrorKind),
}

impl ParseError {
    /// The status to answer with, when an answer is worth sending.
    pub fn status(&self) -> Option<u16> {
        match self {
            ParseError::Closed | ParseError::TimedOut | ParseError::Io(_) => None,
            ParseError::Malformed(_) => Some(400),
            ParseError::HeadTooLarge => Some(431),
            ParseError::TargetTooLong => Some(414),
            ParseError::BodyTooLarge => Some(413),
            ParseError::UnsupportedVersion => Some(505),
            ParseError::UnsupportedMethod => Some(405),
            ParseError::UnsupportedFeature("expect") => Some(417),
            ParseError::UnsupportedFeature(_) => Some(501),
        }
    }

    pub fn message(&self) -> String {
        match self {
            ParseError::Closed => "connection closed".into(),
            ParseError::TimedOut => "request timed out".into(),
            ParseError::Malformed(why) => format!("malformed request: {why}"),
            ParseError::HeadTooLarge => "request head too large".into(),
            ParseError::TargetTooLong => "request target too long".into(),
            ParseError::BodyTooLarge => "request body too large".into(),
            ParseError::UnsupportedVersion => "only HTTP/1.1 is supported".into(),
            ParseError::UnsupportedMethod => "method not supported".into(),
            ParseError::UnsupportedFeature(what) => format!("{what} is not supported"),
            ParseError::Io(kind) => format!("read failed: {kind}"),
        }
    }
}

/// One parsed request. Header names are lowercase.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    /// The request target as sent (`/path?query`).
    pub target: String,
    /// The path, undecoded.
    pub path: String,
    /// The raw query string, without the `?`.
    pub query: Option<String>,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The client may send another request on this connection.
    pub keep_alive: bool,
    /// The remote address, filled in by the host.
    pub peer: Option<std::net::SocketAddr>,
}

impl Request {
    /// The first value of header `name` (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        let wanted = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == wanted)
            .map(|(_, v)| v.as_str())
    }

    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// The query as decoded `name=value` pairs (`+` is a space, `%XX` is decoded).
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        let Some(query) = &self.query else {
            return Vec::new();
        };
        query
            .split('&')
            .filter(|part| !part.is_empty())
            .map(|part| match part.split_once('=') {
                Some((k, v)) => (percent_decode(k), percent_decode(v)),
                None => (percent_decode(part), String::new()),
            })
            .collect()
    }
}

/// Decode `%XX` and `+`. Invalid escapes are kept as they are.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len()
                && hex_val(bytes[i + 1]).is_some()
                && hex_val(bytes[i + 2]).is_some() =>
            {
                let high = hex_val(bytes[i + 1]).unwrap_or(0);
                let low = hex_val(bytes[i + 2]).unwrap_or(0);
                out.push(high * 16 + low);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A byte source whose read timeout can be set (a `TcpStream`, or a script in tests).
pub trait TimedRead: Read {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl TimedRead for std::net::TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        std::net::TcpStream::set_read_timeout(self, timeout)
    }
}

/// Reads requests one after another from a connection, keeping what a read brought beyond the
/// current request (a pipelined next request) for the next call.
pub struct RequestReader<R> {
    stream: R,
    buf: Vec<u8>,
}

fn find_head_end(buf: &[u8], from: usize) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    (from..=buf.len() - 4).find(|&i| &buf[i..i + 4] == b"\r\n\r\n")
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

impl<R: TimedRead> RequestReader<R> {
    pub fn new(stream: R) -> Self {
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    /// The underlying stream (to write a response, or to shut it down).
    pub fn get_ref(&self) -> &R {
        &self.stream
    }

    /// Read and validate the next request.
    pub fn read_request(&mut self, limits: &Limits) -> Result<Request, ParseError> {
        let head_end = self.read_head(limits)?;
        let head: Vec<u8> = self.buf[..head_end].to_vec();
        self.buf.drain(..head_end + 4);
        let parsed = parse_head(&head, limits)?;
        let body = self.read_body(parsed.content_length, limits)?;
        Ok(Request {
            method: parsed.method,
            target: parsed.target,
            path: parsed.path,
            query: parsed.query,
            headers: parsed.headers,
            body,
            keep_alive: parsed.keep_alive,
            peer: None,
        })
    }

    fn read_head(&mut self, limits: &Limits) -> Result<usize, ParseError> {
        let mut started: Option<Instant> = if self.buf.is_empty() {
            None
        } else {
            Some(Instant::now())
        };
        let mut scanned = 0usize;
        loop {
            if let Some(end) = find_head_end(&self.buf, scanned.saturating_sub(3)) {
                if end > limits.max_head_bytes {
                    return Err(ParseError::HeadTooLarge);
                }
                return Ok(end);
            }
            scanned = self.buf.len();
            if self.buf.len() > limits.max_head_bytes + 3 {
                return Err(ParseError::HeadTooLarge);
            }
            let timeout = match started {
                None => limits.idle_timeout,
                Some(first) => {
                    let left = limits.head_timeout.saturating_sub(first.elapsed());
                    if left.is_zero() {
                        return Err(ParseError::TimedOut);
                    }
                    left
                }
            };
            self.stream
                .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
                .map_err(|e| ParseError::Io(e.kind()))?;
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(ParseError::Closed),
                Ok(n) => {
                    if started.is_none() {
                        started = Some(Instant::now());
                    }
                    self.buf.extend_from_slice(&chunk[..n]);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(if started.is_none() {
                        ParseError::Closed
                    } else {
                        ParseError::TimedOut
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(ParseError::Io(e.kind())),
            }
        }
    }

    fn read_body(&mut self, length: usize, limits: &Limits) -> Result<Vec<u8>, ParseError> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        while self.buf.len() < length {
            let left = limits.body_timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(ParseError::TimedOut);
            }
            self.stream
                .set_read_timeout(Some(left.max(Duration::from_millis(1))))
                .map_err(|e| ParseError::Io(e.kind()))?;
            let want = (length - self.buf.len()).min(64 * 1024);
            let mut chunk = vec![0u8; want];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(ParseError::Closed),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(ParseError::TimedOut);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(ParseError::Io(e.kind())),
            }
        }
        let rest = self.buf.split_off(length);
        Ok(std::mem::replace(&mut self.buf, rest))
    }
}

struct ParsedHead {
    method: Method,
    target: String,
    path: String,
    query: Option<String>,
    headers: Vec<(String, String)>,
    content_length: usize,
    keep_alive: bool,
}

fn validate_target(target: &str, limits: &Limits) -> Result<(String, Option<String>), ParseError> {
    if target.len() > limits.max_request_line {
        return Err(ParseError::TargetTooLong);
    }
    if !target.starts_with('/') || target.starts_with("//") {
        return Err(ParseError::Malformed("the target must be an absolute path"));
    }
    if target
        .bytes()
        .any(|b| b < 0x21 || b == 0x7f || b == b'#' || b == b'\\')
    {
        return Err(ParseError::Malformed("forbidden byte in the target"));
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q.to_string())),
        None => (target, None),
    };
    let lowered = path.to_ascii_lowercase();
    if ["%2e", "%2f", "%5c", "%00"]
        .iter()
        .any(|bad| lowered.contains(bad))
    {
        return Err(ParseError::Malformed(
            "encoded dot, separator or NUL in the path",
        ));
    }
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(ParseError::Malformed("dot segment in the path"));
    }
    Ok((path.to_string(), query))
}

fn parse_head(head: &[u8], limits: &Limits) -> Result<ParsedHead, ParseError> {
    if !head.is_ascii() {
        return Err(ParseError::Malformed("non-ASCII byte in the request head"));
    }
    let text =
        std::str::from_utf8(head).map_err(|_| ParseError::Malformed("invalid request head"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    if request_line.len() > limits.max_request_line {
        return Err(ParseError::TargetTooLong);
    }
    if request_line.contains(['\r', '\n']) {
        return Err(ParseError::Malformed("bare CR or LF in the request line"));
    }
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ParseError::Malformed(
            "the request line must be METHOD TARGET VERSION",
        ));
    };
    if method.is_empty() || target.is_empty() || version.is_empty() {
        return Err(ParseError::Malformed("empty part in the request line"));
    }
    if version != "HTTP/1.1" {
        return Err(ParseError::UnsupportedVersion);
    }
    let method = Method::parse(method).ok_or(ParseError::UnsupportedMethod)?;
    let (path, query) = validate_target(target, limits)?;

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if headers.len() >= limits.max_headers {
            return Err(ParseError::HeadTooLarge);
        }
        if line.starts_with([' ', '\t']) {
            return Err(ParseError::Malformed("obsolete line folding"));
        }
        if line.contains(['\r', '\n']) {
            return Err(ParseError::Malformed("bare CR or LF in a header"));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ParseError::Malformed("a header line has no colon"));
        };
        if !is_token(name) {
            return Err(ParseError::Malformed("invalid header name"));
        }
        let value = value.trim_matches([' ', '\t']);
        if value
            .bytes()
            .any(|b| !(b == b'\t' || (0x20..0x7f).contains(&b)))
        {
            return Err(ParseError::Malformed("control byte in a header value"));
        }
        headers.push((name.to_ascii_lowercase(), value.to_string()));
    }

    for singleton in BUILT_IN_SINGLETONS
        .iter()
        .copied()
        .chain(limits.singleton_headers.iter().map(String::as_str))
    {
        if headers.iter().filter(|(n, _)| n == singleton).count() > 1 {
            return Err(ParseError::Malformed(
                "a header that may appear once was repeated",
            ));
        }
    }
    let first = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    if first("transfer-encoding").is_some() {
        return Err(ParseError::UnsupportedFeature("transfer-encoding"));
    }
    if first("expect").is_some() {
        return Err(ParseError::UnsupportedFeature("expect"));
    }
    if first("host").is_none() {
        return Err(ParseError::Malformed("no Host header"));
    }
    let content_length = match first("content-length") {
        None => 0,
        Some(value) => {
            if value.is_empty() || value.len() > 19 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ParseError::Malformed("invalid Content-Length"));
            }
            let parsed: u64 = value
                .parse()
                .map_err(|_| ParseError::Malformed("invalid Content-Length"))?;
            if parsed > limits.max_body_bytes as u64 {
                return Err(ParseError::BodyTooLarge);
            }
            parsed as usize
        }
    };
    let keep_alive = !first("connection").is_some_and(|value| {
        value
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case("close"))
    });
    Ok(ParsedHead {
        method,
        target: target.to_string(),
        path,
        query,
        headers,
        content_length,
        keep_alive,
    })
}

// ---------------------------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------------------------

/// Where a streamed body (server-sent events) is written. Each `send` is one chunk.
pub trait StreamSink {
    fn send(&mut self, chunk: &[u8]) -> io::Result<()>;
    /// The host is shutting down: end the stream.
    fn stopping(&self) -> bool;
}

/// Produces a streamed body on the connection's thread; returns when the stream ends.
pub type StreamFn = Box<dyn FnOnce(&mut dyn StreamSink) + Send>;

/// A response body.
pub enum Body {
    Empty,
    Bytes(Vec<u8>),
    /// Chunked, until the closure returns. The connection closes afterwards.
    Stream(StreamFn),
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Body::Empty => f.write_str("Body::Empty"),
            Body::Bytes(b) => write!(f, "Body::Bytes({} bytes)", b.len()),
            Body::Stream(_) => f.write_str("Body::Stream"),
        }
    }
}

/// A response. Headers the host sets itself (`content-length`, `transfer-encoding`,
/// `connection`) are ignored if given here.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Response {
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    pub fn text(status: u16, text: &str) -> Self {
        Self {
            status,
            headers: vec![(
                "content-type".to_string(),
                "text/plain; charset=utf-8".to_string(),
            )],
            body: Body::Bytes(text.as_bytes().to_vec()),
        }
    }

    pub fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: Body::Bytes(value.to_string().into_bytes()),
        }
    }

    /// `{"error":{"code","message","retryable"}}` with the status the error code maps to.
    pub fn from_error(error: &ServiceError) -> Self {
        Self::json(
            crate::contract::status_for_error(error),
            &serde_json::json!({ "error": error.to_value() }),
        )
    }

    /// A server-sent-event stream: `text/event-stream`, no buffering, chunked.
    pub fn event_stream(stream: StreamFn) -> Self {
        Self {
            status: 200,
            headers: vec![
                ("content-type".to_string(), "text/event-stream".to_string()),
                ("x-accel-buffering".to_string(), "no".to_string()),
            ],
            body: Body::Stream(stream),
        }
    }

    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers
            .push((name.to_ascii_lowercase(), value.to_string()));
        self
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        303 => "See Other",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        417 => "Expectation Failed",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        423 => "Locked",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        _ => "Status",
    }
}

fn header_is_safe(name: &str, value: &str) -> bool {
    is_token(name)
        && value
            .bytes()
            .all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
}

const HOST_OWNED_HEADERS: &[&str] = &["content-length", "transfer-encoding", "connection"];

/// Write `response`. A response whose headers hold a byte that could split it (CR, LF, NUL,
/// another control byte) is replaced by a plain 500: nothing the app supplied reaches the wire
/// unvalidated.
///
/// Returns whether the connection may stay open (false after a stream, a `Connection: close`
/// request, or a write error).
pub fn write_response<W: Write>(
    out: &mut W,
    response: Response,
    keep_alive: bool,
    stopping: &dyn Fn() -> bool,
) -> io::Result<bool> {
    let response = if response.headers.iter().all(|(n, v)| header_is_safe(n, v)) {
        response
    } else {
        Response::text(500, "internal error")
    };
    let Response {
        status,
        headers,
        body,
    } = response;
    let streaming = matches!(body, Body::Stream(_));
    let keep_alive = keep_alive && !streaming;
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    let provided = |name: &str| headers.iter().any(|(n, _)| n == name);
    for (name, value) in &headers {
        if HOST_OWNED_HEADERS.contains(&name.as_str()) {
            continue;
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
    ] {
        if !provided(name) {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
    }
    head.push_str(if keep_alive {
        "connection: keep-alive\r\n"
    } else {
        "connection: close\r\n"
    });
    match body {
        Body::Empty => {
            if status != 204 && status != 304 {
                head.push_str("content-length: 0\r\n");
            }
            head.push_str("\r\n");
            out.write_all(head.as_bytes())?;
        }
        Body::Bytes(bytes) => {
            if status == 204 || status == 304 {
                head.push_str("\r\n");
                out.write_all(head.as_bytes())?;
            } else {
                head.push_str(&format!("content-length: {}\r\n\r\n", bytes.len()));
                out.write_all(head.as_bytes())?;
                out.write_all(&bytes)?;
            }
        }
        Body::Stream(stream) => {
            head.push_str("transfer-encoding: chunked\r\n\r\n");
            out.write_all(head.as_bytes())?;
            out.flush()?;
            let mut sink = ChunkSink {
                out: &mut *out,
                stopping,
            };
            stream(&mut sink);
            out.write_all(b"0\r\n\r\n")?;
        }
    }
    out.flush()?;
    Ok(keep_alive)
}

struct ChunkSink<'a, W: Write> {
    out: &'a mut W,
    stopping: &'a dyn Fn() -> bool,
}

impl<W: Write> StreamSink for ChunkSink<'_, W> {
    fn send(&mut self, chunk: &[u8]) -> io::Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        write!(self.out, "{:x}\r\n", chunk.len())?;
        self.out.write_all(chunk)?;
        self.out.write_all(b"\r\n")?;
        self.out.flush()
    }

    fn stopping(&self) -> bool {
        (self.stopping)()
    }
}
