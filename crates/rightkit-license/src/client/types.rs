//! Configuration and small value types of the native client.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use rightkit_credentials::Store;
use rightkit_http::{ClientConfig as HttpConfig, RetryPolicy};
use zeroize::Zeroizing;

use super::invalidation::LicenseInvalidation;
use super::lock::{MUTATION_WAIT, STATUS_WAIT};
use super::view::LicenseError;
use crate::LifecyclePolicy;

/// Where the per-install device identity comes from. The device adapter or the app supplies it,
/// which keeps the dependency direction (`rightkit-device` depends on this crate, not the other
/// way round).
pub trait DeviceIdSource: Send + Sync {
    /// The stable per-install device id (what typed-key activation binds the token to).
    fn device_id(&self) -> Result<String, DeviceIdError>;
    /// Lowercase hex SHA-256 of the device id, the only form lease endpoints receive (lease
    /// tokens bind it). `rightkit_device::device_id_hash` computes it.
    fn device_id_hash(&self) -> Result<String, DeviceIdError>;
}

/// The device identity could not be produced. Carries no detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceIdError;

impl fmt::Display for DeviceIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("device identity unavailable")
    }
}

impl std::error::Error for DeviceIdError {}

/// A secret text (a licence key, a legacy record) that zeroes itself on drop and never prints.
/// Deliberately not `Clone`, `Display` or `Serialize`.
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// The value. Call at the point of use.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True when it holds nothing but whitespace.
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Native release intent for an app's existing durable pending-removal queue.
/// Never exposed through a view, renderer IPC, Debug or serialization.
pub enum PendingLicenseRelease {
    Activation(SecretString),
    Lease {
        lease_id: String,
        device_id_hash: String,
    },
}

/// Licensing mode of an app: what a [`LicenseClient`](super::LicenseClient) is built from.
pub enum LicenseSetup {
    /// No licence product or no provisioned key (a free app, a key that is `None`). `status`
    /// answers `mode: disabled`; every mutating operation is [`LicenseError::Unconfigured`] and
    /// uses no network.
    Disabled,
    /// A real public key and host compiled into the app.
    Configured(Box<LicenseClientConfig>),
}

impl LicenseSetup {
    /// A configured setup. The config is boxed inside the enum, so build it with this instead
    /// of naming the variant.
    pub fn configured(config: LicenseClientConfig) -> Self {
        Self::Configured(Box::new(config))
    }
}

impl fmt::Debug for LicenseSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("LicenseSetup::Disabled"),
            Self::Configured(config) => write!(f, "LicenseSetup::Configured({config:?})"),
        }
    }
}

/// Default account of the licence record in the app's credential namespace.
pub const DEFAULT_RECORD_ACCOUNT: &str = "license-record";

/// Static inputs of a configured client. The endpoints are fixed: HTTPS, the compiled-in host,
/// the paths of `@rightkit/license` 0.2.1 and the lease routes. There is no option for a URL.
#[derive(Clone)]
pub struct LicenseClientConfig {
    pub(crate) app: String,
    pub(crate) host: String,
    pub(crate) app_version: String,
    pub(crate) public_key: [u8; 32],
    pub(crate) policy: LifecyclePolicy,
    pub(crate) store: Store,
    pub(crate) account: String,
    pub(crate) license_dir: PathBuf,
    pub(crate) clock_path: PathBuf,
    pub(crate) credential_clock: Option<(Store, String)>,
    pub(crate) http: HttpConfig,
    pub(crate) mutation_wait: Duration,
    pub(crate) status_wait: Duration,
    pub(crate) persist_wait: Duration,
    /// Loopback origin for tests only (feature `client-testing`); `None` always in a shipped
    /// build, where every URL is `https://{host}/...`.
    pub(crate) origin_override: Option<String>,
    /// The change counter the client signals; the client makes its own when `None`.
    pub(crate) invalidation: Option<LicenseInvalidation>,
}

impl fmt::Debug for LicenseClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LicenseClientConfig")
            .field("app", &self.app)
            .field("host", &self.host)
            .field("account", &self.account)
            .field("license_dir", &self.license_dir)
            .finish_non_exhaustive()
    }
}

impl LicenseClientConfig {
    /// `app` is the appKey (sent as `app`, matched server-side), `host` the brand host such as
    /// `heardright.app` (no scheme, port or path), `public_key` the Ed25519 key compiled into
    /// the app, `store` the app's credential store, and `license_dir` the app's licence
    /// directory (`<app data>/license`; it holds the operation lock, the clock ledger and the
    /// rotation locks, and must be private to the user).
    ///
    /// `Unconfigured` for an empty or malformed app or host: a build with no provisioned key
    /// uses [`LicenseSetup::Disabled`] instead.
    pub fn new(
        app: impl Into<String>,
        host: impl Into<String>,
        app_version: impl Into<String>,
        public_key: [u8; 32],
        store: Store,
        license_dir: impl Into<PathBuf>,
    ) -> Result<Self, LicenseError> {
        let app = app.into();
        let host = host.into();
        if !valid_app(&app) || !valid_host(&host) || public_key == [0u8; 32] {
            return Err(LicenseError::Unconfigured);
        }
        let license_dir = license_dir.into();
        let timeout = Duration::from_secs(15);
        Ok(Self {
            http: default_http(&host, timeout),
            clock_path: license_dir.join("clock.json"),
            credential_clock: None,
            // The rotation lock files live beside the other licence locks, so every process of
            // the app (window, engine) derives the same directory from `license_dir`.
            store: store.with_rotation_lock_dir(license_dir.join("rotation")),
            app,
            host,
            app_version: app_version.into(),
            public_key,
            policy: LifecyclePolicy::default(),
            account: DEFAULT_RECORD_ACCOUNT.to_string(),
            license_dir,
            mutation_wait: MUTATION_WAIT,
            status_wait: STATUS_WAIT,
            persist_wait: Duration::from_secs(20),
            origin_override: None,
            invalidation: None,
        })
    }

    /// Signal `invalidation` after every operation that changes the stored record, so live engine
    /// verifiers re-read it (see [`LicenseInvalidation`]). Without this the client signals a
    /// counter of its own, reachable through `LicenseClient::invalidation`.
    pub fn with_invalidation(mut self, invalidation: LicenseInvalidation) -> Self {
        self.invalidation = Some(invalidation);
        self
    }

    /// The credential account of the record (default [`DEFAULT_RECORD_ACCOUNT`]).
    pub fn with_account(mut self, account: impl Into<String>) -> Self {
        self.account = account.into();
        self
    }

    /// Where the anti-rollback ledger lives (default `<license dir>/clock.json`). Point it at
    /// the app's existing ledger to keep its anchors across the move to this client.
    pub fn with_clock_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.clock_path = path.into();
        self
    }

    /// Preserve an existing native credential ledger, including its scalar/JSON codec.
    /// Every process must use identical licence directory, namespace & account.
    pub fn with_credential_clock(mut self, store: Store, account: impl Into<String>) -> Self {
        self.credential_clock = Some((
            store.with_rotation_lock_dir(self.license_dir.join("clock-rotation")),
            account.into(),
        ));
        self
    }

    /// Lifecycle policy (grace seconds, lifetime-survives-rollback). Default: hard expiry.
    pub fn with_policy(mut self, policy: LifecyclePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The whole-request deadline of one server call (default 15 s).
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.http.request_timeout = timeout;
        self.http.response_timeout = Some(timeout);
        self
    }

    /// How long a mutation waits for the operation lock (default 5 s) and a status or
    /// authorization read waits for it (default 250 ms).
    pub fn with_waits(mut self, mutation: Duration, status: Duration) -> Self {
        self.mutation_wait = mutation;
        self.status_wait = status;
        self
    }

    /// How long a mutation waits for its fenced write worker before it reports the write as
    /// unresolved (default 20 s). The worker keeps the operation lock until it has finished
    /// whatever this wait.
    pub fn with_persist_wait(mut self, wait: Duration) -> Self {
        self.persist_wait = wait;
        self
    }

    /// TEST ONLY: send every request to `origin` (for example `http://127.0.0.1:PORT`) through
    /// `http`, a client configuration that permits it. A shipped build never enables the
    /// `client-testing` feature, so it cannot reach any host but `https://{host}`.
    #[cfg(feature = "client-testing")]
    pub fn with_test_origin(mut self, origin: impl Into<String>, http: HttpConfig) -> Self {
        self.origin_override = Some(origin.into().trim_end_matches('/').to_string());
        let mut http = http;
        // The fixed policy still applies: no redirects, no blind retries, bounded bodies.
        http.max_redirects = 0;
        http.retry = RetryPolicy::none();
        http.max_response_bytes = 256 * 1024;
        self.http = http;
        self
    }

    /// The anti-rollback ledger path (for an engine that builds its own `AntiRollbackClock`).
    pub fn clock_path(&self) -> &std::path::Path {
        &self.clock_path
    }

    /// The licence directory.
    pub fn license_dir(&self) -> &std::path::Path {
        &self.license_dir
    }

    /// The credential store, with its rotation lock directory set.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The credential account of the record.
    pub fn account(&self) -> &str {
        &self.account
    }

    pub(crate) fn origin(&self) -> String {
        match &self.origin_override {
            Some(origin) => origin.clone(),
            None => format!("https://{}", self.host),
        }
    }
}

fn default_http(host: &str, timeout: Duration) -> HttpConfig {
    let mut config = HttpConfig::first_party(vec![host.to_string()]).https_only(true);
    config.user_agent = concat!("rightkit-license/", env!("CARGO_PKG_VERSION")).to_string();
    config.connect_timeout = Some(Duration::from_secs(10).min(timeout));
    config.request_timeout = timeout;
    config.response_timeout = Some(timeout);
    // Credentials are never replayed to another host, and an activation is not retried blindly.
    config.max_redirects = 0;
    config.max_response_bytes = 256 * 1024;
    config.retry = RetryPolicy::none();
    config
}

fn valid_app(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= 64
        && app
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// A bare DNS name with at least one dot: no scheme, port, path, userinfo or whitespace.
pub(crate) fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..")
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_must_be_bare_dns_names() {
        for good in ["heardright.app", "coderight.cc", "a-b.example.co"] {
            assert!(valid_host(good), "{good}");
        }
        for bad in [
            "",
            "localhost",
            "https://x.app",
            "x.app:443",
            "x.app/path",
            "user@x.app",
            ".x.app",
            "x.app.",
            "x..app",
            "X.app",
            "x app.com",
            "-x.app",
        ] {
            assert!(!valid_host(bad), "{bad}");
        }
    }

    #[test]
    fn app_keys_are_plain() {
        assert!(valid_app("heardright") && valid_app("post-right_2"));
        assert!(!valid_app("") && !valid_app("Heard") && !valid_app("a/b") && !valid_app("a b"));
    }

    #[test]
    fn a_secret_string_never_prints() {
        let secret = SecretString::new("SENTINEL_KEY_VALUE");
        assert!(!format!("{secret:?}").contains("SENTINEL"));
        assert_eq!(secret.expose(), "SENTINEL_KEY_VALUE");
        assert!(SecretString::new("  \n").is_blank());
    }
}
