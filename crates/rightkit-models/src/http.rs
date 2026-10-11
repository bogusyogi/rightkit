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
//!   timeouts, redirects and address policy.
//! - [`AsHttpClient`]: accepted by the download and `ensure_ready` entry points,
//!   so a [`ModelHttp`], a bare [`Client`] and a legacy `&ureq::Agent` all compile.
//!
//! Timeouts: connecting is bounded ([`ModelHttpOptions::connect_timeout`]), and
//! so is the wait for response headers. The body has **no total deadline** unless
//! the caller sets [`ModelHttpOptions::body_timeout`]; a multi-gigabyte download
//! on a slow link is fine as long as bytes keep arriving. A stalled body is
//! caught by the per-read idle limit [`ModelHttpOptions::read_timeout`].

use std::borrow::Cow;
use std::io::{self, Read};
use std::sync::mpsc;
use std::time::Duration;

use rightkit_http::{AddressPolicy, ClientConfig, RetryPolicy};

pub use rightkit_http::{Client, ProxyMode};

/// Redirects followed by [`http_client`].
pub const DEFAULT_MAX_REDIRECTS: u32 = 10;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);

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
}

/// A vetted client plus the per-read idle limit applied to download bodies.
#[derive(Clone)]
pub struct ModelHttp {
    client: Client,
    read_timeout: Option<Duration>,
}

impl ModelHttp {
    pub fn new(options: ModelHttpOptions) -> Self {
        let mut config = ClientConfig::default()
            .address_policy(options.address_policy.clone())
            .proxy(options.proxy.clone());
        config.user_agent = options.resolved_user_agent();
        config.max_redirects = options.max_redirects;
        config.connect_timeout = options.connect_timeout;
        config.recv_body_timeout = options.body_timeout;
        config.stream_timeout = None;
        // The download loop owns retry and resume state.
        config.retry = RetryPolicy::none();
        Self {
            client: Client::new(config),
            read_timeout: options.read_timeout,
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Per-read idle limit for bodies read through this transport.
    pub fn read_timeout(&self) -> Option<Duration> {
        self.read_timeout
    }
}

impl Default for ModelHttp {
    fn default() -> Self {
        Self::new(ModelHttpOptions::default())
    }
}

impl From<Client> for ModelHttp {
    /// A caller-built client with the default idle limit.
    fn from(client: Client) -> Self {
        Self {
            client,
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
        }
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
