//! The headless-browser seam: a synchronous browser session, its configuration, and the factory
//! the host implements. The host owns the browser process (and its address pinning); this crate
//! only drives a session through [`HeadlessBrowser`].

use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// A host plus the vetted address the browser must use for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPin {
    host: String,
    addr: IpAddr,
}

impl HostPin {
    /// `host` must be a plain DNS name (ASCII letters, digits, `.`, `-`, `_`). Anything else is
    /// refused so a hostile value cannot add a second rule or another browser flag.
    pub fn new(host: &str, addr: IpAddr) -> Result<Self, String> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let valid = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if !valid {
            return Err(format!("cannot pin browser host {host:?}"));
        }
        Ok(Self { host, addr })
    }

    /// The pinned host name (lowercase, no trailing dot).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The vetted address.
    pub fn addr(&self) -> IpAddr {
        self.addr
    }
}

/// Headless vs headful selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeadMode {
    #[default]
    Headless,
    WithHead,
}

/// Configuration passed to the browser backend: profile, head mode, resource blocking policy,
/// and stealth overrides (user agent and extra headers).
#[derive(Debug, Clone, Default)]
pub struct BrowserConfig {
    pub profile_dir: Option<PathBuf>,
    pub head_mode: HeadMode,
    pub block_resources: bool,
    pub user_agent: Option<String>,
    pub extra_headers: Vec<(String, String)>,
    /// Vetted address for the page host: the browser resolves that host to this address.
    pub host_pin: Option<HostPin>,
}

/// One synchronous browser session. Every method takes `should_cancel` for cooperative
/// cancellation. `eval` returns the scalar result as text (a string as itself, anything else as
/// JSON).
pub trait HeadlessBrowser: Send + Sync {
    fn open(&self, url: &str, should_cancel: &dyn Fn() -> bool) -> Result<(), String>;
    fn wait_for_selector(
        &self,
        selector: &str,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<(), String>;
    fn click(&self, selector: &str, should_cancel: &dyn Fn() -> bool) -> Result<(), String>;
    fn screenshot(&self, path: &Path, should_cancel: &dyn Fn() -> bool) -> Result<(), String>;
    fn eval(&self, js: &str, should_cancel: &dyn Fn() -> bool) -> Result<String, String>;
    fn close(&self) -> Result<(), String>;
}

/// Creates browser sessions and vets the page host before one opens.
pub trait BrowserHost {
    /// A session for `config`. The browser starts on the first `open`.
    fn new_backend(&self, config: BrowserConfig) -> Box<dyn HeadlessBrowser>;
    /// Resolve and vet `url`'s host and answer the address the browser must use for it, or
    /// `None` for an IP literal (nothing to pin). A refused or unresolvable host is an error, so
    /// the browser is never launched against it.
    fn resolve_host_pin(&self, url: &str) -> Result<Option<HostPin>, String>;
}
