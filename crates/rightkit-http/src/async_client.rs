//! Async HTTP client (feature `async`) on reqwest + tokio with rustls only.
//!
//! It applies the same policy layer as the blocking [`crate::Client`], driven by
//! the same [`ClientConfig`], [`Request`], [`Response`] and [`HttpError`] types:
//! per-phase timeouts, retry/backoff with `Retry-After`, retry-safety for
//! non-idempotent methods, redirect limits (credentials are never replayed to a
//! new host), user agent, bounded bodies and the error taxonomy. Every request and
//! redirect hop passes the outbound address policy ([`crate::netpolicy`]). Proxies are
//! used only when `ClientConfig::proxy` selects one ([`crate::ProxyMode`]). Calls must
//! run inside a tokio runtime.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use tokio::time::{sleep, timeout, Sleep};

use crate::client::{
    header_of, redact_str, redaction_values_for, scrub_error, status_error, ClientConfig, Method,
    Request, Response, RetryEvent,
};
use crate::error::{HttpError, Phase, MAX_ERROR_BODY_BYTES};
use crate::netpolicy::{denial_in_chain, normalize_host, AddressPolicy};
use crate::redirect;
use crate::retry::parse_retry_after;
use crate::sse::{SseDecoder, SseEvent};
use crate::urlredact::{redact_url, redact_urls_in_text};

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
    /// Normalized proxy host when requests go through a proxy: the endpoint is trusted and
    /// destinations are vetted by literal and name rules.
    proxy_host: Option<String>,
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
        // Redirects are followed by this crate, hop by hop, so the address policy and
        // header stripping run on every hop. reqwest must never follow one itself.
        let proxy = config.resolved_proxy()?;
        // `no_proxy()` also turns off reqwest's own environment proxy detection; a proxy,
        // when wanted, is installed explicitly below so its endpoint can be vetted.
        let mut builder = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        let proxy_host = proxy.as_ref().map(|p| p.host.clone());
        if let Some(p) = &proxy {
            let mut rp = reqwest::Proxy::all(p.url.as_str())
                .map_err(|_| HttpError::InvalidRequest("invalid proxy URL".into()))?;
            if !p.no_proxy.is_empty() {
                rp = rp.no_proxy(reqwest::NoProxy::from_string(&p.no_proxy.join(",")));
            }
            builder = builder.proxy(rp);
        }
        // The vetting resolver returns only checked addresses, and vets the proxy endpoint.
        builder = builder.dns_resolver(Arc::new(VettingDns {
            policy: Arc::new(config.address_policy.clone()),
            proxy_host: proxy_host.clone(),
        }));
        if let Some(d) = config.connect_timeout {
            builder = builder.connect_timeout(d);
        }
        let http = builder.build().map_err(map_reqwest_error)?;
        Ok(Self {
            http,
            config,
            proxy_host,
        })
    }

    /// Client that may reach only `hosts` (exact match), only over HTTPS; see
    /// [`ClientConfig::first_party`].
    pub fn first_party(hosts: Vec<String>) -> Result<Self, HttpError> {
        Self::new(ClientConfig::first_party(hosts))
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
            let values = redaction_values_for(&self.config, req);
            Err(scrub_error(
                status_error(resp.status, &resp.headers, resp.body, resp.attempts),
                &values,
            ))
        }
    }

    /// Buffered request returning any final status (retryable statuses are still retried).
    pub async fn send_any(&self, req: &Request) -> Result<Response, HttpError> {
        let total = req.timeout.unwrap_or(self.config.request_timeout);
        let raw = self.execute(req, Some(total)).await?;
        let deadline = self.body_deadline(raw.deadline);
        let values = redaction_values_for(&self.config, req);
        let body = read_limited(raw.response, self.config.max_response_bytes, deadline)
            .await
            .map_err(|e| scrub_error(e, &values))?;
        Ok(Response {
            status: raw.status,
            headers: raw.headers,
            body,
            attempts: raw.attempts,
        })
    }

    /// Streaming request; non-2xx becomes [`HttpError::Status`]. Retries happen
    /// only before the response headers arrive (never mid-stream). Uses
    /// `Request::timeout` as the total stream budget when set, otherwise
    /// `ClientConfig::stream_timeout`, like [`crate::Client::stream`].
    pub async fn stream(&self, req: &Request) -> Result<AsyncStreamResponse, HttpError> {
        let raw = self
            .execute(req, req.timeout.or(self.config.stream_timeout))
            .await?;
        if !(200..300).contains(&raw.status) {
            let values = redaction_values_for(&self.config, req);
            let body = read_limited(
                raw.response,
                MAX_ERROR_BODY_BYTES,
                self.body_deadline(raw.deadline),
            )
            .await
            .unwrap_or_default();
            return Err(scrub_error(
                status_error(raw.status, &raw.headers, body, raw.attempts),
                &values,
            ));
        }
        Ok(self.stream_response_from(raw))
    }

    /// Streaming request returning any final status with its streaming body
    /// (retryable statuses are still retried per policy; use
    /// `RetryPolicy::none()` for exactly one attempt). The caller inspects
    /// `status`/`headers` (e.g. `401` + `www-authenticate`) and bounds the body.
    /// Dropping the returned stream or this future cancels the request.
    pub async fn stream_any(&self, req: &Request) -> Result<AsyncStreamResponse, HttpError> {
        let raw = self
            .execute(req, req.timeout.or(self.config.stream_timeout))
            .await?;
        Ok(self.stream_response_from(raw))
    }

    fn stream_response_from(&self, raw: Raw) -> AsyncStreamResponse {
        let deadline = self.body_deadline(raw.deadline);
        AsyncStreamResponse {
            status: raw.status,
            headers: raw.headers,
            attempts: raw.attempts,
            body: BodyStream::new(Box::pin(raw.response.bytes_stream()), deadline),
        }
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
        let values = redaction_values_for(&self.config, req);
        self.execute_inner(req, total, &values)
            .await
            .map_err(|e| scrub_error(e, &values))
    }

    async fn execute_inner(
        &self,
        req: &Request,
        total: Option<Duration>,
        values: &[String],
    ) -> Result<Raw, HttpError> {
        let policy = req
            .retry
            .clone()
            .unwrap_or_else(|| self.config.retry.clone());
        let max = policy.max_attempts.max(1);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let outcome = self.once(req, total, values).await;
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
                        HttpError::Timeout { .. } => req.method.is_idempotent(),
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
                    url: redact_str(&redact_url(&req.url), values),
                    retry: attempt,
                    reason: redact_str(&redact_urls_in_text(&reason), values),
                    delay,
                });
            }
            sleep(delay).await;
        }
    }

    /// One attempt: the request and, up to `max_redirects`, its redirect hops. Every hop
    /// goes through the same address policy.
    async fn once(
        &self,
        req: &Request,
        total: Option<Duration>,
        secrets: &[String],
    ) -> Result<Raw, HttpError> {
        let started = Instant::now();
        let overall = total.map(|t| started + t);
        let mut method = req.method;
        let mut url = reqwest::Url::parse(&req.url)
            .map_err(|_| HttpError::InvalidRequest("invalid URL".into()))?;
        let mut headers = req.headers.clone();
        let mut body = req.body.clone();
        let mut hops = 0u32;
        loop {
            let remaining = match overall {
                None => None,
                Some(dl) => {
                    let left = dl.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        let phase = if hops > 0 {
                            Phase::Redirect
                        } else {
                            Phase::Headers
                        };
                        return Err(HttpError::timeout("request", phase));
                    }
                    Some(left)
                }
            };
            let raw = match self
                .once_hop(method, &url, &headers, &body, remaining)
                .await
            {
                Ok(raw) => raw,
                Err(e) if hops > 0 => return Err(e.into_redirect_phase()),
                Err(e) => return Err(e),
            };
            if self.config.max_redirects == 0 || !redirect::is_redirect_status(raw.status) {
                return Ok(Raw {
                    deadline: overall,
                    ..raw
                });
            }
            let Some(location) = header_of(&raw.headers, "location") else {
                return Ok(Raw {
                    deadline: overall,
                    ..raw
                });
            };
            let Some(next_method) = redirect::redirect_method(raw.status, method) else {
                return Ok(Raw {
                    deadline: overall,
                    ..raw
                });
            };
            if hops >= self.config.max_redirects {
                if self.config.max_redirects_will_error {
                    return Err(HttpError::transport("too many redirects", Phase::Redirect));
                }
                return Ok(Raw {
                    deadline: overall,
                    ..raw
                });
            }
            hops += 1;
            let next = url
                .join(location.trim())
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https"))
                .ok_or_else(|| {
                    HttpError::InvalidRequest("redirect with an unusable Location".into())
                })?;
            if next.origin() != url.origin() {
                redirect::strip_cross_origin(&mut headers, secrets);
            }
            if next_method != method {
                body.clear();
                redirect::strip_body_headers(&mut headers);
            }
            method = next_method;
            url = next;
            // `raw` (and its connection) is dropped here before the next hop.
        }
    }

    /// Admission for one URL: metadata names, the first-party pin, and IP literals. Names
    /// are vetted by [`VettingDns`] when the connection resolves them. Through a proxy the
    /// proxy resolves the name instead, so the name rules apply here and its address is
    /// not checked.
    fn precheck(&self, url: &reqwest::Url) -> Result<(), HttpError> {
        let policy: &AddressPolicy = &self.config.address_policy;
        let host = match url.host_str() {
            Some(h) => normalize_host(h),
            None => return Err(HttpError::InvalidRequest("URL has no host".into())),
        };
        policy.admit(url.scheme(), &host)?;
        if self.proxy_host.is_some() {
            // The proxy resolves the name, so vet it by literal and by name rules.
            policy.vet_via_proxy(&host)?;
        } else if let Some(addrs) = policy.fixed_addrs(&host) {
            policy.vet(&host, addrs)?;
        }
        Ok(())
    }

    async fn once_hop(
        &self,
        method: Method,
        url: &reqwest::Url,
        headers: &[(String, String)],
        body: &[u8],
        total: Option<Duration>,
    ) -> Result<Raw, HttpError> {
        let method_r = match method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
            Method::Head => reqwest::Method::HEAD,
        };
        self.precheck(url)?;
        let mut builder = self.http.request(method_r, url.clone());
        for (k, v) in headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        if !body.is_empty() || matches!(method, Method::Post | Method::Put | Method::Patch) {
            builder = builder.body(body.to_vec());
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
                .map_err(|_| HttpError::timeout("response headers", Phase::Headers))?,
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

/// reqwest DNS resolver that resolves once, vets every address, and returns only the
/// vetted ones. hyper connects to exactly the addresses returned.
struct VettingDns {
    policy: Arc<AddressPolicy>,
    /// Normalized proxy host: resolved but trusted, no address-class denial.
    proxy_host: Option<String>,
}

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

impl reqwest::dns::Resolve for VettingDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.policy.clone();
        let host = normalize_host(name.as_str());
        let is_proxy = self.proxy_host.as_deref() == Some(host.as_str());
        Box::pin(async move {
            let vetted = vetted_addrs(&policy, &host, is_proxy).await?;
            let iter = vetted
                .into_iter()
                .map(|ip| std::net::SocketAddr::new(ip, 0));
            Ok::<reqwest::dns::Addrs, BoxErr>(Box::new(iter))
        })
    }
}

/// Resolve `host` once and return its addresses only if every one passes the policy.
async fn vetted_addrs(
    policy: &AddressPolicy,
    host: &str,
    is_proxy: bool,
) -> Result<Vec<std::net::IpAddr>, BoxErr> {
    let addrs: Vec<std::net::IpAddr> = if let Some(fixed) = policy.fixed_addrs(host) {
        fixed
    } else if let Some(hook) = policy.resolver_hook() {
        hook(host)
    } else {
        match timeout(
            Duration::from_secs(10),
            tokio::net::lookup_host((host.to_string(), 0)),
        )
        .await
        {
            Ok(Ok(found)) => found.map(|a| a.ip()).collect(),
            Ok(Err(e)) => return Err(Box::new(e)),
            Err(_) => {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "name lookup timed out",
                )))
            }
        }
    };
    if is_proxy {
        // The proxy endpoint is trusted configuration: resolved, not classified.
        return if addrs.is_empty() {
            Err("proxy host did not resolve".into())
        } else {
            Ok(addrs)
        };
    }
    policy
        .vet(host, addrs)
        .map_err(|denial| Box::new(denial) as BoxErr)
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
                return Poll::Ready(Some(Err(HttpError::timeout("response body", Phase::Body))));
            }
        }
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(Ok(b))),
            Poll::Ready(Some(Err(e))) => {
                self.done = true;
                Poll::Ready(Some(Err(map_reqwest_error_in(e, Phase::Body))))
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
            let chunk = chunk.map_err(|e| map_reqwest_error_in(e, Phase::Body))?;
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
            .map_err(|_| HttpError::timeout("response body", Phase::Body))?,
        None => read.await,
    }
}

fn map_reqwest_error(err: reqwest::Error) -> HttpError {
    map_reqwest_error_in(err, Phase::Headers)
}

/// `default` is the phase for failures that say nothing more: `Headers` before the
/// response arrives, `Body` while reading it.
fn map_reqwest_error_in(err: reqwest::Error, default: Phase) -> HttpError {
    // The vetting resolver refused the destination.
    if let Some(denial) = denial_in_chain(&err) {
        return denial.into();
    }
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
    let phase = if err.is_connect() {
        Phase::Connect
    } else {
        default
    };
    if err.is_timeout() {
        HttpError::timeout(message, phase)
    } else if err.is_redirect() {
        HttpError::transport("too many redirects", Phase::Redirect)
    } else if err.is_builder() {
        HttpError::InvalidRequest(message)
    } else {
        HttpError::transport(message, phase)
    }
}
