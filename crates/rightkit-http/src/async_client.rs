//! Async HTTP client (feature `async`) on reqwest + tokio with rustls only.
//!
//! It applies the same policy layer as the blocking [`crate::Client`], driven by
//! the same [`ClientConfig`], [`Request`], [`Response`] and [`HttpError`] types:
//! per-phase timeouts, retry/backoff with `Retry-After`, retry-safety for
//! non-idempotent methods, redirect limits (credentials are never replayed to a
//! new host), user agent, bounded bodies and the error taxonomy. Proxies come
//! from the environment (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`),
//! as with the blocking client. Calls must run inside a tokio runtime.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use tokio::time::{sleep, timeout, Sleep};

use crate::client::{header_of, status_error, ClientConfig, Method, Request, Response, RetryEvent};
use crate::error::{HttpError, MAX_ERROR_BODY_BYTES};
use crate::retry::parse_retry_after;
use crate::sse::{SseDecoder, SseEvent};

/// Default per-line bound for [`AsyncStreamResponse::sse`].
pub const DEFAULT_SSE_MAX_LINE_BYTES: usize = 1024 * 1024;
/// Default per-event bound for [`AsyncStreamResponse::sse`].
pub const DEFAULT_SSE_MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// Async counterpart of [`crate::Client`].
#[derive(Clone)]
pub struct AsyncClient {
    http: reqwest::Client,
    config: ClientConfig,
}

struct Raw {
    status: u16,
    headers: Vec<(String, String)>,
    response: reqwest::Response,
    attempts: u32,
    /// Whole-attempt deadline (buffered: request timeout, streams: stream timeout).
    deadline: Option<Instant>,
}

impl AsyncClient {
    pub fn new(config: ClientConfig) -> Result<Self, HttpError> {
        let max = config.max_redirects as usize;
        let will_error = config.max_redirects_will_error;
        let redirect = if max == 0 {
            reqwest::redirect::Policy::none()
        } else {
            reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > max {
                    if will_error {
                        attempt.error("too many redirects")
                    } else {
                        attempt.stop()
                    }
                } else {
                    attempt.follow()
                }
            })
        };
        let mut builder = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .redirect(redirect);
        if let Some(d) = config.connect_timeout {
            builder = builder.connect_timeout(d);
        }
        let http = builder.build().map_err(map_reqwest_error)?;
        Ok(Self { http, config })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Buffered request; non-2xx becomes [`HttpError::Status`].
    pub async fn send(&self, req: &Request) -> Result<Response, HttpError> {
        let resp = self.send_any(req).await?;
        if (200..300).contains(&resp.status) {
            Ok(resp)
        } else {
            Err(status_error(
                resp.status,
                &resp.headers,
                resp.body,
                resp.attempts,
            ))
        }
    }

    /// Buffered request returning any final status (retryable statuses are still retried).
    pub async fn send_any(&self, req: &Request) -> Result<Response, HttpError> {
        let total = req.timeout.unwrap_or(self.config.request_timeout);
        let raw = self.execute(req, Some(total)).await?;
        let deadline = self.body_deadline(raw.deadline);
        let body = read_limited(raw.response, self.config.max_response_bytes, deadline).await?;
        Ok(Response {
            status: raw.status,
            headers: raw.headers,
            body,
            attempts: raw.attempts,
        })
    }

    /// Streaming request; non-2xx becomes [`HttpError::Status`]. Retries happen
    /// only before the response headers arrive (never mid-stream).
    pub async fn stream(&self, req: &Request) -> Result<AsyncStreamResponse, HttpError> {
        let raw = self.execute(req, self.config.stream_timeout).await?;
        if !(200..300).contains(&raw.status) {
            let body = read_limited(
                raw.response,
                MAX_ERROR_BODY_BYTES,
                self.body_deadline(raw.deadline),
            )
            .await
            .unwrap_or_default();
            return Err(status_error(raw.status, &raw.headers, body, raw.attempts));
        }
        let deadline = self.body_deadline(raw.deadline);
        Ok(AsyncStreamResponse {
            status: raw.status,
            headers: raw.headers,
            attempts: raw.attempts,
            body: BodyStream::new(Box::pin(raw.response.bytes_stream()), deadline),
        })
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, HttpError> {
        self.send(&Request::get(url).header("Accept", "application/json"))
            .await?
            .json()
    }

    /// Phase deadline for the body: the sooner of the attempt deadline and
    /// `recv_body_timeout` counted from now (headers just arrived).
    fn body_deadline(&self, attempt: Option<Instant>) -> Option<Instant> {
        let phase = self.config.recv_body_timeout.map(|d| Instant::now() + d);
        match (attempt, phase) {
            (Some(a), Some(p)) => Some(a.min(p)),
            (a, p) => a.or(p),
        }
    }

    async fn execute(&self, req: &Request, total: Option<Duration>) -> Result<Raw, HttpError> {
        let policy = req
            .retry
            .clone()
            .unwrap_or_else(|| self.config.retry.clone());
        let max = policy.max_attempts.max(1);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let outcome = self.once(req, total).await;
            let (reason, retry_after) = match outcome {
                Ok(raw) => {
                    let retryable = policy.is_retryable_status(raw.status)
                        && (req.method.is_idempotent() || req.retry_unsafe_statuses);
                    if !retryable || attempt >= max {
                        return Ok(Raw {
                            attempts: attempt,
                            ..raw
                        });
                    }
                    let ra = header_of(&raw.headers, "retry-after").and_then(parse_retry_after);
                    // Drain so the connection is released.
                    let drain = Some(Instant::now() + Duration::from_secs(5));
                    let _ = read_limited(raw.response, MAX_ERROR_BODY_BYTES, drain).await;
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
                obs(&RetryEvent {
                    url: req.url.clone(),
                    retry: attempt,
                    reason,
                    delay,
                });
            }
            sleep(delay).await;
        }
    }

    async fn once(&self, req: &Request, total: Option<Duration>) -> Result<Raw, HttpError> {
        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
            Method::Head => reqwest::Method::HEAD,
        };
        let mut builder = self.http.request(method, req.url.as_str());
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        if !req.body.is_empty() || matches!(req.method, Method::Post | Method::Put | Method::Patch)
        {
            builder = builder.body(req.body.clone());
        }
        let request = builder.build().map_err(map_reqwest_error)?;

        let started = Instant::now();
        let deadline = total.map(|t| started + t);
        let headers_budget = match (self.config.response_timeout, total) {
            (Some(r), Some(t)) => Some(r.min(t)),
            (r, t) => r.or(t),
        };
        let send = self.http.execute(request);
        let response = match headers_budget {
            Some(d) => timeout(d, send)
                .await
                .map_err(|_| HttpError::Timeout("response headers".into()))?,
            None => send.await,
        }
        .map_err(map_reqwest_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        Ok(Raw {
            status,
            headers,
            response,
            attempts: 1,
            deadline,
        })
    }
}

/// Streaming response from [`AsyncClient::stream`].
pub struct AsyncStreamResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub attempts: u32,
    body: BodyStream,
}

impl AsyncStreamResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        header_of(&self.headers, name)
    }

    /// Raw body chunks. Errors are typed [`HttpError`]s; the stream ends after
    /// the first error.
    pub fn into_stream(self) -> BodyStream {
        self.body
    }

    /// Decode the body as Server-Sent Events with the default bounds
    /// (1 MiB per line, 4 MiB per event).
    pub fn sse(self) -> SseStream {
        self.sse_with_limits(DEFAULT_SSE_MAX_LINE_BYTES, DEFAULT_SSE_MAX_EVENT_BYTES)
    }

    pub fn sse_with_limits(self, max_line_bytes: usize, max_event_bytes: usize) -> SseStream {
        SseStream::new(
            self.body,
            SseDecoder::with_limits(max_line_bytes, max_event_bytes),
        )
    }
}

/// Body chunks with the stream/body deadline applied and errors typed.
pub struct BodyStream {
    inner: ByteStream,
    deadline: Option<Pin<Box<Sleep>>>,
    done: bool,
}

impl BodyStream {
    fn new(inner: ByteStream, deadline: Option<Instant>) -> Self {
        let deadline = deadline.map(|d| {
            Box::pin(tokio::time::sleep(
                d.saturating_duration_since(Instant::now()),
            ))
        });
        Self {
            inner,
            deadline,
            done: false,
        }
    }
}

impl Stream for BodyStream {
    type Item = Result<Bytes, HttpError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        if let Some(sleep) = self.deadline.as_mut() {
            if sleep.as_mut().poll(cx).is_ready() {
                self.done = true;
                return Poll::Ready(Some(Err(HttpError::Timeout("response body".into()))));
            }
        }
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(Ok(b))),
            Poll::Ready(Some(Err(e))) => {
                self.done = true;
                Poll::Ready(Some(Err(map_reqwest_error(e))))
            }
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Async stream of decoded events. An event cut off by EOF is dropped, per the
/// EventSource spec; the stream ends after the first error.
pub struct SseStream {
    body: BodyStream,
    decoder: SseDecoder,
    queue: VecDeque<SseEvent>,
    done: bool,
}

impl SseStream {
    fn new(body: BodyStream, decoder: SseDecoder) -> Self {
        Self {
            body,
            decoder,
            queue: VecDeque::new(),
            done: false,
        }
    }
}

impl Stream for SseStream {
    type Item = Result<SseEvent, HttpError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Poll::Ready(Some(Ok(ev)));
            }
            if self.done {
                return Poll::Ready(None);
            }
            match Pin::new(&mut self.body).poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => match self.decoder.push(&chunk) {
                    Ok(events) => self.queue.extend(events),
                    Err(e) => {
                        self.done = true;
                        return Poll::Ready(Some(Err(e)));
                    }
                },
                Poll::Ready(Some(Err(e))) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => self.done = true,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

async fn read_limited(
    response: reqwest::Response,
    limit: usize,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, HttpError> {
    let read = async {
        let mut out: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_reqwest_error)?;
            if out.len().saturating_add(chunk.len()) > limit {
                return Err(HttpError::BodyTooLarge { limit });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    };
    match deadline {
        Some(d) => timeout(d.saturating_duration_since(Instant::now()), read)
            .await
            .map_err(|_| HttpError::Timeout("response body".into()))?,
        None => read.await,
    }
}

fn map_reqwest_error(err: reqwest::Error) -> HttpError {
    // Never carry the URL: it may hold query-string credentials.
    let err = err.without_url();
    let message = {
        let mut m = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(s) = source {
            m.push_str(": ");
            m.push_str(&s.to_string());
            source = s.source();
        }
        m
    };
    if err.is_timeout() {
        HttpError::Timeout(message)
    } else if err.is_redirect() {
        HttpError::Transport {
            message: "too many redirects".into(),
            connect_phase: false,
        }
    } else if err.is_builder() {
        HttpError::InvalidRequest(message)
    } else {
        HttpError::Transport {
            connect_phase: err.is_connect(),
            message,
        }
    }
}
