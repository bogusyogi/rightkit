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

use crate::cancel::CancelToken;
use crate::client::{
    header_of, AttemptEvent, redact_str, redaction_values_for, scrub_error, status_error, ClientConfig, Method,
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
    /// Direct connections.
    http: reqwest::Client,
    /// Connections through the configured proxy; `Some` exactly when `proxy` is.
    http_proxy: Option<reqwest::Client>,
    verifier: &'static str,
    config: ClientConfig,
    /// The proxy in use (its `NO_PROXY` list decides which destinations bypass it).
    proxy: Option<ureq::Proxy>,
    /// `ProxyMode::System` decided per request URL (then `proxy` and `http_proxy` are `None`).
    routes: Option<AsyncRoutes>,
    proxy_diagnostic: Option<crate::client::ProxyDiagnostic>,
    proxy_mode_used: crate::client::ProxyModeUsed,
}

/// The per-request system routes and one transport per scheme's proxy.
#[derive(Clone)]
struct AsyncRoutes {
    routes: Arc<crate::client::SystemRoutes>,
    /// Through the `http` endpoint; `Some` exactly when the settings name one.
    http: Option<reqwest::Client>,
    /// Through the `https` endpoint; `Some` exactly when the settings name one.
    https: Option<reqwest::Client>,
}

struct Raw {
    status: u16,
    headers: Vec<(String, String)>,
    response: reqwest::Response,
    attempts: u32,
    /// Whole-attempt deadline (buffered: request timeout, streams: stream timeout).
    deadline: Option<Instant>,
}

/// The rustls configuration installed into reqwest, and what it is built from.
pub(crate) struct BuiltTls {
    pub(crate) config: rustls::ClientConfig,
    /// The verifier actually installed: `"platform"` or `"bundled"`.
    pub(crate) kind: &'static str,
    /// Number of trust anchors in the root store (`Bundled`); 0 for the OS verifier. Read by
    /// the unit tests only.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) anchors: usize,
}

/// Build the whole TLS configuration for a trust choice. Both branches install an explicit
/// rustls `ClientConfig` through `use_preconfigured_tls`, so root selection never depends on
/// which reqwest TLS features some other crate in the build turned on (`default-tls`,
/// `rustls-tls-native-roots`, ...). ALPN advertises `http/1.1` only: reqwest's `http2`
/// feature is not enabled here, and a server that selected `h2` would panic hyper-util.
pub(crate) fn build_tls(
    trust: crate::client::TrustRoots,
    extra_roots: &[Vec<u8>],
) -> Result<BuiltTls, HttpError> {
    let fail = |what: &str| HttpError::InvalidRequest(format!("TLS configuration: {what}"));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| fail("no protocol versions"))?;
    let (mut config, kind, anchors) = match trust {
        crate::client::TrustRoots::Platform => {
            let verifier = rustls_platform_verifier::Verifier::new(provider)
                .map_err(|_| fail("the platform verifier is unavailable"))?;
            let config = builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(verifier))
                .with_no_client_auth();
            (config, "platform", 0)
        }
        crate::client::TrustRoots::Bundled => {
            let mut store = rustls::RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            for der in extra_roots {
                store
                    .add(rustls::pki_types::CertificateDer::from(der.clone()))
                    .map_err(|_| fail("a test root is not a certificate"))?;
            }
            let anchors = store.len();
            let config = builder.with_root_certificates(store).with_no_client_auth();
            (config, "bundled", anchors)
        }
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(BuiltTls {
        config,
        kind,
        anchors,
    })
}

impl AsyncClient {
    /// Which certificate verifier this client was built with: `"platform"` (the OS
    /// verifier) or `"bundled"` (Mozilla roots). It reports the configuration that was
    /// installed into the HTTP stack, not the setting that was asked for.
    pub fn verifier_kind(&self) -> &'static str {
        self.verifier
    }

    fn reqwest_builder(config: &ClientConfig, tls: BuiltTls) -> reqwest::ClientBuilder {
        // `no_proxy()` also turns off reqwest's own environment proxy detection and its
        // own NO_PROXY matching: this crate makes the one routing decision per hop
        // (`AsyncClient::precheck`) and picks the client that does it.
        let mut builder = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .http1_only()
            .use_preconfigured_tls(tls.config);
        // The vetting resolver returns only checked addresses. An `http` proxy is pinned to
        // its address by `proxy_url`; an `https` proxy named by host is resolved here like
        // any destination, so a private one must be listed with `allow_if`.
        builder = builder.dns_resolver(Arc::new(VettingDns {
            policy: Arc::new(config.address_policy.clone()),
        }));
        if let Some(d) = config.connect_timeout {
            builder = builder.connect_timeout(d);
        }
        builder
    }

    pub fn new(config: ClientConfig) -> Result<Self, HttpError> {
        // Redirects are followed by this crate, hop by hop, so the address policy and
        // header stripping run on every hop. reqwest must never follow one itself.
        let (routing, proxy_mode_used) = config.resolved_routing_with_mode()?;
        let (proxy, system_routes) = match routing {
            crate::client::Routing::Fixed(proxy) => (proxy, None),
            crate::client::Routing::PerRequest(routes) => (None, Some(routes)),
        };
        let extra = crate::trust::test_roots();
        let tls = build_tls(config.trust, &extra)?;
        let verifier = tls.kind;
        let http = Self::reqwest_builder(&config, tls)
            .build()
            .map_err(map_reqwest_error)?;
        // With a proxy there are two clients: `http` connects directly, `http_proxy` sends
        // everything through the proxy with no exclusion list of its own. `precheck`
        // decides per request with the same matcher it vets with, so policy and transport
        // cannot disagree about which path a request takes.
        let through = |p: &crate::client::ResolvedProxy| -> Result<reqwest::Client, HttpError> {
            let mut rp = reqwest::Proxy::all(proxy_url(&config, p)?)
                .map_err(|_| HttpError::InvalidRequest("invalid proxy URL".into()))?;
            if let (Some(user), pass) = (p.ureq.username(), p.ureq.password()) {
                rp = rp.basic_auth(user, pass.unwrap_or(""));
            }
            let tls = build_tls(config.trust, &extra)?;
            Self::reqwest_builder(&config, tls)
                .proxy(rp)
                .build()
                .map_err(map_reqwest_error)
        };
        let http_proxy = proxy.as_ref().map(&through).transpose()?;
        let routes = match system_routes {
            Some(routes) => Some(AsyncRoutes {
                http: routes.http().map(&through).transpose()?,
                https: routes.https().map(&through).transpose()?,
                routes,
            }),
            None => None,
        };
        Ok(Self {
            http,
            http_proxy,
            verifier,
            config,
            routes,
            proxy_diagnostic: proxy.as_ref().map(crate::client::ProxyDiagnostic::of),
            proxy_mode_used,
            proxy: proxy.map(|p| p.ureq),
        })
    }

    /// The proxy this client uses, if any, without credentials. `None` also when
    /// `ProxyMode::System` is decided per request URL, because there is no single proxy to
    /// report.
    pub fn proxy_diagnostic(&self) -> Option<&crate::client::ProxyDiagnostic> {
        self.proxy_diagnostic.as_ref()
    }

    /// Which proxy source this client actually uses; see
    /// [`Client::proxy_mode_used`](crate::Client::proxy_mode_used).
    pub fn proxy_mode_used(&self) -> &crate::client::ProxyModeUsed {
        &self.proxy_mode_used
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
        let values = redaction_values_for(&self.config, req);
        if req.headers_only {
            // Dropping the response unread closes the connection.
            return Ok(Response {
                status: raw.status,
                headers: raw.headers,
                body: Vec::new(),
                attempts: raw.attempts,
                redact: values,
            });
        }
        let deadline = self.body_deadline(raw.deadline);
        let limit = req
            .max_response_bytes
            .unwrap_or(self.config.max_response_bytes);
        let body = read_limited(raw.response, limit, deadline, req.cancel.as_ref())
            .await
            .map_err(|e| scrub_error(e, &values))?;
        Ok(Response {
            status: raw.status,
            headers: raw.headers,
            body,
            attempts: raw.attempts,
            redact: values,
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
                req.cancel.as_ref(),
            )
            .await
            .unwrap_or_default();
            return Err(scrub_error(
                status_error(raw.status, &raw.headers, body, raw.attempts),
                &values,
            ));
        }
        Ok(self.stream_response_from(raw, req.cancel.clone()))
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
        Ok(self.stream_response_from(raw, req.cancel.clone()))
    }

    fn stream_response_from(&self, raw: Raw, cancel: Option<CancelToken>) -> AsyncStreamResponse {
        let deadline = self.body_deadline(raw.deadline);
        AsyncStreamResponse {
            status: raw.status,
            headers: raw.headers,
            attempts: raw.attempts,
            body: BodyStream::new(Box::pin(raw.response.bytes_stream()), deadline, cancel),
        }
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, HttpError> {
        let req = Request::get(url).header("Accept", "application/json");
        let values = redaction_values_for(&self.config, &req);
        self.send(&req)
            .await?
            .json()
            .map_err(|e| scrub_error(e, &values))
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
        let deadline = req.deadline.map(|d| Instant::now() + d);
        let cancel = req.cancel.as_ref();
        let mut attempt = 0u32;
        loop {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return Err(HttpError::Cancelled);
            }
            let attempt_total = match deadline {
                None => total,
                Some(dl) => {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(HttpError::timeout(
                            format!("request deadline exceeded after {attempt} attempt(s)"),
                            Phase::Headers,
                        ));
                    }
                    Some(total.map_or(remaining, |t| t.min(remaining)))
                }
            };
            attempt += 1;
            if let Some(hook) = &self.config.on_attempt {
                hook(&AttemptEvent {
                    url: redact_str(&redact_url(&req.url), values),
                    method: req.method,
                    attempt,
                    max_attempts: max,
                })
                .map_err(|reason| {
                    HttpError::InvalidRequest(format!("attempt {attempt} refused: {reason}"))
                })?;
            }
            let outcome = race_cancel(cancel, self.once(req, attempt_total, values)).await?;
            let (reason, retry_after) = match outcome {
                Ok(raw) => {
                    let retryable = policy.is_retryable_status(raw.status)
                        && (req.method.is_idempotent() || req.retry_unsafe_statuses);
                    let ra = header_of(&raw.headers, "retry-after").and_then(parse_retry_after);
                    if !retryable || attempt >= max || policy.defers(ra) {
                        return Ok(Raw {
                            attempts: attempt,
                            ..raw
                        });
                    }
                    if deadline_passes(deadline, policy.delay(attempt, ra)) {
                        return Ok(Raw {
                            attempts: attempt,
                            ..raw
                        });
                    }
                    // Drain so the connection is released, but never past any budget the
                    // caller set: the attempt's deadline, the whole-request deadline less
                    // the sleep that follows, and the body-phase timeout.
                    let delay = policy.delay(attempt, ra);
                    let drain = [
                        Some(Instant::now() + Duration::from_secs(5)),
                        raw.deadline,
                        deadline.map(|dl| dl.checked_sub(delay).unwrap_or_else(Instant::now)),
                        self.config.recv_body_timeout.map(|d| Instant::now() + d),
                    ]
                    .into_iter()
                    .flatten()
                    .min();
                    let drained =
                        read_limited(raw.response, MAX_ERROR_BODY_BYTES, drain, cancel).await;
                    if matches!(drained, Err(HttpError::Cancelled)) {
                        return Err(HttpError::Cancelled);
                    }
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
                    if deadline_passes(deadline, policy.delay(attempt, None)) {
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
            // Never sleep past the whole-request deadline: the next loop pass then reports
            // the elapsed deadline instead of a late retry.
            let delay = match deadline {
                Some(dl) => delay.min(dl.saturating_duration_since(Instant::now())),
                None => delay,
            };
            if !sleep_cancellable(delay, cancel).await {
                return Err(HttpError::Cancelled);
            }
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
        // The URL parser rewrites legacy IPv4 spellings, so the spelling check reads the host
        // from the string as written, and redirects are resolved on strings as well.
        let mut raw_url = req.url.clone();
        let mut url = reqwest::Url::parse(&raw_url)
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
                .once_hop(method, &url, &raw_url, &headers, &body, remaining, hops > 0)
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
            let unusable =
                || HttpError::InvalidRequest("redirect with an unusable Location".into());
            let next_raw = redirect::resolve_location(&raw_url, location).ok_or_else(unusable)?;
            let next = reqwest::Url::parse(&next_raw)
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https"))
                .ok_or_else(unusable)?;
            // `https_only` first: its error names the rule the caller set, where the address
            // policy's downgrade refusal (always on) would hide it.
            redirect::require_https(
                self.config.https_only,
                next.scheme(),
                next.host_str().unwrap_or(""),
                true,
            )?;
            self.config
                .address_policy
                .check_redirect_scheme(url.scheme(), next.scheme(), next.host_str().unwrap_or(""))
                .map_err(|d| HttpError::from(d).into_redirect_phase())?;
            if next.origin() != url.origin() {
                redirect::strip_cross_origin(&mut headers, secrets);
            }
            if next_method != method {
                body.clear();
                redirect::strip_body_headers(&mut headers);
            }
            method = next_method;
            url = next;
            raw_url = next_raw;
            // `raw` (and its connection) is dropped here before the next hop.
        }
    }

    /// Admission for one URL: metadata names, the first-party pin, and IP literals. Names
    /// are vetted by [`VettingDns`] when the connection resolves them. Through a proxy the
    /// proxy resolves the name instead, so the name rules apply here and its address is
    /// not checked. Legacy IPv4 spellings are canonicalised first, as the URL parser does.
    fn precheck(
        &self,
        url: &reqwest::Url,
        raw_url: &str,
        redirect_hop: bool,
    ) -> Result<bool, HttpError> {
        let policy: &AddressPolicy = &self.config.address_policy;
        let host = match url.host_str() {
            Some(h) => normalize_host(h),
            None => return Err(HttpError::InvalidRequest("URL has no host".into())),
        };
        let raw_host = redirect::raw_host_of(raw_url).unwrap_or_else(|| host.clone());
        redirect::require_https(
            self.config.https_only,
            url.scheme(),
            &raw_host,
            redirect_hop,
        )?;
        policy.admit(url.scheme(), &raw_host)?;
        let via_proxy = if let Some(r) = &self.routes {
            // Per request: the bypass list and the scheme's own endpoint decide.
            r.routes
                .proxy_for(&url.scheme().to_ascii_lowercase(), &host)
                .is_some()
        } else {
            self.proxy.as_ref().is_some_and(|p| {
                url.as_str()
                    .parse::<ureq::http::Uri>()
                    .map(|uri| !p.is_no_proxy(&uri))
                    .unwrap_or(true)
            })
        };
        if via_proxy {
            // The proxy resolves the name, so vet it by literal and by name rules.
            policy.vet_via_proxy(&host)?;
        } else if let Some(addrs) = policy.fixed_addrs(&host) {
            policy.vet(&host, addrs)?;
        }
        Ok(via_proxy)
    }

    #[allow(clippy::too_many_arguments)]
    async fn once_hop(
        &self,
        method: Method,
        url: &reqwest::Url,
        raw_url: &str,
        headers: &[(String, String)],
        body: &[u8],
        total: Option<Duration>,
        redirect_hop: bool,
    ) -> Result<Raw, HttpError> {
        let method_r = match method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
            Method::Head => reqwest::Method::HEAD,
        };
        let via_proxy = self.precheck(url, raw_url, redirect_hop)?;
        // The transport follows the decision the policy was applied for.
        let client = match (&self.routes, &self.http_proxy, via_proxy) {
            (Some(r), _, true) => {
                let proxied = if url.scheme() == "https" {
                    &r.https
                } else {
                    &r.http
                };
                // `via_proxy` came from `proxy_for`, so this transport exists; never fall
                // back to a direct connection if it somehow does not.
                proxied.as_ref().ok_or_else(|| {
                    HttpError::InvalidRequest("system proxy route unavailable".into())
                })?
            }
            (None, Some(proxied), true) => proxied,
            _ => &self.http,
        };
        let mut builder = client.request(method_r, url.clone());
        for (k, v) in headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        // A bodiless GET, HEAD or DELETE sends no body and no `Content-Length`. POST, PUT and
        // PATCH always send their body; an empty one carries `Content-Length: 0` unless the
        // caller set its own length, which the blocking client matches.
        if !body.is_empty() {
            builder = builder.body(body.to_vec());
        } else if matches!(method, Method::Post | Method::Put | Method::Patch) {
            builder = builder.body(Vec::new());
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            {
                builder = builder.header("content-length", "0");
            }
        }
        let request = builder.build().map_err(map_reqwest_error)?;

        let started = Instant::now();
        let deadline = total.map(|t| started + t);
        let headers_budget = match (self.config.response_timeout, total) {
            (Some(r), Some(t)) => Some(r.min(t)),
            (r, t) => r.or(t),
        };
        let send = client.execute(request);
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
}

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

impl reqwest::dns::Resolve for VettingDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.policy.clone();
        let host = normalize_host(name.as_str());
        Box::pin(async move {
            let vetted = vetted_addrs(&policy, &host).await?;
            let iter = vetted
                .into_iter()
                .map(|ip| std::net::SocketAddr::new(ip, 0));
            Ok::<reqwest::dns::Addrs, BoxErr>(Box::new(iter))
        })
    }
}

/// Resolve `host` once and return its addresses only if every one passes the policy.
async fn vetted_addrs(policy: &AddressPolicy, host: &str) -> Result<Vec<std::net::IpAddr>, BoxErr> {
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

/// How often a pending [`BodyStream`] with a cancel token wakes itself to look at the token.
const CANCEL_POLL: Duration = Duration::from_millis(25);

/// Body chunks with the stream/body deadline applied and errors typed.
pub struct BodyStream {
    inner: ByteStream,
    deadline: Option<Pin<Box<Sleep>>>,
    cancel: Option<CancelToken>,
    /// Present exactly when `cancel` is. [`CancelToken::cancel`] only sets an atomic flag and
    /// wakes no task, so a read that is pending on a server that sends nothing would never
    /// observe it. This timer is re-armed every [`CANCEL_POLL`] while the stream is polled and
    /// pending, which wakes the task to re-check the flag.
    cancel_tick: Option<Pin<Box<Sleep>>>,
    done: bool,
}

impl BodyStream {
    fn new(inner: ByteStream, deadline: Option<Instant>, cancel: Option<CancelToken>) -> Self {
        let deadline = deadline.map(|d| {
            Box::pin(tokio::time::sleep(
                d.saturating_duration_since(Instant::now()),
            ))
        });
        let cancel_tick = cancel
            .as_ref()
            .map(|_| Box::pin(tokio::time::sleep(CANCEL_POLL)));
        Self {
            inner,
            deadline,
            cancel,
            cancel_tick,
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
        if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            self.done = true;
            return Poll::Ready(Some(Err(HttpError::Cancelled)));
        }
        // Make sure this task is woken within `CANCEL_POLL` even when the body is silent.
        if let Some(tick) = self.cancel_tick.as_mut() {
            if tick.as_mut().poll(cx).is_ready() {
                // Fired: arm the next interval (polling registers the waker), then re-check.
                tick.as_mut()
                    .reset(tokio::time::Instant::now() + CANCEL_POLL);
                let _ = tick.as_mut().poll(cx);
                if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
                    self.done = true;
                    return Poll::Ready(Some(Err(HttpError::Cancelled)));
                }
            }
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
    cancel: Option<&CancelToken>,
) -> Result<Vec<u8>, HttpError> {
    let read = async {
        let mut out: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = race_cancel(cancel, stream.next()).await? {
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

fn deadline_passes(deadline: Option<Instant>, delay: Duration) -> bool {
    deadline.is_some_and(|dl| Instant::now() + delay >= dl)
}

/// Run `fut`, giving up with [`HttpError::Cancelled`] as soon as `token` is cancelled.
/// The token is polled every 25 ms.
async fn race_cancel<F: Future>(
    token: Option<&CancelToken>,
    fut: F,
) -> Result<F::Output, HttpError> {
    let Some(token) = token else {
        return Ok(fut.await);
    };
    if token.is_cancelled() {
        return Err(HttpError::Cancelled);
    }
    let watch = async {
        while !token.is_cancelled() {
            sleep(Duration::from_millis(25)).await;
        }
    };
    futures_util::pin_mut!(fut);
    futures_util::pin_mut!(watch);
    match futures_util::future::select(fut, watch).await {
        futures_util::future::Either::Left((out, _)) => Ok(out),
        futures_util::future::Either::Right(_) => Err(HttpError::Cancelled),
    }
}

/// Sleep for `total`, waking every 50 ms to observe cancellation. False when cancelled.
async fn sleep_cancellable(total: Duration, token: Option<&CancelToken>) -> bool {
    let until = Instant::now() + total;
    loop {
        if token.is_some_and(CancelToken::is_cancelled) {
            return false;
        }
        let now = Instant::now();
        if now >= until {
            return true;
        }
        sleep((until - now).min(Duration::from_millis(50))).await;
    }
}

/// URL for reqwest's proxy. An `http` proxy is resolved here, once, and addressed by IP so
/// the connection cannot be redirected by a later lookup; an `https` proxy keeps its name
/// (TLS needs it) and is vetted as an ordinary destination by the DNS resolver.
fn proxy_url(config: &ClientConfig, p: &crate::client::ResolvedProxy) -> Result<String, HttpError> {
    if p.ureq.protocol() != ureq::ProxyProtocol::Http {
        return Ok(p.url.clone());
    }
    let policy = &config.address_policy;
    let addrs = match policy.fixed_addrs(&p.host) {
        Some(a) => a,
        None => policy
            .lookup_blocking(&p.host, p.port, Some(Duration::from_secs(10)))
            .map_err(|_| HttpError::transport("proxy host not found", Phase::Connect))?,
    };
    let ip = addrs
        .first()
        .ok_or_else(|| HttpError::transport("proxy host not found", Phase::Connect))?;
    Ok(match ip {
        std::net::IpAddr::V4(v4) => format!("http://{v4}:{}", p.port),
        std::net::IpAddr::V6(v6) => format!("http://[{v6}]:{}", p.port),
    })
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

#[cfg(test)]
mod tls_tests {
    use super::*;
    use crate::client::TrustRoots;

    #[test]
    fn both_branches_install_an_explicit_config_that_advertises_http1_only() {
        for trust in [TrustRoots::Platform, TrustRoots::Bundled] {
            let built = build_tls(trust, &[]).unwrap();
            assert_eq!(built.config.alpn_protocols, vec![b"http/1.1".to_vec()]);
            assert_eq!(built.kind, trust.verifier_kind());
        }
    }

    #[test]
    fn bundled_is_exactly_the_mozilla_roots_and_platform_has_no_store() {
        let bundled = build_tls(TrustRoots::Bundled, &[]).unwrap();
        assert_eq!(bundled.anchors, webpki_roots::TLS_SERVER_ROOTS.len());
        assert!(bundled.anchors > 100);
        let platform = build_tls(TrustRoots::Platform, &[]).unwrap();
        assert_eq!(platform.anchors, 0);
        // A registered test root must be a certificate.
        assert!(build_tls(TrustRoots::Bundled, &[vec![1, 2, 3]]).is_err());
    }

    #[test]
    fn clients_report_the_verifier_that_was_installed() {
        let c = AsyncClient::new(ClientConfig::default()).unwrap();
        assert_eq!(c.verifier_kind(), "platform");
        let c = AsyncClient::new(ClientConfig::default().trust(TrustRoots::Bundled)).unwrap();
        assert_eq!(c.verifier_kind(), "bundled");
    }
}
