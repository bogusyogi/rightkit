use std::fmt;
use std::io::{self, Read};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::cancel::{is_cancelled_io, is_set, sleep_cancellable, CancelToken, CheckedReader};
use crate::error::{HttpError, Phase, MAX_ERROR_BODY_BYTES};
use crate::netpolicy::{denial_in_chain, normalize_host, AddressPolicy, VettingResolver, ViaProxy};
use crate::redirect;
use crate::retry::{parse_retry_after, RetryPolicy};
use crate::sse::SseReader;
use crate::urlredact::{redact_url, redact_urls_in_text, url_secret_parts};

pub(crate) const REDACTED: &str = "[redacted]";

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

/// One retry decision, reported to [`ClientConfig::on_retry`]. `url` and `reason`
/// have configured and per-request secret values replaced by `[redacted]`.
#[derive(Debug, Clone)]
pub struct RetryEvent {
    pub url: String,
    /// Retry number about to run (1 = first retry).
    pub retry: u32,
    pub reason: String,
    pub delay: Duration,
}

type RetryObserver = Arc<dyn Fn(&RetryEvent) + Send + Sync>;

/// How requests reach the network.
///
/// **Threat model.** A direct connection is the strongest case: this crate resolves the
/// name, checks every address and connects to exactly those addresses. Through a proxy it
/// cannot: the proxy resolves the destination name itself, so the address a connection
/// reaches cannot be checked or pinned here. The guarantee through a proxy is therefore
/// name- and literal-based, and weaker than a direct connection. `ProxyMode::None` stays
/// the default for that reason.
///
/// With a proxy:
///
/// - The proxy endpoint is trusted (it is user or administrator configuration, often on a
///   private address such as `http://10.0.0.5:3128`). It gets no address-class denial.
///   Only `http` and `https` proxies are accepted.
/// - Every destination is still vetted: IP literals (and `localhost`) by class, hostnames
///   by the name rules (`localhost`, `*.localhost`, `*.local`, `*.internal` and
///   single-label names are refused unless an `allow_if` rule lists the host), cloud
///   metadata names, and the first-party host pin. `deny_ranges` and `deny_if` apply to
///   literals only, because a hostname's address is not known. A hostname that resolves to
///   a private address at the proxy is not caught.
/// - Every redirect hop is vetted the same way.
/// - Hosts that match `NO_PROXY` connect directly and get the full direct-connection check.
/// - [`ClientConfig::proxy_bypass_first_party`] skips the proxy for a first-party client.
///
/// The proxy is trusted: it sees destinations and can forward anywhere. Credentials in a
/// proxy URL never appear in `Debug` output or errors.
#[derive(Clone, Default)]
pub enum ProxyMode {
    /// No proxy. Environment proxy variables are ignored. This is the default.
    #[default]
    None,
    /// `ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` (first set, either case) and `NO_PROXY`.
    Environment,
    /// A proxy URL such as `http://proxy.corp:3128` or `socks5://127.0.0.1:1080`.
    Explicit(String),
    /// The proxy the operating system is configured with for the current user.
    ///
    /// Windows: the static proxy and bypass list of the system proxy settings, followed
    /// exactly or not at all. One `http` or `https` proxy for both schemes with a bypass list
    /// of plain hosts (`host`, `*.example.com`) is used. Anything else (different routes per
    /// scheme, `<local>`, a scheme-limited or inner-wildcard bypass entry, a proxy
    /// auto-config script, automatic detection) is
    /// [`HttpError::SystemProxyUnsupported`](crate::HttpError::SystemProxyUnsupported) and
    /// nothing is sent; catch it and use the mode the app used before. When nothing is
    /// configured, and on other systems, this is [`ProxyMode::Environment`].
    System,
}

impl fmt::Debug for ProxyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyMode::None => f.write_str("None"),
            ProxyMode::Environment => f.write_str("Environment"),
            ProxyMode::Explicit(url) => write!(f, "Explicit({})", redact_url(url)),
            ProxyMode::System => f.write_str("System"),
        }
    }
}

/// Which certificate roots verify a server's TLS certificate.
///
/// Verification is always on; there is no option that turns it off. `Platform` hands the
/// decision to the operating system (macOS Security framework, Windows CryptoAPI, the
/// system roots on Linux) through `rustls-platform-verifier` in both clients, so roots an
/// enterprise or an inspection proxy installed on the machine are honoured, as curl and
/// browsers honour them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrustRoots {
    /// The operating system's trust store. This is the default. When the store cannot be
    /// used the client fails closed: the handshake (blocking) or `AsyncClient::new` (async)
    /// returns an error.
    #[default]
    Platform,
    /// Mozilla's root certificates compiled into the crate (`webpki-roots`), ignoring the
    /// machine's store. Use it when the machine's roots must not be trusted, or when
    /// results must not depend on the machine. A host behind TLS inspection will fail.
    Bundled,
}

/// ureq TLS settings for a trust choice. The provider and the root source are named
/// explicitly, so no other crate's additive features change what is trusted.
fn blocking_tls_config(trust: TrustRoots) -> ureq::tls::TlsConfig {
    let roots = match trust {
        TrustRoots::Platform => ureq::tls::RootCerts::PlatformVerifier,
        TrustRoots::Bundled => {
            let extra = crate::trust::test_roots();
            if extra.is_empty() {
                ureq::tls::RootCerts::WebPki
            } else {
                ureq::tls::RootCerts::Specific(std::sync::Arc::new(
                    extra
                        .iter()
                        .map(|d| ureq::tls::Certificate::from_der(d).to_owned())
                        .collect(),
                ))
            }
        }
    };
    ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .root_certs(roots)
        .build()
}

impl TrustRoots {
    /// `"platform"` or `"bundled"`: the verifier a client built from this setting uses.
    pub fn verifier_kind(self) -> &'static str {
        match self {
            TrustRoots::Platform => "platform",
            TrustRoots::Bundled => "bundled",
        }
    }
}

/// Proxy settings resolved from a [`ProxyMode`].
pub(crate) struct ResolvedProxy {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) ureq: ureq::Proxy,
    /// The proxy URL as configured. It may carry credentials: never print it.
    #[cfg_attr(not(feature = "async"), allow(dead_code))]
    pub(crate) url: String,
    /// `NO_PROXY`-style entries as configured (the matcher lives in `ureq`).
    #[allow(dead_code)]
    pub(crate) no_proxy: Vec<String>,
    /// True when the proxy came from the environment or the operating system's settings.
    pub(crate) from_env: bool,
}

/// What a client knows about the proxy it uses, for diagnostics. Credentials are never part
/// of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyDiagnostic {
    /// `scheme://host:port` of the proxy, without credentials, path or query.
    pub endpoint: String,
    /// True when the proxy was taken from the environment or from the operating system's
    /// settings ([`ProxyMode::System`]), false for `ProxyMode::Explicit`.
    pub from_environment: bool,
    /// The proxy endpoint is exempt from the destination class check because the caller or
    /// the environment configured it. Destinations are still checked: literals by class,
    /// names by the name rules, and `NO_PROXY` hosts fully.
    pub endpoint_exempt_from_class_check: bool,
}

impl ProxyDiagnostic {
    pub(crate) fn of(p: &ResolvedProxy) -> Self {
        Self {
            endpoint: format!("{}://{}:{}", p.scheme(), p.host, p.port),
            from_environment: p.from_env,
            endpoint_exempt_from_class_check: true,
        }
    }
}

impl ResolvedProxy {
    fn scheme(&self) -> &'static str {
        if self.ureq.protocol() == ureq::ProxyProtocol::Https {
            "https"
        } else {
            "http"
        }
    }
}

const PROXY_ENV: [&str; 6] = [
    "ALL_PROXY",
    "all_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
];

/// Build an `http`/`https` proxy that resolves destination names itself.
fn build_proxy(url: &str, no_proxy: &[String]) -> Result<ResolvedProxy, HttpError> {
    let invalid = || HttpError::InvalidRequest("invalid proxy URL".into());
    let base = ureq::Proxy::new(url).map_err(|_| invalid())?;
    if !matches!(
        base.protocol(),
        ureq::ProxyProtocol::Http | ureq::ProxyProtocol::Https
    ) {
        return Err(invalid());
    }
    let mut builder = ureq::Proxy::builder(base.protocol())
        .host(base.host())
        .port(base.port())
        .resolve_target(false);
    if let Some(user) = base.username() {
        builder = builder.username(user);
    }
    if let Some(pass) = base.password() {
        builder = builder.password(pass);
    }
    for expr in no_proxy {
        builder = builder.no_proxy(expr);
    }
    let ureq = builder.build().map_err(|_| invalid())?;
    Ok(ResolvedProxy {
        host: normalize_host(ureq.host()),
        port: ureq.port(),
        ureq,
        url: url.to_string(),
        no_proxy: no_proxy.to_vec(),
        from_env: false,
    })
}

/// Proxy from environment-style variables read through `get`. Empty values are unset.
pub(crate) fn proxy_from_lookup(
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<ResolvedProxy>, HttpError> {
    let no_proxy: Vec<String> = ["NO_PROXY", "no_proxy"]
        .iter()
        .find_map(|name| get(name).filter(|v| !v.is_empty()))
        .map(|v| {
            v.split(',')
                .map(|e| e.trim().to_string())
                .filter(|e| !e.is_empty())
                .collect()
        })
        .unwrap_or_default();
    for name in PROXY_ENV {
        if let Some(url) = get(name).filter(|v| !v.trim().is_empty()) {
            if let Ok(mut proxy) = build_proxy(url.trim(), &no_proxy) {
                proxy.from_env = true;
                return Ok(Some(proxy));
            }
        }
    }
    Ok(None)
}

impl ClientConfig {
    /// The proxy this config uses, or `None` when it connects directly.
    pub(crate) fn resolved_proxy(&self) -> Result<Option<ResolvedProxy>, HttpError> {
        if self.proxy_bypass_first_party && self.address_policy.is_first_party() {
            return Ok(None);
        }
        match &self.proxy {
            ProxyMode::None => Ok(None),
            ProxyMode::Explicit(url) => build_proxy(url, &[]).map(Some),
            ProxyMode::Environment => proxy_from_lookup(&|name| std::env::var(name).ok()),
            ProxyMode::System => {
                match proxy_from_system(&crate::system_proxy::system_proxy_settings())? {
                    Some(proxy) => Ok(Some(proxy)),
                    None => proxy_from_lookup(&|name| std::env::var(name).ok()),
                }
            }
        }
    }
}

/// The proxy the operating system's static settings name, treated like an environment proxy.
/// Settings that name no usable proxy, or one this crate cannot build, give `Ok(None)`.
///
/// Settings this client cannot follow exactly (see [`crate::system_proxy`]) are an error that
/// names the first such element. A client uses one proxy for every request and a bypass list
/// of plain hosts, so following them approximately would send traffic to a proxy, or around
/// it, against what the user configured. The client then fails closed, as for any invalid
/// proxy setting.
pub(crate) fn proxy_from_system(
    settings: &crate::system_proxy::SystemProxySettings,
) -> Result<Option<ResolvedProxy>, HttpError> {
    use crate::system_proxy::SystemProxyChoice;
    match crate::system_proxy::proxy_choice(settings) {
        SystemProxyChoice::None => Ok(None),
        SystemProxyChoice::Single { url, no_proxy } => {
            Ok(build_proxy(&url, &no_proxy).ok().map(|mut proxy| {
                proxy.from_env = true;
                proxy
            }))
        }
        SystemProxyChoice::Unsupported(element) => {
            Err(HttpError::SystemProxyUnsupported { element })
        }
    }
}

#[derive(Clone)]
pub struct ClientConfig {
    pub user_agent: String,
    /// Connection-phase deadline; `None` disables this per-phase deadline.
    pub connect_timeout: Option<Duration>,
    /// Whole-request deadline for buffered calls (headers + body).
    pub request_timeout: Duration,
    /// Time to receive response headers; for streams this is the first-byte
    /// (first-token) deadline. `None` disables this per-phase deadline.
    pub response_timeout: Option<Duration>,
    /// Total time to receive the body, starting after response headers arrive.
    /// The budget is not restarted per read. `None` disables this phase deadline.
    pub recv_body_timeout: Option<Duration>,
    /// Optional total lifetime cap for a stream body.
    pub stream_timeout: Option<Duration>,
    /// Redirects followed (0 = never; credentials are never replayed to a new host).
    pub max_redirects: u32,
    /// Error when a nonzero redirect limit is exceeded (default true, like ureq).
    /// False returns the final 3xx response. With `max_redirects = 0`, redirects
    /// are never followed and the 3xx response is always returned.
    pub max_redirects_will_error: bool,
    pub max_response_bytes: usize,
    pub retry: RetryPolicy,
    pub on_retry: Option<RetryObserver>,
    /// Secret strings scrubbed from every error message and retry event. Add
    /// values with [`ClientConfig::redact_values`].
    pub redact_values: Vec<String>,
    /// Outbound address policy. The default is public-web policy: loopback, private,
    /// link-local, CGNAT, multicast and reserved destinations are denied on the first
    /// request and on every redirect hop. See [`crate::netpolicy`].
    pub address_policy: AddressPolicy,
    /// Proxy use. The default is [`ProxyMode::None`]: no proxy, and environment proxy
    /// variables are ignored. See [`ProxyMode`] for the threat model.
    pub proxy: ProxyMode,
    /// When the address policy is first-party, connect to the first-party hosts directly
    /// even if `proxy` is set. Default `false`: first-party calls use the proxy too.
    pub proxy_bypass_first_party: bool,
    /// Certificate roots used to verify servers. The default is [`TrustRoots::Platform`]
    /// (the OS trust store); [`TrustRoots::Bundled`] restores the Mozilla set used before
    /// 0.3.0. Verification cannot be disabled.
    pub trust: TrustRoots,
    /// Refuse any URL that is not `https`, on the first request and on every redirect hop,
    /// with [`HttpError::HttpsRequired`]. Default `false`, which keeps the earlier
    /// behaviour (plain `http` allowed; an HTTPS to HTTP redirect is still refused by the
    /// address policy unless it opts in).
    pub https_only: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            user_agent: concat!("rightkit-http/", env!("CARGO_PKG_VERSION")).to_string(),
            connect_timeout: Some(Duration::from_secs(10)),
            request_timeout: Duration::from_secs(60),
            response_timeout: Some(Duration::from_secs(60)),
            recv_body_timeout: None,
            stream_timeout: None,
            max_redirects: 0,
            max_redirects_will_error: true,
            max_response_bytes: 32 * 1024 * 1024,
            retry: RetryPolicy::default(),
            on_retry: None,
            redact_values: Vec::new(),
            address_policy: AddressPolicy::default(),
            proxy: ProxyMode::None,
            proxy_bypass_first_party: false,
            trust: TrustRoots::Platform,
            https_only: false,
        }
    }
}

impl ClientConfig {
    /// Default settings with a first-party policy: only `hosts` (exact match), only over
    /// HTTPS, resolving to public addresses unless the policy opts a host out with
    /// `AddressPolicy::allow_private_resolution`. Use it for license checks and model downloads against known hosts.
    pub fn first_party(hosts: Vec<String>) -> Self {
        Self::default().address_policy(AddressPolicy::first_party(hosts))
    }

    /// Settings that match a plain `ureq` agent, for callers moving off direct ureq: environment
    /// proxy variables honoured ([`ProxyMode::Environment`]), Mozilla roots ([`TrustRoots::Bundled`]),
    /// 10 redirects, no retries ([`RetryPolicy::none`]) and the given user agent. Every other field
    /// keeps its [`Default`] value. It does NOT enable private-address resolution: the address policy
    /// stays public-web unless set explicitly.
    pub fn legacy_ureq_parity(user_agent: impl Into<String>) -> Self {
        Self {
            user_agent: user_agent.into(),
            proxy: ProxyMode::Environment,
            trust: TrustRoots::Bundled,
            max_redirects: 10,
            retry: RetryPolicy::none(),
            ..Self::default()
        }
    }

    /// Replace the outbound address policy.
    pub fn address_policy(mut self, policy: AddressPolicy) -> Self {
        self.address_policy = policy;
        self
    }

    /// Set the proxy mode; see [`ProxyMode`].
    pub fn proxy(mut self, mode: ProxyMode) -> Self {
        self.proxy = mode;
        self
    }

    /// Choose the certificate roots; see [`TrustRoots`].
    pub fn trust(mut self, trust: TrustRoots) -> Self {
        self.trust = trust;
        self
    }

    /// Refuse non-`https` URLs and redirect hops; see [`ClientConfig::https_only`].
    pub fn https_only(mut self, https_only: bool) -> Self {
        self.https_only = https_only;
        self
    }

    /// Let a first-party client reach its hosts without the proxy; see
    /// [`ClientConfig::proxy_bypass_first_party`].
    pub fn proxy_bypass_first_party(mut self, bypass: bool) -> Self {
        self.proxy_bypass_first_party = bypass;
        self
    }

    /// Add secret strings to scrub from error text (`[redacted]`). Empty strings are ignored.
    pub fn redact_values(mut self, values: Vec<String>) -> Self {
        self.redact_values
            .extend(values.into_iter().filter(|v| !v.is_empty()));
        self
    }
}

#[derive(Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Per-attempt total timeout (buffered calls and stream headers).
    pub timeout: Option<Duration>,
    pub retry: Option<RetryPolicy>,
    /// Allow retrying a POST/PATCH after HTTP-status failures (429, 5xx, ...). Default
    /// `false` since 0.3.0: a replayed POST (a single-use OAuth code exchange, a payment) can
    /// be harmful. Transport errors on non-idempotent methods retry only in the connect
    /// phase regardless. Set `true` per request for an idempotent POST; see the README for
    /// how this interacts with `RetryPolicy::none()`.
    pub retry_unsafe_statuses: bool,
    pub(crate) max_response_bytes: Option<usize>,
    pub(crate) deadline: Option<Duration>,
    pub(crate) cancel: Option<CancelToken>,
    pub(crate) secret_values: Vec<String>,
    pub(crate) headers_only: bool,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(String, String)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                let shown = if redirect::is_secret_header_name(k) {
                    REDACTED.to_string()
                } else {
                    redact_str(&redact_urls_in_text(v), &sorted_values(&self.secret_values))
                };
                (k.clone(), shown)
            })
            .collect();
        f.debug_struct("Request")
            .field("method", &self.method)
            .field(
                "url",
                &redact_str(&redact_url(&self.url), &self.secret_values),
            )
            .field("headers", &headers)
            .field("body_len", &self.body.len())
            .field("timeout", &self.timeout)
            .field("retry", &self.retry)
            .field("retry_unsafe_statuses", &self.retry_unsafe_statuses)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("deadline", &self.deadline)
            .field("cancel", &self.cancel.is_some())
            .field("headers_only", &self.headers_only)
            .finish()
    }
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
            retry_unsafe_statuses: false,
            max_response_bytes: None,
            deadline: None,
            cancel: None,
            secret_values: Vec::new(),
            headers_only: false,
        }
    }
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::Get, url)
    }
    pub fn post(url: impl Into<String>) -> Self {
        Self::new(Method::Post, url)
    }
    pub fn put(url: impl Into<String>) -> Self {
        Self::new(Method::Put, url)
    }
    pub fn delete(url: impl Into<String>) -> Self {
        Self::new(Method::Delete, url)
    }
    /// Append a header. Use [`Request::set_header`] to replace an existing one.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    /// Set a header, replacing every existing header with the same name
    /// (ASCII case-insensitive). The first occurrence keeps its position.
    pub fn set_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        let value = value.into();
        let position = self
            .headers
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(&name));
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&name));
        match position {
            Some(i) => self.headers.insert(i, (name, value)),
            None => self.headers.push((name, value)),
        }
        self
    }
    /// Set a header whose value is a secret. The value is sent on the wire but
    /// never appears in error text, retry events, or `Debug` output.
    pub fn secret_header(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        let mut req = self;
        if !value.is_empty() {
            req.secret_values.push(value.clone());
        }
        req.set_header(name, value)
    }
    /// `Authorization: Bearer <token>`. The token is treated as a secret.
    pub fn bearer(mut self, token: &str) -> Self {
        if !token.is_empty() {
            self.secret_values.push(token.to_string());
        }
        self.header("Authorization", format!("Bearer {token}"))
    }
    /// Register a secret that is not a header, for example a form field or a value inside a
    /// body. It is scrubbed from error text, retry events and `Debug` output exactly like a
    /// [`Request::secret_header`] value. Empty values are ignored.
    pub fn redact_value(mut self, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.is_empty() {
            self.secret_values.push(value);
        }
        self
    }
    /// [`Request::form`] that also registers the value of every pair whose name is in
    /// `secret_names` (compared ignoring ASCII case) as a secret, so a token or client secret
    /// sent in the body is scrubbed from errors like a secret header.
    pub fn secret_form(mut self, pairs: &[(&str, &str)], secret_names: &[&str]) -> Self {
        for (name, value) in pairs {
            if secret_names.iter().any(|s| s.eq_ignore_ascii_case(name)) && !value.is_empty() {
                self.secret_values.push((*value).to_string());
            }
        }
        self.form(pairs)
    }
    /// Replace every secret value registered on this request (not the client's configured
    /// ones) in `text` with `[redacted]`. For text the caller took from a response, such as
    /// an error field a provider echoed back, where no HTTP error carries it for scrubbing.
    pub fn scrub(&self, text: &str) -> String {
        redact_str(text, &sorted_values(&self.secret_values))
    }
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
    pub fn json<T: Serialize>(mut self, value: &T) -> Result<Self, HttpError> {
        self.body = serde_json::to_vec(value).map_err(json_error)?;
        self.headers
            .push(("Content-Type".into(), "application/json".into()));
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
    /// Per-attempt total timeout. Each retry attempt gets this budget; use
    /// [`Request::deadline`] to bound all attempts together.
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }
    /// Overall budget for the call: all attempts and the retry sleeps between
    /// them. When the next sleep would pass the deadline, the last outcome is
    /// returned instead of sleeping. The per-attempt timeout is unchanged.
    pub fn deadline(mut self, total: Duration) -> Self {
        self.deadline = Some(total);
        self
    }
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }
    /// Allow retrying POST/PATCH after HTTP-status failures (`true`) or not (`false`, the default).
    pub fn retry_unsafe_statuses(mut self, allow: bool) -> Self {
        self.retry_unsafe_statuses = allow;
        self
    }
    /// Per-request response body limit, overriding `ClientConfig::max_response_bytes`.
    pub fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = Some(limit);
        self
    }
    /// Return only the status and headers: `send`, `send_any` and `Client::send_headers` do
    /// not read the response body. It is dropped unread, which closes that connection.
    /// Any status is returned by `send_any` and `send_headers`, including 3xx when redirects
    /// are off and 4xx/5xx after retries; `send` still turns non-2xx into an error.
    pub fn headers_only(mut self) -> Self {
        self.headers_only = true;
        self
    }
    /// Cancel this call through `token`. Checked between attempts, during retry
    /// sleeps and before every body read; a cancelled call returns [`HttpError::Cancelled`].
    pub fn cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }
}

/// Percent-encode for `application/x-www-form-urlencoded` / URL query values.
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
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

#[derive(Clone)]
pub struct Response {
    pub status: u16,
    /// Lower-cased header names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub attempts: u32,
    /// Secrets to scrub from `Debug` output: the client's `redact_values` and the
    /// request's secret values and URL secrets.
    pub(crate) redact: Vec<String>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        header_of(&self.headers, name)
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    /// Decode the body. The error names the failure category and position only: serde's
    /// own text can echo fragments of the body, which may be secret.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, HttpError> {
        serde_json::from_slice(&self.body).map_err(json_error)
    }
}

/// A [`HttpError::Json`] that never quotes the input.
pub(crate) fn json_error(e: serde_json::Error) -> HttpError {
    HttpError::Json(format!(
        "{:?} error at line {} column {}",
        e.classify(),
        e.line(),
        e.column()
    ))
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &redact_headers_with(&self.headers, &self.redact))
            .field("body_len", &self.body.len())
            .field("attempts", &self.attempts)
            .finish()
    }
}

/// Status line and headers of a response whose body was not read; see
/// [`Client::send_headers`].
#[derive(Clone)]
pub struct ResponseHead {
    pub status: u16,
    /// Lower-cased header names.
    pub headers: Vec<(String, String)>,
    /// URL of the response after the redirects that were followed. It is the caller's own
    /// URL data and keeps any query string; `Debug` redacts it.
    pub final_url: String,
    /// Redirects followed to reach `final_url`.
    pub hops: u32,
    /// HTTP requests made, counting retries (not redirect hops).
    pub attempts: u32,
    /// Secrets to scrub from `Debug` output; see [`Response`].
    pub(crate) redact: Vec<String>,
}

impl ResponseHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        header_of(&self.headers, name)
    }
}

impl fmt::Debug for ResponseHead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseHead")
            .field("status", &self.status)
            .field("headers", &redact_headers_with(&self.headers, &self.redact))
            .field(
                "final_url",
                &redact_str(&redact_url(&self.final_url), &self.redact),
            )
            .field("hops", &self.hops)
            .field("attempts", &self.attempts)
            .finish()
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

pub(crate) fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    pub(crate) config: ClientConfig,
    /// The proxy in use, for the `NO_PROXY` decision and destination vetting.
    proxy: Option<ureq::Proxy>,
    proxy_diagnostic: Option<ProxyDiagnostic>,
    /// Set when the proxy settings were invalid; every request then fails closed.
    config_error: Option<String>,
    /// The invalid setting is a system proxy configuration this client cannot follow:
    /// requests fail with the typed [`HttpError::SystemProxyUnsupported`] naming this
    /// element, instead of a generic message.
    system_proxy_unsupported: Option<String>,
}

pub(crate) struct Raw {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: ureq::Body,
    pub(crate) attempts: u32,
    /// URL of this response after followed redirects.
    pub(crate) final_url: String,
    /// Redirects followed.
    pub(crate) hops: u32,
    /// The response head as read off the wire before ureq could decode the body and strip
    /// `Content-Encoding` (see `wire.rs`). `Unobserved` is not the same as "no encoding".
    pub(crate) wire_head: crate::wire::WireHead,
}

impl Client {
    /// Build a client. Invalid proxy settings do not panic: every request then returns
    /// [`HttpError::InvalidRequest`]. Use [`Client::try_new`] to see the error up front.
    pub fn new(config: ClientConfig) -> Self {
        match Self::try_new(config.clone()) {
            Ok(client) => client,
            Err(e) => {
                let mut direct = config;
                direct.proxy = ProxyMode::None;
                let mut client = Self::build(direct, None);
                if let HttpError::SystemProxyUnsupported { element } = &e {
                    client.system_proxy_unsupported = Some(element.clone());
                }
                client.config_error = Some(e.to_string());
                client
            }
        }
    }

    /// Build a client, failing when the proxy settings are invalid.
    pub fn try_new(config: ClientConfig) -> Result<Self, HttpError> {
        let proxy = config.resolved_proxy()?;
        Ok(Self::build(config, proxy))
    }

    fn build(config: ClientConfig, proxy: Option<ResolvedProxy>) -> Self {
        // Redirects are followed by this crate, hop by hop, so the address policy and
        // header stripping run on every hop. ureq must never follow one itself.
        let builder = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(config.user_agent.clone())
            .timeout_connect(config.connect_timeout)
            .timeout_resolve(config.connect_timeout)
            .tls_config(blocking_tls_config(config.trust));
        let diagnostic = proxy.as_ref().map(ProxyDiagnostic::of);
        let (builder, endpoint, proxy) = match proxy {
            Some(p) => (
                builder.proxy(Some(p.ureq.clone())),
                Some((p.host, p.port)),
                Some(p.ureq),
            ),
            None => (builder.proxy(None), None, None),
        };
        // The vetting resolver returns only checked addresses, and ureq's TCP connector
        // connects to exactly those (`ConnectionDetails::addrs`). Destinations that go
        // through a proxy are vetted by name and literal in `once_hop` instead.
        let agent = ureq::Agent::with_parts(
            builder.build(),
            crate::wire::SniffConnector(ureq::unversioned::transport::DefaultConnector::default()),
            VettingResolver {
                policy: Arc::new(config.address_policy.clone()),
                proxy: endpoint,
                lookup_limit: config.connect_timeout.unwrap_or(Duration::from_secs(10)),
            },
        );
        Self {
            agent,
            config,
            proxy,
            proxy_diagnostic: diagnostic,
            config_error: None,
            system_proxy_unsupported: None,
        }
    }

    /// Which certificate verifier this client was built with: `"platform"` (the OS verifier)
    /// or `"bundled"` (Mozilla roots). It follows [`ClientConfig::trust`].
    pub fn verifier_kind(&self) -> &'static str {
        self.config.trust.verifier_kind()
    }

    /// The proxy this client uses, if any, without credentials. See [`ProxyDiagnostic`].
    pub fn proxy_diagnostic(&self) -> Option<&ProxyDiagnostic> {
        self.proxy_diagnostic.as_ref()
    }

    /// Client that may reach only `hosts` (exact match), only over HTTPS; see
    /// [`ClientConfig::first_party`].
    pub fn first_party(hosts: Vec<String>) -> Self {
        Self::new(ClientConfig::first_party(hosts))
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
            let values = self.redaction_values(req);
            Err(scrub_error(
                status_error(resp.status, &resp.headers, resp.body, resp.attempts),
                &values,
            ))
        }
    }

    /// Buffered request returning any final status (retryable statuses are still retried).
    pub fn send_any(&self, req: &Request) -> Result<Response, HttpError> {
        let timeout = req.timeout.unwrap_or(self.config.request_timeout);
        let cancel = request_cancel(req);
        let mut raw = self.execute(req, Some(timeout), cancel)?;
        let values = self.redaction_values(req);
        if req.headers_only {
            // Dropping the body unread closes the connection.
            return Ok(Response {
                status: raw.status,
                headers: raw.headers,
                body: Vec::new(),
                attempts: raw.attempts,
                redact: values,
            });
        }
        let limit = req
            .max_response_bytes
            .unwrap_or(self.config.max_response_bytes);
        let mut body_reader = raw.body.as_reader();
        let mut reader = CheckedReader {
            inner: &mut body_reader,
            cancel,
        };
        let body = read_limited(&mut reader, limit).map_err(|e| scrub_error(e, &values))?;
        Ok(Response {
            status: raw.status,
            headers: raw.headers,
            body,
            attempts: raw.attempts,
            redact: values,
        })
    }

    /// Status and headers only, for any status, without reading the body. Redirects, retries,
    /// the address policy and cancellation behave as for [`Client::send_any`]; the body is
    /// dropped unread, which closes that connection. `final_url` and `hops` report where
    /// redirects led.
    pub fn send_headers(&self, req: &Request) -> Result<ResponseHead, HttpError> {
        let timeout = req.timeout.unwrap_or(self.config.request_timeout);
        let raw = self.execute(req, Some(timeout), request_cancel(req))?;
        let Raw {
            status,
            headers,
            body,
            attempts,
            final_url,
            hops,
            ..
        } = raw;
        drop(body);
        Ok(ResponseHead {
            status,
            headers,
            final_url,
            hops,
            attempts,
            redact: self.redaction_values(req),
        })
    }

    /// Streaming request returning any final status with its headers and streaming body
    /// (retryable statuses are still retried per policy). The caller inspects `status` and
    /// `headers` (for example `401` plus `www-authenticate`) and bounds the body. Timeout and
    /// cancellation behave as for [`Client::stream`].
    pub fn stream_any(&self, req: &Request) -> Result<StreamResponse, HttpError> {
        let total = req.timeout.or(self.config.stream_timeout);
        let raw = self.execute(req, total, request_cancel(req))?;
        let values = self.redaction_values(req);
        Ok(Self::wrap_stream(raw, req, values))
    }

    fn wrap_stream(raw: Raw, req: &Request, values: Vec<String>) -> StreamResponse {
        let guarded = GuardReader {
            inner: Box::new(raw.body.into_reader()),
            cancel: req.cancel.clone(),
            values,
        };
        StreamResponse {
            status: raw.status,
            headers: raw.headers,
            attempts: raw.attempts,
            reader: Box::new(guarded),
        }
    }

    /// Streaming request; non-2xx becomes [`HttpError::Status`] (use [`Client::stream_any`]
    /// to receive such a response with its headers). Retries happen
    /// only before the response headers arrive (never mid-stream). Uses
    /// `Request::timeout` as the total stream budget when set, otherwise
    /// `ClientConfig::stream_timeout`.
    pub fn stream(&self, req: &Request) -> Result<StreamResponse, HttpError> {
        let cancel = request_cancel(req);
        let total = req.timeout.or(self.config.stream_timeout);
        let mut raw = self.execute(req, total, cancel)?;
        let values = self.redaction_values(req);
        if !(200..300).contains(&raw.status) {
            let mut body_reader = raw.body.as_reader();
            let mut reader = CheckedReader {
                inner: &mut body_reader,
                cancel,
            };
            let body = read_limited(&mut reader, MAX_ERROR_BODY_BYTES).unwrap_or_default();
            if is_set(cancel) {
                return Err(HttpError::Cancelled);
            }
            return Err(scrub_error(
                status_error(raw.status, &raw.headers, body, raw.attempts),
                &values,
            ));
        }
        Ok(Self::wrap_stream(raw, req, values))
    }

    pub fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, HttpError> {
        let req = Request::get(url).header("Accept", "application/json");
        let values = self.redaction_values(&req);
        self.send(&req)?.json().map_err(|e| scrub_error(e, &values))
    }

    /// Configured secrets plus this request's secret values, longest first.
    pub(crate) fn redaction_values(&self, req: &Request) -> Vec<String> {
        redaction_values_for(&self.config, req)
    }

    /// Run the retry loop. `total` bounds each attempt; `cancel` is checked between
    /// attempts and during retry sleeps. Errors are scrubbed of secret values.
    pub(crate) fn execute(
        &self,
        req: &Request,
        total: Option<Duration>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Raw, HttpError> {
        let values = self.redaction_values(req);
        self.execute_inner(req, total, cancel, &values)
            .map_err(|e| scrub_error(e, &values))
    }

    fn execute_inner(
        &self,
        req: &Request,
        total: Option<Duration>,
        cancel: Option<&AtomicBool>,
        values: &[String],
    ) -> Result<Raw, HttpError> {
        let policy = req
            .retry
            .clone()
            .unwrap_or_else(|| self.config.retry.clone());
        let max = policy.max_attempts.max(1);
        let deadline = req.deadline.map(|d| Instant::now() + d);
        let mut attempt = 0u32;
        loop {
            if is_set(cancel) {
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
            let (reason, delay) = match self.once(req, attempt_total, values) {
                Ok(raw) => {
                    let retryable = policy.is_retryable_status(raw.status)
                        && (req.method.is_idempotent() || req.retry_unsafe_statuses);
                    let retry_after =
                        header_of(&raw.headers, "retry-after").and_then(parse_retry_after);
                    if !retryable || attempt >= max {
                        return Ok(Raw {
                            attempts: attempt,
                            ..raw
                        });
                    }
                    let delay = policy.delay(attempt, retry_after);
                    if deadline_passes(deadline, delay) {
                        return Ok(Raw {
                            attempts: attempt,
                            ..raw
                        });
                    }
                    // Drain so the connection is released.
                    let mut body = raw.body;
                    let mut reader = body.as_reader();
                    let mut checked = CheckedReader {
                        inner: &mut reader,
                        cancel,
                    };
                    let _ = read_limited(&mut checked, MAX_ERROR_BODY_BYTES);
                    if is_set(cancel) {
                        return Err(HttpError::Cancelled);
                    }
                    (format!("HTTP {}", raw.status), delay)
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
                    let delay = policy.delay(attempt, None);
                    if deadline_passes(deadline, delay) {
                        return Err(err);
                    }
                    (err.to_string(), delay)
                }
            };
            if let Some(obs) = &self.config.on_retry {
                obs(&RetryEvent {
                    url: redact_str(&redact_url(&req.url), values),
                    retry: attempt,
                    reason: redact_str(&redact_urls_in_text(&reason), values),
                    delay,
                });
            }
            // Never sleep past the whole-request deadline: the drain above can have used it up.
            // The next loop pass then reports the elapsed deadline instead of a late retry.
            let delay = match deadline {
                Some(dl) => delay.min(dl.saturating_duration_since(Instant::now())),
                None => delay,
            };
            if !sleep_cancellable(delay, cancel) {
                return Err(HttpError::Cancelled);
            }
        }
    }

    /// One attempt: the request and, up to `max_redirects`, its redirect hops. Every hop
    /// goes through the same address policy.
    fn once(
        &self,
        req: &Request,
        total: Option<Duration>,
        secrets: &[String],
    ) -> Result<Raw, HttpError> {
        let started = Instant::now();
        let mut method = req.method;
        let mut url = req.url.clone();
        let mut headers = req.headers.clone();
        let mut body = req.body.clone();
        let mut hops = 0u32;
        loop {
            let remaining = match total {
                None => None,
                Some(t) => {
                    let left = t.saturating_sub(started.elapsed());
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
            let raw = match self.once_hop(method, &url, &headers, &body, remaining, hops > 0) {
                Ok(raw) => raw,
                Err(e) if hops > 0 => return Err(e.into_redirect_phase()),
                Err(e) => return Err(e),
            };
            if self.config.max_redirects == 0 || !redirect::is_redirect_status(raw.status) {
                return Ok(Raw {
                    final_url: url,
                    hops,
                    ..raw
                });
            }
            let Some(location) = header_of(&raw.headers, "location") else {
                return Ok(Raw {
                    final_url: url,
                    hops,
                    ..raw
                });
            };
            let Some(next_method) = redirect::redirect_method(raw.status, method) else {
                return Ok(Raw {
                    final_url: url,
                    hops,
                    ..raw
                });
            };
            if hops >= self.config.max_redirects {
                if self.config.max_redirects_will_error {
                    return Err(HttpError::transport("too many redirects", Phase::Redirect));
                }
                return Ok(Raw {
                    final_url: url,
                    hops,
                    ..raw
                });
            }
            hops += 1;
            let next_url = redirect::resolve_location(&url, location).ok_or_else(|| {
                HttpError::InvalidRequest("redirect with an unusable Location".into())
            })?;
            if let (Some(from), Some(to), Some(host)) = (
                redirect::scheme_of(&url),
                redirect::scheme_of(&next_url),
                redirect::host_of(&next_url),
            ) {
                // `https_only` first: its error names the rule the caller set, where the
                // address policy's downgrade refusal (always on) would hide it.
                redirect::require_https(self.config.https_only, &to, &host, true)?;
                self.config
                    .address_policy
                    .check_redirect_scheme(&from, &to, &host)
                    .map_err(|d| HttpError::from(d).into_redirect_phase())?;
            }
            if redirect::origin(&url) != redirect::origin(&next_url) {
                redirect::strip_cross_origin(&mut headers, secrets);
            }
            if next_method != method {
                body.clear();
                redirect::strip_body_headers(&mut headers);
            }
            method = next_method;
            url = next_url;
            // `raw` (and its connection) is dropped here before the next hop.
        }
    }

    fn once_hop(
        &self,
        method: Method,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
        total: Option<Duration>,
        redirect_hop: bool,
    ) -> Result<Raw, HttpError> {
        let mut builder = ureq::http::Request::builder()
            .method(method.as_str())
            .uri(url);
        for (k, v) in headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        // ureq frames a `Vec` body, even an empty one, with `Content-Length: 0`, which GET,
        // HEAD and DELETE must not carry. `()` has no body and no body headers. POST, PUT and
        // PATCH always send their body, so an empty one keeps `Content-Length: 0`.
        let bodiless =
            body.is_empty() && !matches!(method, Method::Post | Method::Put | Method::Patch);
        if bodiless {
            let request = builder
                .body(())
                .map_err(|_| HttpError::InvalidRequest("request could not be built".into()))?;
            self.run_hop(request, total, redirect_hop)
        } else {
            let request = builder
                .body(body.to_vec())
                .map_err(|_| HttpError::InvalidRequest("request could not be built".into()))?;
            self.run_hop(request, total, redirect_hop)
        }
    }

    /// One hop with a built request: the policy checks, the proxy scope and the ureq call.
    fn run_hop<S: ureq::AsSendBody>(
        &self,
        request: ureq::http::Request<S>,
        total: Option<Duration>,
        redirect_hop: bool,
    ) -> Result<Raw, HttpError> {
        if let Some(element) = &self.system_proxy_unsupported {
            return Err(HttpError::SystemProxyUnsupported {
                element: element.clone(),
            });
        }
        if let Some(reason) = &self.config_error {
            return Err(HttpError::InvalidRequest(reason.clone()));
        }
        // Every hop, not only the first, so a path that skips the redirect loop still
        // cannot reach plain http.
        redirect::require_https(
            self.config.https_only,
            request.uri().scheme_str().unwrap_or(""),
            request.uri().host().unwrap_or(""),
            redirect_hop,
        )?;
        let via_proxy = self.vet_proxied_destination(request.uri())?;
        let _scope = ViaProxy::enter(via_proxy);
        let configured = self
            .agent
            .configure_request(request)
            .timeout_global(total)
            .timeout_recv_response(self.config.response_timeout)
            .timeout_recv_body(self.config.recv_body_timeout)
            .build();
        crate::wire::clear();
        let response = self.agent.run(configured).map_err(map_ureq_error)?;
        let wire_head = crate::wire::take_head();
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
            body: response.into_body(),
            attempts: 1,
            final_url: String::new(),
            hops: 0,
            wire_head,
        })
    }
}

impl Client {
    /// Apply the address policy to `url` without sending anything: used by downloads so a
    /// denied URL never touches the filesystem. Through a proxy only literals and names are
    /// checked; otherwise the name is resolved and every address vetted (a pre-check: the
    /// request itself resolves again and connects to its own vetted addresses).
    pub(crate) fn preflight_url(&self, url: &str) -> Result<(), HttpError> {
        if let Some(element) = &self.system_proxy_unsupported {
            return Err(HttpError::SystemProxyUnsupported {
                element: element.clone(),
            });
        }
        if let Some(reason) = &self.config_error {
            return Err(HttpError::InvalidRequest(reason.clone()));
        }
        let uri: ureq::http::Uri = url
            .parse()
            .map_err(|_| HttpError::InvalidRequest("invalid URL".into()))?;
        let scheme = uri.scheme_str().unwrap_or("").to_ascii_lowercase();
        let raw_host = uri.host().unwrap_or("");
        redirect::require_https(self.config.https_only, &scheme, raw_host, false)?;
        let host = normalize_host(raw_host);
        let policy = &self.config.address_policy;
        policy.admit(&scheme, raw_host)?;
        if self.vet_proxied_destination(&uri)? {
            return Ok(());
        }
        let port = uri
            .port_u16()
            .unwrap_or(if scheme == "http" { 80 } else { 443 });
        policy.resolve_vetted(&host, port).map(|_| ())
    }

    /// Through a proxy the destination's address cannot be checked, so vet its name and
    /// literal form first. Hosts that bypass the proxy (`NO_PROXY`) are vetted by the
    /// resolver like direct connections.
    /// Returns whether the request goes through the proxy.
    fn vet_proxied_destination(&self, uri: &ureq::http::Uri) -> Result<bool, HttpError> {
        let Some(proxy) = &self.proxy else {
            return Ok(false);
        };
        if proxy.is_no_proxy(uri) {
            return Ok(false);
        }
        let policy = &self.config.address_policy;
        let scheme = uri.scheme_str().unwrap_or("").to_ascii_lowercase();
        let raw_host = uri.host().unwrap_or("");
        policy.admit(&scheme, raw_host)?;
        policy.vet_via_proxy(raw_host)?;
        Ok(true)
    }
}

fn sorted_values(values: &[String]) -> Vec<String> {
    let mut v: Vec<String> = values.iter().filter(|v| !v.is_empty()).cloned().collect();
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v
}

/// Header list safe to print: credential-named headers (including cookies) are replaced,
/// URL-bearing headers lose their query and userinfo, and any `scheme://` text in other
/// values is redacted the same way.
pub(crate) fn redact_headers_for_debug(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            let shown = if redirect::is_secret_header_name(k) {
                REDACTED.to_string()
            } else if matches!(
                k.to_ascii_lowercase().as_str(),
                "location" | "content-location" | "refresh" | "link" | "referer"
            ) {
                redact_urls_in_text(&redact_url(v))
            } else {
                redact_urls_in_text(v)
            };
            (k.clone(), shown)
        })
        .collect()
}

/// [`redact_headers_for_debug`] plus every recorded secret value scrubbed from what is left.
pub(crate) fn redact_headers_with(
    headers: &[(String, String)],
    values: &[String],
) -> Vec<(String, String)> {
    redact_headers_for_debug(headers)
        .into_iter()
        .map(|(k, v)| (k, redact_str(&v, values)))
        .collect()
}

/// Configured secrets, the request's secret values, and the query and userinfo of its URL,
/// longest first. Every error text and retry event is scrubbed of these.
pub(crate) fn redaction_values_for(config: &ClientConfig, req: &Request) -> Vec<String> {
    let mut values: Vec<String> = config
        .redact_values
        .iter()
        .chain(req.secret_values.iter())
        .filter(|v| !v.is_empty())
        .cloned()
        .chain(url_secret_parts(&req.url))
        .collect();
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    values.dedup();
    values
}

fn request_cancel(req: &Request) -> Option<&AtomicBool> {
    req.cancel.as_ref().map(CancelToken::as_atomic)
}

fn deadline_passes(deadline: Option<Instant>, delay: Duration) -> bool {
    deadline.is_some_and(|dl| Instant::now() + delay >= dl)
}

/// Reader for stream bodies: honours cancellation per read and scrubs secret
/// values from I/O error text.
struct GuardReader {
    inner: Box<dyn Read + Send>,
    cancel: Option<CancelToken>,
    values: Vec<String>,
}

impl Read for GuardReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            return Err(crate::cancel::cancelled_io());
        }
        self.inner.read(buf).map_err(|e| {
            if is_cancelled_io(&e) {
                return e;
            }
            let text = e.to_string();
            let scrubbed = redact_str(&redact_urls_in_text(&text), &self.values);
            if scrubbed == text {
                e
            } else {
                io::Error::new(e.kind(), scrubbed)
            }
        })
    }
}

pub(crate) fn status_error(
    status: u16,
    headers: &[(String, String)],
    body: Vec<u8>,
    attempts: u32,
) -> HttpError {
    let mut body = body;
    body.truncate(MAX_ERROR_BODY_BYTES);
    HttpError::Status {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
        retry_after: header_of(headers, "retry-after").and_then(parse_retry_after),
        attempts,
    }
}

/// Replace every occurrence of each secret value with `[redacted]`.
pub(crate) fn redact_str(text: &str, values: &[String]) -> String {
    let mut out = text.to_string();
    for value in values {
        if !value.is_empty() && out.contains(value.as_str()) {
            out = out.replace(value.as_str(), REDACTED);
        }
    }
    out
}

/// Scrub secret values from the text of any error variant.
pub(crate) fn scrub_error(err: HttpError, values: &[String]) -> HttpError {
    // Free-text messages lose secret values and any URL query or userinfo. A `Status`
    // body is the server's response and only loses secret values.
    let r = |s: String| redact_str(&redact_urls_in_text(&s), values);
    match err {
        HttpError::InvalidRequest(m) => HttpError::InvalidRequest(r(m)),
        HttpError::Transport {
            message,
            connect_phase,
            phase,
        } => HttpError::Transport {
            message: r(message),
            connect_phase,
            phase,
        },
        HttpError::Timeout { message, phase } => HttpError::Timeout {
            message: r(message),
            phase,
        },
        HttpError::Status {
            status,
            body,
            retry_after,
            attempts,
        } => HttpError::Status {
            status,
            body: redact_str(&body, values),
            retry_after,
            attempts,
        },
        HttpError::BodyTooLarge { limit } => HttpError::BodyTooLarge { limit },
        HttpError::Json(m) => HttpError::Json(r(m)),
        HttpError::Stream(m) => HttpError::Stream(r(m)),
        HttpError::Cancelled => HttpError::Cancelled,
        HttpError::SystemProxyUnsupported { element } => HttpError::SystemProxyUnsupported {
            element: element.clone(),
        },
        HttpError::HttpsRequired {
            host,
            scheme,
            redirect,
        } => HttpError::HttpsRequired {
            host: r(host),
            scheme,
            redirect,
        },
        HttpError::AddressDenied {
            host,
            addr,
            class,
            reason,
        } => HttpError::AddressDenied {
            host: r(host),
            addr,
            class,
            reason: r(reason),
        },
    }
}

pub(crate) fn read_limited(reader: &mut dyn Read, limit: usize) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| {
            if is_cancelled_io(&e) {
                return HttpError::Cancelled;
            }
            match e.kind() {
                std::io::ErrorKind::TimedOut => HttpError::timeout(e.to_string(), Phase::Body),
                // Body readers wrap ureq errors (including phase timeouts) in
                // io::ErrorKind::Other. Unwrap them before applying our taxonomy.
                _ => map_ureq_error_in(e.into(), Phase::Body),
            }
        })?;
    if out.len() > limit {
        return Err(HttpError::BodyTooLarge { limit });
    }
    Ok(out)
}

/// Map a ureq error raised while connecting or waiting for headers.
pub(crate) fn map_ureq_error(err: ureq::Error) -> HttpError {
    map_ureq_error_in(err, Phase::Headers)
}

/// Phase of a ureq timeout, falling back to `default` for the global and per-call budgets.
fn timeout_phase(t: &ureq::Timeout, default: Phase) -> Phase {
    use ureq::Timeout as T;
    match t {
        T::Resolve | T::Connect => Phase::Connect,
        T::SendRequest | T::SendBody | T::RecvResponse => Phase::Headers,
        T::RecvBody => Phase::Body,
        _ => default,
    }
}

/// Map a ureq error. `default` is the phase for failures that name none (socket errors,
/// global timeouts): `Headers` before the response arrives, `Body` while reading it.
pub(crate) fn map_ureq_error_in(err: ureq::Error, default: Phase) -> HttpError {
    use ureq::Error as E;
    // The vetting resolver refused the destination.
    if let E::Io(io) = &err {
        if let Some(inner) = io.get_ref() {
            if let Some(denial) = denial_in_chain(inner) {
                return denial.into();
            }
        }
    }
    match err {
        E::Timeout(t) => HttpError::timeout(format!("{t:?}"), timeout_phase(&t, default)),
        E::TooManyRedirects => HttpError::transport("too many redirects", Phase::Redirect),
        E::HostNotFound => HttpError::transport("host not found", Phase::Connect),
        E::ConnectionFailed => HttpError::transport("connection failed", Phase::Connect),
        E::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            HttpError::transport(io.to_string(), Phase::Connect)
        }
        E::Io(io) => HttpError::transport(io.to_string(), default),
        E::BadUri(m) => HttpError::InvalidRequest(m),
        E::Http(e) => HttpError::InvalidRequest(e.to_string()),
        other => HttpError::transport(other.to_string(), default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ureq_parity_sets_only_the_ureq_fields() {
        let cfg = ClientConfig::legacy_ureq_parity("heardright/1.0");
        let base = ClientConfig::default();
        assert_eq!(cfg.user_agent, "heardright/1.0");
        assert!(matches!(cfg.proxy, ProxyMode::Environment));
        assert_eq!(cfg.trust, TrustRoots::Bundled);
        assert_eq!(cfg.max_redirects, 10);
        assert_eq!(cfg.retry, RetryPolicy::none());
        // Private-address resolution stays off: the policy is the public-web default.
        assert_eq!(
            format!("{:?}", cfg.address_policy),
            format!("{:?}", base.address_policy)
        );
        // Every other field keeps its default.
        assert_eq!(cfg.connect_timeout, base.connect_timeout);
        assert_eq!(cfg.request_timeout, base.request_timeout);
        assert_eq!(cfg.response_timeout, base.response_timeout);
        assert_eq!(cfg.recv_body_timeout, base.recv_body_timeout);
        assert_eq!(cfg.stream_timeout, base.stream_timeout);
        assert_eq!(cfg.max_redirects_will_error, base.max_redirects_will_error);
        assert_eq!(cfg.max_response_bytes, base.max_response_bytes);
        assert!(cfg.on_retry.is_none());
        assert!(cfg.redact_values.is_empty());
        assert_eq!(cfg.proxy_bypass_first_party, base.proxy_bypass_first_party);
        assert_eq!(cfg.https_only, base.https_only);
    }

    #[test]
    fn put_and_delete_set_their_methods() {
        assert_eq!(Request::put("http://x/").method, Method::Put);
        assert_eq!(Request::delete("http://x/").method, Method::Delete);
        assert!(Request::put("http://x/").body.is_empty());
    }

    #[test]
    fn set_header_replaces_case_insensitively_in_place() {
        let req = Request::post("http://x/")
            .header("X-A", "1")
            .header("Accept", "text/plain")
            .header("x-a", "2")
            .set_header("X-A", "3");
        assert_eq!(
            req.headers,
            vec![
                ("X-A".to_string(), "3".to_string()),
                ("Accept".to_string(), "text/plain".to_string()),
            ]
        );
        let req = req.set_header("content-type", "text/csv");
        assert_eq!(
            req.headers.last(),
            Some(&("content-type".to_string(), "text/csv".to_string()))
        );
    }

    #[test]
    fn redaction_replaces_values_and_longest_first_scrub() {
        let values = vec!["secret-token-123".to_string(), "secret".to_string()];
        assert_eq!(
            redact_str("bad secret-token-123 and secret", &values),
            "bad [redacted] and [redacted]"
        );
    }

    #[test]
    fn secret_header_hidden_in_debug_but_sent() {
        let req = Request::get("http://x/").secret_header("X-Key", "hunter2-value");
        assert!(req.headers.iter().any(|(_, v)| v == "hunter2-value"));
        let debug = format!("{req:?}");
        assert!(!debug.contains("hunter2-value"), "{debug}");
        assert!(debug.contains(REDACTED));
    }

    #[test]
    fn scrub_error_covers_text_variants() {
        let values = vec!["tok-9".to_string()];
        match scrub_error(
            HttpError::Status {
                status: 401,
                body: "invalid tok-9".into(),
                retry_after: None,
                attempts: 1,
            },
            &values,
        ) {
            HttpError::Status { body, .. } => assert_eq!(body, "invalid [redacted]"),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn uri(s: &str) -> ureq::http::Uri {
        s.parse().unwrap()
    }

    #[test]
    fn environment_proxy_uses_the_first_set_variable_and_skips_empty_ones() {
        let env = env_of(&[
            ("ALL_PROXY", ""),
            ("HTTPS_PROXY", "http://secure.proxy:3128"),
            ("HTTP_PROXY", "http://plain.proxy:8080"),
            ("NO_PROXY", "a.test, .b.test ,"),
        ]);
        let p = proxy_from_lookup(&env).unwrap().unwrap();
        assert_eq!(p.host, "secure.proxy");
        assert_eq!(p.port, 3128);
        assert_eq!(
            p.no_proxy,
            vec!["a.test".to_string(), ".b.test".to_string()]
        );
        assert!(p.ureq.is_no_proxy(&uri("http://a.test/")));
        assert!(p.ureq.is_no_proxy(&uri("http://x.b.test/")));
        assert!(!p.ureq.is_no_proxy(&uri("http://c.test/")));
    }

    #[test]
    fn environment_proxy_order_and_case() {
        let env = env_of(&[
            ("http_proxy", "http://lower.proxy:1"),
            ("ALL_PROXY", "http://all.proxy:2"),
        ]);
        assert_eq!(proxy_from_lookup(&env).unwrap().unwrap().host, "all.proxy");
        let env = env_of(&[("http_proxy", "http://lower.proxy:1")]);
        assert_eq!(
            proxy_from_lookup(&env).unwrap().unwrap().host,
            "lower.proxy"
        );
        let env = env_of(&[("no_proxy", "z.test"), ("https_proxy", "http://p.proxy:9")]);
        assert_eq!(
            proxy_from_lookup(&env).unwrap().unwrap().no_proxy,
            vec!["z.test".to_string()]
        );
    }

    #[test]
    fn environment_without_a_usable_proxy_is_none() {
        assert!(proxy_from_lookup(&env_of(&[])).unwrap().is_none());
        // An unsupported scheme is skipped in favour of the next variable.
        let env = env_of(&[
            ("ALL_PROXY", "ftp://nope.proxy:21"),
            ("HTTP_PROXY", "http://ok.proxy:80"),
        ]);
        assert_eq!(proxy_from_lookup(&env).unwrap().unwrap().host, "ok.proxy");
        assert!(
            proxy_from_lookup(&env_of(&[("ALL_PROXY", "ftp://nope.proxy:21")]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn explicit_proxy_lets_the_proxy_resolve_and_keeps_credentials() {
        let p = build_proxy("http://user:pass@Proxy.Corp:3128", &[]).unwrap();
        assert_eq!(p.host, "proxy.corp");
        assert_eq!(p.port, 3128);
        assert!(!p.ureq.resolve_target());
        assert_eq!(p.ureq.username(), Some("user"));
        assert_eq!(p.ureq.password(), Some("pass"));
        assert!(build_proxy("https://10.0.0.5:3128", &[]).is_ok());
        assert!(build_proxy("socks5://127.0.0.1:1080", &[]).is_err());
        assert!(build_proxy("ftp://x:1", &[]).is_err());
        assert!(build_proxy("not a url", &[]).is_err());
    }

    #[test]
    fn proxy_mode_debug_never_shows_credentials() {
        let shown = format!(
            "{:?}",
            ProxyMode::Explicit("http://user:pw-9@proxy:3128/x?k=v".into())
        );
        assert!(
            !shown.contains("pw-9") && !shown.contains("user:") && !shown.contains("k=v"),
            "{shown}"
        );
        assert_eq!(format!("{:?}", ProxyMode::None), "None");
        assert_eq!(format!("{:?}", ProxyMode::Environment), "Environment");
        assert_eq!(format!("{:?}", ProxyMode::System), "System");
    }

    #[test]
    fn system_settings_resolve_like_an_environment_proxy() {
        use crate::system_proxy::SystemProxySettings;
        let proxy = proxy_from_system(&SystemProxySettings {
            proxy: Some("http=web.corp:8080;https=web.corp:8080".into()),
            bypass: Some("*.corp.example;intranet".into()),
            ..SystemProxySettings::default()
        })
        .unwrap()
        .expect("a static system proxy resolves");
        assert_eq!((proxy.host.as_str(), proxy.port), ("web.corp", 8080));
        assert_eq!(
            proxy.no_proxy,
            vec![".corp.example".to_string(), "intranet".to_string()]
        );
        assert!(ProxyDiagnostic::of(&proxy).from_environment);
        // Nothing configured: the caller falls back to the environment.
        assert!(proxy_from_system(&SystemProxySettings::default())
            .unwrap()
            .is_none());
    }

    #[test]
    fn system_settings_this_client_cannot_follow_exactly_fail_closed() {
        use crate::system_proxy::SystemProxySettings;
        let static_proxy = |proxy: &str, bypass: Option<&str>| SystemProxySettings {
            proxy: Some(proxy.into()),
            bypass: bypass.map(str::to_string),
            ..SystemProxySettings::default()
        };
        // Nothing is approximated with one proxy: plain HTTP is never sent to the HTTPS
        // endpoint, nothing goes around the proxy, and no bypass entry is dropped while the
        // proxy stays in use (fifth read, finding 11).
        let rows = [
            (
                static_proxy("http=web.corp:8080;https=secure.corp:8443", None),
                "HTTP and HTTPS are given different proxy routes",
            ),
            (
                static_proxy("https=secure.corp:8443", None),
                "HTTP and HTTPS are given different proxy routes",
            ),
            (
                static_proxy("http=web.corp:8080", None),
                "HTTP and HTTPS are given different proxy routes",
            ),
            (
                static_proxy("web.corp:8080", Some("http://private.example")),
                "the bypass entry `http://private.example` cannot be followed exactly",
            ),
            (
                static_proxy("web.corp:8080", Some("*.corp.example;https://only.example")),
                "the bypass entry `https://only.example` cannot be followed exactly",
            ),
            (
                static_proxy("web.corp:8080", Some("<local>")),
                "the bypass entry `<local>` cannot be followed exactly",
            ),
            (
                static_proxy("web.corp:8080", Some("10.*")),
                "the bypass entry `10.*` cannot be followed exactly",
            ),
            (
                SystemProxySettings {
                    auto_config: true,
                    ..SystemProxySettings::default()
                },
                "a proxy auto-config script is set",
            ),
            (
                SystemProxySettings {
                    auto_detect: true,
                    ..SystemProxySettings::default()
                },
                "automatic proxy detection is on",
            ),
        ];
        for (settings, expected) in rows {
            let error = proxy_from_system(&settings)
                .err()
                .unwrap_or_else(|| panic!("{expected}: must be refused"));
            assert!(
                matches!(&error, HttpError::SystemProxyUnsupported { element } if element == expected),
                "{expected}: {error}"
            );
            assert_eq!(
                error.to_string(),
                format!(
                    "the Windows proxy settings cannot be followed exactly by this version \
                     ({expected}); nothing was sent"
                )
            );
            assert!(matches!(error.class(), crate::error::ErrorClass::Invalid));
        }
    }

    #[test]
    fn first_party_bypass_drops_the_proxy_only_when_asked() {
        let base = ClientConfig::first_party(vec!["api.example.com".into()])
            .proxy(ProxyMode::Explicit("http://proxy:3128".into()));
        assert!(base.resolved_proxy().unwrap().is_some());
        assert!(base
            .clone()
            .proxy_bypass_first_party(true)
            .resolved_proxy()
            .unwrap()
            .is_none());
        // The flag means nothing for a public-web client.
        let public = ClientConfig::default()
            .proxy(ProxyMode::Explicit("http://proxy:3128".into()))
            .proxy_bypass_first_party(true);
        assert!(public.resolved_proxy().unwrap().is_some());
        assert!(ClientConfig::default().resolved_proxy().unwrap().is_none());
    }

    #[test]
    fn invalid_explicit_proxy_fails_closed() {
        let config = ClientConfig::default().proxy(ProxyMode::Explicit("ftp://nope:1".into()));
        assert!(matches!(
            Client::try_new(config.clone()),
            Err(HttpError::InvalidRequest(_))
        ));
        let err = Client::new(config)
            .send(&Request::get("http://127.0.0.1:9/"))
            .unwrap_err();
        assert!(matches!(err, HttpError::InvalidRequest(_)), "{err:?}");
    }
}
