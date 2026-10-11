//! Vetted HTTP for model downloads and grants.
//!
//! Every outbound request in this crate goes through `rightkit-http`: the
//! destination's addresses are resolved once, classified, and connected to only
//! when public, and every redirect hop is vetted the same way. A manifest or a
//! redirect can therefore never steer a download at loopback, a private network,
//! link-local or a cloud-metadata address.
//!
//! - [`http_client`]: public-web policy, environment proxies, the default for
//!   model downloads.
//! - [`ModelHttpOptions`] / [`http_client_with_options`]: proxy, user agent,
//!   trust roots, timeouts, retries, redirects and address policy.
//! - [`AsHttpClient`]: accepted by the download and `ensure_ready` entry points,
//!   so a [`ModelHttp`], a bare [`Client`] and a legacy `&ureq::Agent` all compile.
//!
//! Timeouts: connecting is bounded ([`ModelHttpOptions::connect_timeout`]), and
//! so is the wait for response headers ([`ModelHttpOptions::header_timeout`]).
//! The body has **no total deadline** unless the caller sets
//! [`ModelHttpOptions::body_timeout`] or the whole-download budget
//! [`ModelHttpOptions::total_timeout`]; a multi-gigabyte download on a slow link
//! is fine as long as bytes keep arriving. A stalled body is caught by the
//! per-read idle limit [`ModelHttpOptions::read_timeout`]. A failed attempt is
//! retried up to [`ModelHttpOptions::retries`] times.
//!
//! Development-only loopback allowance: see [`ModelHttpOptions::allow_loopback_host`]
//! and the `dev-worker-override` feature.

use std::borrow::Cow;
use std::io::{self, Read};
use std::sync::mpsc;
use std::time::Duration;

use rightkit_http::{AddressPolicy, ClientConfig, RetryPolicy};

pub use rightkit_http::{Client, HttpError, ProxyMode, TrustRoots};

/// Redirects followed by [`http_client`].
pub const DEFAULT_MAX_REDIRECTS: u32 = 10;
/// Automatic retries of a failed download attempt, after the first attempt: two, so three
/// attempts in all.
pub const DEFAULT_RETRIES: u32 = 2;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_HEADER_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// True when this build honours [`ModelHttpOptions::allow_loopback_host`]: it was built with the
/// non-default, development-only cargo feature `dev-worker-override`. A release build leaves
/// this `false`, and the loopback allowance is then ignored.
pub const DEV_WORKER_OVERRIDE_ENABLED: bool = cfg!(feature = "dev-worker-override");

/// One loopback host and port a development build may reach; see
/// [`ModelHttpOptions::allow_loopback_host`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopbackHost {
    /// A host name or IP literal: `127.0.0.1`, `localhost` or `::1` (brackets optional).
    pub host: String,
    pub port: u16,
}

/// Settings for model-download HTTP.
#[derive(Clone, Debug)]
pub struct ModelHttpOptions {
    /// Default [`ProxyMode::Environment`]: `HTTPS_PROXY`, `HTTP_PROXY`,
    /// `ALL_PROXY` and `NO_PROXY`. Destinations are still vetted by name and
    /// literal address; the proxy itself is trusted.
    pub proxy: ProxyMode,
    /// Full `User-Agent` value. When `None`, [`ModelHttpOptions::app`] decides.
    pub user_agent: Option<String>,
    /// Calling app as `(name, version)`. The user agent becomes
    /// `<name>/<version> rightkit-models/<crate version>`; without it, just
    /// `rightkit-models/<crate version>`.
    pub app: Option<(String, String)>,
    /// Connection phase limit. `None` disables it.
    pub connect_timeout: Option<Duration>,
    /// Longest wait for the next body bytes. `None` disables it.
    pub read_timeout: Option<Duration>,
    /// Total time for one body, from the response headers. `None` (default): no
    /// total deadline.
    pub body_timeout: Option<Duration>,
    /// Vetted redirects followed. Zero keeps a scoped capability URL on its origin.
    pub max_redirects: u32,
    /// Outbound address policy; default public-web.
    pub address_policy: AddressPolicy,
    /// Certificate roots that verify servers. Default [`TrustRoots::Platform`], the operating
    /// system's trust store. [`TrustRoots::Bundled`] uses the Mozilla root set compiled into
    /// `rightkit-http`, ignoring the machine's store (so a TLS-inspecting proxy's root is not
    /// trusted). `rightkit-http` offers no other source, so an app cannot supply its own roots
    /// here. Verification cannot be turned off. Applies to downloads, and to grant requests
    /// sent through this transport (`ensure_with_grant_via`).
    pub trust: TrustRoots,
    /// Longest wait for the response headers (the first byte of a download). `None` disables
    /// it. Default 60 seconds.
    pub header_timeout: Option<Duration>,
    /// One budget for a whole download call: every attempt, the pauses between attempts and the
    /// body. When it runs out the download fails with a message that names the budget, and
    /// nothing is retried. It also bounds each attempt's body, so a slow transfer ends at the
    /// budget even while bytes keep arriving. `None` (default): no total budget.
    pub total_timeout: Option<Duration>,
    /// Automatic retries of a failed download attempt, after the first. Default
    /// [`DEFAULT_RETRIES`] (2, so three attempts in all). `0` makes one attempt. Only failures
    /// worth another try are retried (a transport failure, a stalled body, 408, 429, 5xx);
    /// the pause grows from 250 ms to 1 s.
    pub retries: u32,
    /// Development only. One loopback host and port that may be reached directly, for a local
    /// worker. Honoured only when built with the `dev-worker-override` cargo feature
    /// ([`DEV_WORKER_OVERRIDE_ENABLED`]); a release build ignores it. Set it with
    /// [`ModelHttpOptions::allow_loopback_host`].
    pub loopback_host: Option<LoopbackHost>,
}

impl Default for ModelHttpOptions {
    fn default() -> Self {
        Self {
            proxy: ProxyMode::Environment,
            user_agent: None,
            app: None,
            connect_timeout: Some(DEFAULT_CONNECT_TIMEOUT),
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
            body_timeout: None,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            address_policy: AddressPolicy::public_web(),
            trust: TrustRoots::Platform,
            header_timeout: Some(DEFAULT_HEADER_TIMEOUT),
            total_timeout: None,
            retries: DEFAULT_RETRIES,
            loopback_host: None,
        }
    }
}

impl ModelHttpOptions {
    /// The `User-Agent` these options send.
    pub fn resolved_user_agent(&self) -> String {
        let own = concat!("rightkit-models/", env!("CARGO_PKG_VERSION"));
        match (&self.user_agent, &self.app) {
            (Some(agent), _) => agent.clone(),
            (None, Some((name, version))) => format!("{name}/{version} {own}"),
            (None, None) => own.to_string(),
        }
    }

    /// Development only: let this transport reach exactly one loopback `host` on exactly one
    /// `port`, for a debug worker override (for example `127.0.0.1:8787`).
    ///
    /// This is honoured only when `rightkit-models` is built with the non-default cargo
    /// feature `dev-worker-override` ([`DEV_WORKER_OVERRIDE_ENABLED`]). Without it the call is
    /// accepted and does nothing: loopback stays refused, so a release build cannot enable
    /// the allowance by configuration. Do not enable the feature in a release build.
    ///
    /// Narrow by construction:
    ///
    /// - A request whose URL names exactly this host and port is sent by a separate client
    ///   that has no proxy, that never follows a redirect, and that may reach loopback
    ///   addresses for this host only. Any other URL, including this host on another port,
    ///   goes through the normal client, where loopback stays refused.
    /// - The normal client has no allowance, so a redirect to this host and port from any
    ///   other server is refused like any loopback redirect. A redirect answered by the
    ///   allowed host is not followed: the 3xx response fails the download.
    ///
    /// `host` is a name or an IP literal (`localhost`, `127.0.0.1`, `[::1]`); anything else is
    /// refused by [`ModelHttpOptions::validate`] and ignored by [`ModelHttp::new`].
    pub fn allow_loopback_host(mut self, host: impl Into<String>, port: u16) -> Self {
        self.loopback_host = Some(LoopbackHost {
            host: host.into(),
            port,
        });
        self
    }

    /// Check the values that become header text or an allowance: the user agent (the value
    /// from [`ModelHttpOptions::resolved_user_agent`]) must be a valid header value, and
    /// [`ModelHttpOptions::loopback_host`], when set, must be a plain host name or IP literal
    /// with a non-zero port. [`ModelHttp::try_new`] reports the error; [`ModelHttp::new`]
    /// falls back to the default user agent and drops an invalid allowance.
    pub fn validate(&self) -> Result<(), HttpError> {
        check_header_value("user agent", &self.resolved_user_agent())?;
        if let Some(loopback) = &self.loopback_host {
            if loopback.port == 0 {
                return Err(HttpError::InvalidRequest(
                    "loopback host port must not be 0".into(),
                ));
            }
            if normalize_loopback_host(&loopback.host).is_none() {
                return Err(HttpError::InvalidRequest(
                    "loopback host must be a plain host name or IP literal".into(),
                ));
            }
        }
        Ok(())
    }

    /// These options with every value `validate` would refuse replaced by its default.
    fn sanitized(mut self) -> Self {
        if let Some(agent) = &self.user_agent {
            if check_header_value("user agent", agent).is_err() {
                self.user_agent = None;
            }
        }
        if check_header_value("user agent", &self.resolved_user_agent()).is_err() {
            self.app = None;
        }
        if self.validate().is_err() {
            self.loopback_host = None;
        }
        self
    }
}

/// A header value this crate will send: not empty, no leading or trailing space, and only
/// printable ASCII (space to `~`), so no control character, line break or non-ASCII byte can
/// reach the wire. The value itself is not echoed in the error.
fn check_header_value(what: &str, value: &str) -> Result<(), HttpError> {
    if value.trim().is_empty() {
        return Err(HttpError::InvalidRequest(format!(
            "{what} must not be empty"
        )));
    }
    if value != value.trim() {
        return Err(HttpError::InvalidRequest(format!(
            "{what} must not start or end with whitespace"
        )));
    }
    if let Some(bad) = value.chars().find(|c| !('\u{20}'..='\u{7e}').contains(c)) {
        return Err(HttpError::InvalidRequest(format!(
            "{what} is not a valid header value (character U+{:04X})",
            u32::from(bad)
        )));
    }
    Ok(())
}

/// The lowercase host of an allowance: brackets removed, one trailing dot removed. `None`
/// unless it is a name of letters, digits, `-` and `.`, or an IPv6 literal.
fn normalize_loopback_host(host: &str) -> Option<String> {
    let trimmed = host.trim();
    let inner = match trimmed.strip_prefix('[') {
        Some(rest) => rest.strip_suffix(']')?,
        None => trimmed,
    };
    if inner.contains(':') {
        return inner
            .parse::<std::net::Ipv6Addr>()
            .ok()
            .map(|address| address.to_string());
    }
    let inner = inner.strip_suffix('.').unwrap_or(inner);
    if inner.is_empty()
        || !inner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
    {
        return None;
    }
    Some(inner.to_ascii_lowercase())
}

/// Host (lowercase, no brackets, no trailing dot) and port of an `http` or `https` URL's
/// authority. `None` for any other scheme, for userinfo, and for an unparsable port.
#[cfg_attr(not(any(test, feature = "dev-worker-override")), allow(dead_code))]
fn url_host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default_port = if scheme.eq_ignore_ascii_case("http") {
        80
    } else if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (host, after) = v6.split_once(']')?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse::<u16>().ok()?,
                None if after.is_empty() => default_port,
                None => return None,
            };
            (host, port)
        }
        None => match authority.rsplit_once(':') {
            Some((host, p)) if !host.contains(':') => (host, p.parse::<u16>().ok()?),
            Some(_) => return None,
            None => (authority, default_port),
        },
    };
    Some((normalize_loopback_host(host)?, port))
}

/// A vetted client plus what a download adds to it: the per-read idle limit, the retry count
/// and the optional total budget.
#[derive(Clone)]
pub struct ModelHttp {
    client: Client,
    read_timeout: Option<Duration>,
    retries: u32,
    total_timeout: Option<Duration>,
    #[cfg(feature = "dev-worker-override")]
    dev_loopback: Option<DevLoopback>,
}

/// The separate client for the one allowed loopback host and port (feature
/// `dev-worker-override`): no proxy, no redirects, loopback reachable for that host only.
#[cfg(feature = "dev-worker-override")]
#[derive(Clone)]
struct DevLoopback {
    host: String,
    port: u16,
    client: Client,
}

#[cfg(feature = "dev-worker-override")]
impl DevLoopback {
    fn new(config: &ClientConfig, options: &ModelHttpOptions) -> Option<Self> {
        let allowed = options.loopback_host.as_ref()?;
        let host = normalize_loopback_host(&allowed.host)?;
        let mut dev = config.clone();
        dev.address_policy = dev
            .address_policy
            .clone()
            .allow_if(vec![host.clone()], |ip| ip.is_loopback());
        // A proxy would resolve the name itself, so the loopback check could not apply; and a
        // redirect must never ride on this allowance.
        dev.proxy = ProxyMode::None;
        dev.max_redirects = 0;
        Some(Self {
            host,
            port: allowed.port,
            client: Client::new(dev),
        })
    }

    fn matches(&self, url: &str) -> bool {
        url_host_port(url).is_some_and(|(host, port)| host == self.host && port == self.port)
    }
}

impl ModelHttp {
    /// Build the transport from `options`. Never fails: a user agent that is not a valid
    /// header value is replaced by the default (`rightkit-models/<version>`), and an invalid
    /// loopback allowance is dropped. Use [`ModelHttp::try_new`] to see those errors instead.
    pub fn new(options: ModelHttpOptions) -> Self {
        let options = if options.validate().is_ok() {
            options
        } else {
            options.sanitized()
        };
        Self::build(options)
    }

    /// As [`ModelHttp::new`], but an invalid user agent or loopback allowance is an error
    /// ([`ModelHttpOptions::validate`]) and no transport is built.
    pub fn try_new(options: ModelHttpOptions) -> Result<Self, HttpError> {
        options.validate()?;
        Ok(Self::build(options))
    }

    fn build(options: ModelHttpOptions) -> Self {
        let mut config = ClientConfig::default()
            .address_policy(options.address_policy.clone())
            .proxy(options.proxy.clone())
            .trust(options.trust);
        config.user_agent = options.resolved_user_agent();
        config.max_redirects = options.max_redirects;
        config.connect_timeout = options.connect_timeout;
        config.response_timeout = options.header_timeout;
        config.recv_body_timeout = options.body_timeout;
        config.stream_timeout = None;
        // The download loop owns retry and resume state.
        config.retry = RetryPolicy::none();
        Self::assemble(
            Client::new(config.clone()),
            options.read_timeout,
            options.retries,
            options.total_timeout,
        )
        .with_dev_loopback(&config, &options)
    }

    fn assemble(
        client: Client,
        read_timeout: Option<Duration>,
        retries: u32,
        total_timeout: Option<Duration>,
    ) -> Self {
        Self {
            client,
            read_timeout,
            retries,
            total_timeout,
            #[cfg(feature = "dev-worker-override")]
            dev_loopback: None,
        }
    }

    #[cfg(feature = "dev-worker-override")]
    fn with_dev_loopback(mut self, config: &ClientConfig, options: &ModelHttpOptions) -> Self {
        self.dev_loopback = DevLoopback::new(config, options);
        self
    }

    #[cfg(not(feature = "dev-worker-override"))]
    fn with_dev_loopback(self, _config: &ClientConfig, _options: &ModelHttpOptions) -> Self {
        self
    }

    /// The vetted client. It refuses loopback. To send a request to a URL, use
    /// [`ModelHttp::client_for`], which also picks the development loopback client when that
    /// URL is the one allowed host and port.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The client that sends a request to `url`: the separate loopback client when the
    /// `dev-worker-override` feature is on and `url` names exactly the allowed host and port
    /// ([`ModelHttpOptions::allow_loopback_host`]); otherwise [`ModelHttp::client`]. Only the
    /// first URL of a request is matched, and the loopback client never follows a redirect.
    pub fn client_for(&self, url: &str) -> &Client {
        self.dev_client_for(url).unwrap_or(&self.client)
    }

    #[cfg(feature = "dev-worker-override")]
    fn dev_client_for(&self, url: &str) -> Option<&Client> {
        self.dev_loopback
            .as_ref()
            .filter(|dev| dev.matches(url))
            .map(|dev| &dev.client)
    }

    #[cfg(not(feature = "dev-worker-override"))]
    fn dev_client_for(&self, _url: &str) -> Option<&Client> {
        None
    }

    /// The loopback host and port this transport may reach: `Some` only when the
    /// `dev-worker-override` feature is on and a valid allowance was set. Always `None` in a
    /// release build.
    pub fn dev_loopback_host(&self) -> Option<(&str, u16)> {
        #[cfg(feature = "dev-worker-override")]
        {
            self.dev_loopback
                .as_ref()
                .map(|dev| (dev.host.as_str(), dev.port))
        }
        #[cfg(not(feature = "dev-worker-override"))]
        {
            None
        }
    }

    /// Per-read idle limit for bodies read through this transport.
    pub fn read_timeout(&self) -> Option<Duration> {
        self.read_timeout
    }

    /// Automatic retries of a failed download attempt, after the first
    /// ([`ModelHttpOptions::retries`]).
    pub fn retries(&self) -> u32 {
        self.retries
    }

    /// The budget for a whole download call, when one is set
    /// ([`ModelHttpOptions::total_timeout`]).
    pub fn total_timeout(&self) -> Option<Duration> {
        self.total_timeout
    }
}

impl Default for ModelHttp {
    fn default() -> Self {
        Self::new(ModelHttpOptions::default())
    }
}

impl From<Client> for ModelHttp {
    /// A caller-built client with the default idle limit, [`DEFAULT_RETRIES`] retries, no total
    /// budget and no loopback allowance. The client keeps its own timeouts and trust roots.
    fn from(client: Client) -> Self {
        Self::assemble(client, Some(DEFAULT_READ_TIMEOUT), DEFAULT_RETRIES, None)
    }
}

/// Shared transport for model downloads: public-web address policy,
/// environment proxies, [`DEFAULT_MAX_REDIRECTS`] vetted redirects.
pub fn http_client() -> ModelHttp {
    ModelHttp::default()
}

/// As [`http_client`] with an explicit redirect limit.
pub fn http_client_with_redirects(max_redirects: u32) -> ModelHttp {
    http_client_with_options(ModelHttpOptions {
        max_redirects,
        ..Default::default()
    })
}

/// As [`http_client_with_redirects`] under `policy`.
pub fn http_client_with_policy(max_redirects: u32, policy: AddressPolicy) -> ModelHttp {
    http_client_with_options(ModelHttpOptions {
        max_redirects,
        address_policy: policy,
        ..Default::default()
    })
}

pub fn http_client_with_options(options: ModelHttpOptions) -> ModelHttp {
    ModelHttp::new(options)
}

/// Anything the download entry points accept as their HTTP transport.
///
/// A legacy `ureq::Agent` is accepted for source compatibility only: its
/// transport is never used. The request goes through a fresh vetted
/// [`http_client`] that keeps just the agent's redirect limit, so a caller still
/// holding an agent cannot bypass the address policy.
pub trait AsHttpClient {
    fn vetted_client(&self) -> Cow<'_, ModelHttp>;
}

impl AsHttpClient for ModelHttp {
    fn vetted_client(&self) -> Cow<'_, ModelHttp> {
        Cow::Borrowed(self)
    }
}

impl AsHttpClient for Client {
    fn vetted_client(&self) -> Cow<'_, ModelHttp> {
        Cow::Owned(ModelHttp::from(self.clone()))
    }
}

impl AsHttpClient for ureq::Agent {
    fn vetted_client(&self) -> Cow<'_, ModelHttp> {
        Cow::Owned(http_client_with_redirects(self.config().max_redirects()))
    }
}

/// Reader that fails with `TimedOut` when no bytes arrive for `idle`.
///
/// The blocking read runs on a helper thread; on a stall the helper is
/// abandoned and ends when its read returns or errors. This is a stopgap for a
/// per-read limit that `rightkit-http` does not expose.
pub(crate) struct IdleReader {
    chunks: mpsc::Receiver<io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    at: usize,
    idle: Duration,
    done: bool,
}

impl IdleReader {
    pub(crate) fn new(mut inner: Box<dyn Read + Send>, idle: Duration) -> Self {
        let (sender, chunks) = mpsc::sync_channel(2);
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let item = match inner.read(&mut buffer) {
                    Ok(0) => Ok(Vec::new()),
                    Ok(n) => Ok(buffer[..n].to_vec()),
                    Err(error) => Err(error),
                };
                let last = !matches!(&item, Ok(chunk) if !chunk.is_empty());
                if sender.send(item).is_err() || last {
                    return;
                }
            }
        });
        Self {
            chunks,
            pending: Vec::new(),
            at: 0,
            idle,
            done: false,
        }
    }
}

impl Read for IdleReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.pending.len() {
            if self.done {
                return Ok(0);
            }
            match self.chunks.recv_timeout(self.idle) {
                Ok(Ok(chunk)) if chunk.is_empty() => {
                    self.done = true;
                    return Ok(0);
                }
                Ok(Ok(chunk)) => {
                    self.pending = chunk;
                    self.at = 0;
                }
                Ok(Err(error)) => {
                    self.done = true;
                    return Err(error);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.done = true;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("no data for {} ms", self.idle.as_millis()),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
        let n = out.len().min(self.pending.len() - self.at);
        out[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_values_accept_printable_ascii_only() {
        assert!(check_header_value("ua", "HeardRight/2.0.1 rightkit-models/0.3.0").is_ok());
        for bad in [
            "",
            "   ",
            " lead",
            "trail ",
            "a\r\nX-Injected: 1",
            "a\nb",
            "tab\there",
            "nul\0",
            "del\u{7f}",
            "caf\u{e9}",
        ] {
            assert!(check_header_value("ua", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_error_does_not_echo_the_value() {
        let error = check_header_value("user agent", "secret\r\nvalue").unwrap_err();
        assert!(!error.to_string().contains("secret"), "{error}");
    }

    #[test]
    fn loopback_hosts_are_plain_names_or_ip_literals() {
        assert_eq!(
            normalize_loopback_host("127.0.0.1").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(
            normalize_loopback_host("LocalHost.").as_deref(),
            Some("localhost")
        );
        assert_eq!(normalize_loopback_host("[::1]").as_deref(), Some("::1"));
        assert_eq!(normalize_loopback_host("::1").as_deref(), Some("::1"));
        for bad in [
            "",
            " ",
            "[",
            "[::1",
            "a/b",
            "a b",
            "u@host",
            "host:80/x",
            "host:80",
            "ex%61mple",
            "*",
        ] {
            assert_eq!(normalize_loopback_host(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn url_authority_is_parsed_with_default_ports() {
        let cases = [
            ("http://127.0.0.1:8787/v1/x", Some(("127.0.0.1", 8787))),
            ("HTTP://LocalHost:8787", Some(("localhost", 8787))),
            ("http://localhost/x", Some(("localhost", 80))),
            ("https://localhost/x?a=b", Some(("localhost", 443))),
            ("http://[::1]:9/p", Some(("::1", 9))),
            ("http://[::1]/p", Some(("::1", 80))),
            ("http://127.0.0.1.:8787/", Some(("127.0.0.1", 8787))),
            ("http://u:p@127.0.0.1:8787/", None),
            ("http://127.0.0.1:notaport/", None),
            ("http://127.0.0.1:99999/", None),
            ("ftp://127.0.0.1:21/", None),
            ("127.0.0.1:8787", None),
            ("http:///x", None),
            ("http://::1/x", None),
        ];
        for (url, want) in cases {
            assert_eq!(
                url_host_port(url),
                want.map(|(h, p)| (h.to_string(), p)),
                "{url}"
            );
        }
    }

    #[test]
    fn defaults_keep_todays_settings() {
        let options = ModelHttpOptions::default();
        assert_eq!(options.connect_timeout, Some(Duration::from_secs(30)));
        assert_eq!(options.header_timeout, Some(Duration::from_secs(60)));
        assert_eq!(options.read_timeout, Some(Duration::from_secs(120)));
        assert_eq!(options.total_timeout, None);
        assert_eq!(options.retries, 2);
        assert_eq!(options.trust, TrustRoots::Platform);
        assert_eq!(options.loopback_host, None);
    }

    #[test]
    fn a_wrapped_client_keeps_the_default_download_settings() {
        let http = ModelHttp::from(Client::new(ClientConfig::default()));
        assert_eq!(http.retries(), DEFAULT_RETRIES);
        assert_eq!(http.total_timeout(), None);
        assert_eq!(http.read_timeout(), Some(Duration::from_secs(120)));
        assert_eq!(http.dev_loopback_host(), None);
    }
}
