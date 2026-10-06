use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{HttpError, MAX_ERROR_BODY_BYTES};
use crate::retry::{parse_retry_after, RetryPolicy};
use crate::sse::SseReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
        }
    }
    /// RFC 9110 idempotent methods (safe to retry after an ambiguous failure).
    pub fn is_idempotent(self) -> bool {
        !matches!(self, Method::Post | Method::Patch)
    }
}

/// One retry decision, reported to [`ClientConfig::on_retry`].
#[derive(Debug, Clone)]
pub struct RetryEvent {
    pub url: String,
    /// Retry number about to run (1 = first retry).
    pub retry: u32,
    pub reason: String,
    pub delay: Duration,
}

type RetryObserver = Arc<dyn Fn(&RetryEvent) + Send + Sync>;

#[derive(Clone)]
pub struct ClientConfig {
    pub user_agent: String,
    pub connect_timeout: Duration,
    /// Whole-request deadline for buffered calls (headers + body).
    pub request_timeout: Duration,
    /// Time to receive response headers; for streams this is the first-byte
    /// (first-token) deadline.
    pub response_timeout: Duration,
    /// Optional total lifetime cap for a stream body.
    pub stream_timeout: Option<Duration>,
    /// Redirects followed (0 = never; credentials are never replayed to a new host).
    pub max_redirects: u32,
    pub max_response_bytes: usize,
    pub retry: RetryPolicy,
    pub on_retry: Option<RetryObserver>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            user_agent: concat!("rightkit-http/", env!("CARGO_PKG_VERSION")).to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            response_timeout: Duration::from_secs(60),
            stream_timeout: None,
            max_redirects: 0,
            max_response_bytes: 32 * 1024 * 1024,
            retry: RetryPolicy::default(),
            on_retry: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub timeout: Option<Duration>,
    pub retry: Option<RetryPolicy>,
    /// Allow retrying a POST/PATCH after HTTP-status failures (default true for
    /// status codes; transport errors on non-idempotent methods only retry in
    /// the connect phase regardless).
    pub retry_unsafe_statuses: bool,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: None,
            retry: None,
            retry_unsafe_statuses: true,
        }
    }
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::Get, url)
    }
    pub fn post(url: impl Into<String>) -> Self {
        Self::new(Method::Post, url)
    }
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    pub fn bearer(self, token: &str) -> Self {
        self.header("Authorization", format!("Bearer {token}"))
    }
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
    pub fn json<T: Serialize>(mut self, value: &T) -> Result<Self, HttpError> {
        self.body = serde_json::to_vec(value).map_err(|e| HttpError::Json(e.to_string()))?;
        self.headers.push(("Content-Type".into(), "application/json".into()));
        Ok(self)
    }
    /// `application/x-www-form-urlencoded` body.
    pub fn form(mut self, pairs: &[(&str, &str)]) -> Self {
        self.body = form_encode(pairs).into_bytes();
        self.headers.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        self
    }
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }
}

/// Percent-encode for `application/x-www-form-urlencoded` / URL query values.
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    /// Lower-cased header names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub attempts: u32,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        header_of(&self.headers, name)
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, HttpError> {
        serde_json::from_slice(&self.body).map_err(|e| HttpError::Json(e.to_string()))
    }
}

pub struct StreamResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub attempts: u32,
    reader: Box<dyn Read + Send>,
}

impl StreamResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        header_of(&self.headers, name)
    }
    pub fn into_reader(self) -> Box<dyn Read + Send> {
        self.reader
    }
    /// Decode the body as Server-Sent Events.
    pub fn sse(self) -> SseReader<Box<dyn Read + Send>> {
        SseReader::new(self.reader)
    }
}

fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    config: ClientConfig,
}

struct Raw {
    status: u16,
    headers: Vec<(String, String)>,
    body: ureq::Body,
    attempts: u32,
}

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(config.max_redirects)
            .max_redirects_will_error(false)
            .user_agent(config.user_agent.clone())
            .timeout_connect(Some(config.connect_timeout))
            .build()
            .into();
        Self { agent, config }
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Buffered request; non-2xx becomes [`HttpError::Status`].
    pub fn send(&self, req: &Request) -> Result<Response, HttpError> {
        let resp = self.send_any(req)?;
        if (200..300).contains(&resp.status) {
            Ok(resp)
        } else {
            Err(status_error(resp.status, &resp.headers, resp.body, resp.attempts))
        }
    }

    /// Buffered request returning any final status (retryable statuses are still retried).
    pub fn send_any(&self, req: &Request) -> Result<Response, HttpError> {
        let timeout = req.timeout.unwrap_or(self.config.request_timeout);
        let mut raw = self.execute(req, Some(timeout))?;
        let body = read_limited(&mut raw.body.as_reader(), self.config.max_response_bytes)?;
        Ok(Response { status: raw.status, headers: raw.headers, body, attempts: raw.attempts })
    }

    /// Streaming request; non-2xx becomes [`HttpError::Status`]. Retries happen
    /// only before the response headers arrive (never mid-stream).
    pub fn stream(&self, req: &Request) -> Result<StreamResponse, HttpError> {
        let mut raw = self.execute(req, self.config.stream_timeout)?;
        if !(200..300).contains(&raw.status) {
            let body = read_limited(&mut raw.body.as_reader(), MAX_ERROR_BODY_BYTES).unwrap_or_default();
            return Err(status_error(raw.status, &raw.headers, body, raw.attempts));
        }
        Ok(StreamResponse {
            status: raw.status,
            headers: raw.headers,
            attempts: raw.attempts,
            reader: Box::new(raw.body.into_reader()),
        })
    }

    pub fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, HttpError> {
        self.send(&Request::get(url).header("Accept", "application/json"))?.json()
    }

    fn execute(&self, req: &Request, total: Option<Duration>) -> Result<Raw, HttpError> {
        let policy = req.retry.clone().unwrap_or_else(|| self.config.retry.clone());
        let max = policy.max_attempts.max(1);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let outcome = self.once(req, total);
            let (reason, retry_after) = match outcome {
                Ok(raw) => {
                    let retryable = policy.is_retryable_status(raw.status)
                        && (req.method.is_idempotent() || req.retry_unsafe_statuses);
                    if !retryable || attempt >= max {
                        return Ok(Raw { attempts: attempt, ..raw });
                    }
                    let ra = header_of(&raw.headers, "retry-after").and_then(parse_retry_after);
                    // Drain so the connection is released.
                    let mut body = raw.body;
                    let _ = read_limited(&mut body.as_reader(), MAX_ERROR_BODY_BYTES);
                    (format!("HTTP {}", raw.status), ra)
                }
                Err(err) => {
                    let retryable = match &err {
                        HttpError::Transport { connect_phase, .. } => {
                            *connect_phase || req.method.is_idempotent()
                        }
                        HttpError::Timeout(_) => req.method.is_idempotent(),
                        _ => false,
                    };
                    if !retryable || attempt >= max {
                        return Err(err);
                    }
                    (err.to_string(), None)
                }
            };
            let delay = policy.delay(attempt, retry_after);
            if let Some(obs) = &self.config.on_retry {
                obs(&RetryEvent { url: req.url.clone(), retry: attempt, reason, delay });
            }
            std::thread::sleep(delay);
        }
    }

    fn once(&self, req: &Request, total: Option<Duration>) -> Result<Raw, HttpError> {
        let mut builder = ureq::http::Request::builder()
            .method(req.method.as_str())
            .uri(&req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let request = builder
            .body(req.body.clone())
            .map_err(|e| HttpError::InvalidRequest(e.to_string()))?;
        let configured = self
            .agent
            .configure_request(request)
            .timeout_global(total)
            .timeout_recv_response(Some(self.config.response_timeout))
            .build();
        let response = self.agent.run(configured).map_err(map_ureq_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_ascii_lowercase(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        Ok(Raw { status, headers, body: response.into_body(), attempts: 1 })
    }
}

fn status_error(status: u16, headers: &[(String, String)], body: Vec<u8>, attempts: u32) -> HttpError {
    let mut body = body;
    body.truncate(MAX_ERROR_BODY_BYTES);
    HttpError::Status {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
        retry_after: header_of(headers, "retry-after").and_then(parse_retry_after),
        attempts,
    }
}

fn read_limited(reader: &mut dyn Read, limit: usize) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => HttpError::Timeout(e.to_string()),
            _ => HttpError::Transport { message: e.to_string(), connect_phase: false },
        })?;
    if out.len() > limit {
        return Err(HttpError::BodyTooLarge { limit });
    }
    Ok(out)
}

fn map_ureq_error(err: ureq::Error) -> HttpError {
    use ureq::Error as E;
    match err {
        E::Timeout(t) => HttpError::Timeout(format!("{t:?}")),
        E::HostNotFound | E::ConnectionFailed => {
            HttpError::Transport { message: err.to_string(), connect_phase: true }
        }
        E::Io(ref io) if matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
        ) => HttpError::Transport { message: err.to_string(), connect_phase: true },
        E::BadUri(m) => HttpError::InvalidRequest(m),
        E::Http(e) => HttpError::InvalidRequest(e.to_string()),
        other => HttpError::Transport { message: other.to_string(), connect_phase: false },
    }
}
