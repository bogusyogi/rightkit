//! Engine-side entitlement verification (Pro entitlement, M13).
//!
//! An engine process (sidecar, daemon, stdio helper, headless CLI) must decide Pro
//! access itself. It must never accept an entitlement *verdict* from its caller: a
//! boolean on a config push, a serialized snapshot on stdin, an environment variable or
//! a command-line flag is attacker-controlled input to a process that holds the
//! protected capability.
//!
//! [`EngineEntitlements`] therefore has no input for a verdict. It reads the signed
//! licence token from the app's own data root, authenticates it against the public key
//! compiled into the engine, and checks signature, app, device binding and expiry
//! against a [`TrustedClock`]. The device identity is derived by the engine through a
//! host-supplied source ([`EngineEntitlementsConfig::new`]), never received from the
//! window. A successful verification is cached for a bounded TTL
//! ([`MAX_CACHE_TTL_SECS`]); expiry is re-checked on every call even while cached.
//!
//! ## Window to engine pattern
//! 1. The window (UI process) obtains and stores the signed token, as it does today, in
//!    `<data_root>/<token_file>` (default [`DEFAULT_TOKEN_FILE`]), the same location the
//!    engine reads. The window is the only writer; the engine only reads.
//! 2. When the licence changes (activation, refresh, deactivation) the window may tell
//!    the engine "licence changed" through any channel. That message carries **no
//!    entitlement data**. The engine reacts by calling [`EngineEntitlements::invalidate`]
//!    and the next [`EngineEntitlements::require`] re-reads and re-verifies the file.
//!    A message that claims "you are Pro" is ignored: there is nowhere to put it.
//! 3. The engine calls [`EngineEntitlements::require`] (or [`require_pro`]) at the
//!    point of use of each gated capability, on every start path (window, stdio,
//!    headless, cross-app). The returned [`Entitlement`] is the verdict; do not copy it
//!    into a settings or config struct that another process can write.
//! 4. A test or debug override belongs behind a Cargo feature or `cfg(test)` that
//!    release builds cannot enable; do not branch on an environment variable.
//!
//! [`require_pro`]: EngineEntitlements::require_pro

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::{
    system_now_unix, ClockRefusal, Entitlement, LicenseRequirement, SystemClock, TrustedClock,
    VerifyError,
};

/// Default token file name under the app data root.
pub const DEFAULT_TOKEN_FILE: &str = "license.token";
/// Upper bound on the cache TTL. A larger configured value is clamped to this.
pub const MAX_CACHE_TTL_SECS: i64 = 300;
/// Default cache TTL.
pub const DEFAULT_CACHE_TTL_SECS: i64 = 60;
/// A token file larger than this is refused unread.
pub const MAX_TOKEN_BYTES: u64 = 16 * 1024;

/// Why an engine entitlement check refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEntitlementError {
    /// No token file in the data root (Free).
    NoLicense,
    /// The token file exists but could not be read, is empty, oversized or not UTF-8.
    Unreadable(String),
    /// The engine could not derive its own device identity.
    DeviceUnavailable(String),
    /// Signature, app, device, expiry or lifecycle check failed.
    Invalid(VerifyError),
    /// The trusted clock refused (rollback, missing ledger, storage failure).
    Clock(ClockRefusal),
    /// The licence is authentic and current but does not satisfy the requirement.
    NotEntitled,
}

impl fmt::Display for EngineEntitlementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLicense => write!(f, "no licence is stored"),
            Self::Unreadable(e) => write!(f, "licence file unreadable: {e}"),
            Self::DeviceUnavailable(e) => write!(f, "device identity unavailable: {e}"),
            Self::Invalid(e) => write!(f, "{e}"),
            Self::Clock(e) => write!(f, "trusted clock refused: {e}"),
            Self::NotEntitled => write!(f, "licence does not include this feature"),
        }
    }
}
impl std::error::Error for EngineEntitlementError {}

type DeviceSource = Box<dyn Fn() -> Result<String, String> + Send + Sync>;
type WallClock = Box<dyn Fn() -> Result<i64, String> + Send + Sync>;

/// Static inputs of an engine verifier.
pub struct EngineEntitlementsConfig {
    pubkey: [u8; 32],
    app: String,
    data_root: PathBuf,
    token_file: String,
    ttl_secs: i64,
    device: DeviceSource,
}

impl EngineEntitlementsConfig {
    /// `pubkey` is the licence public key compiled into the engine binary, `app` the
    /// appKey the token must be minted for, `data_root` the app data directory, and
    /// `device` the engine's own device-identity derivation (the same derivation the
    /// window uses to obtain the bound token, executed in the engine process).
    pub fn new(
        pubkey: [u8; 32],
        app: impl Into<String>,
        data_root: impl Into<PathBuf>,
        device: impl Fn() -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            pubkey,
            app: app.into(),
            data_root: data_root.into(),
            token_file: DEFAULT_TOKEN_FILE.to_string(),
            ttl_secs: DEFAULT_CACHE_TTL_SECS,
            device: Box::new(device),
        }
    }

    /// Token file name inside the data root. A name containing a path separator or `..`
    /// is replaced by [`DEFAULT_TOKEN_FILE`] so the read cannot leave the data root.
    pub fn with_token_file(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let plain =
            !name.is_empty() && name != ".." && name != "." && !name.contains(['/', '\\', '\0']);
        if plain {
            self.token_file = name;
        }
        self
    }

    /// Cache TTL in seconds, clamped to `0..=`[`MAX_CACHE_TTL_SECS`]. `0` disables caching.
    pub fn with_cache_ttl_secs(mut self, ttl: i64) -> Self {
        self.ttl_secs = ttl.clamp(0, MAX_CACHE_TTL_SECS);
        self
    }
}

struct Cached {
    entitlement: Entitlement,
    verified_at: i64,
}

/// Verifies the stored licence inside an engine process. See the module docs.
pub struct EngineEntitlements<C = SystemClock> {
    cfg: EngineEntitlementsConfig,
    clock: C,
    wall: WallClock,
    cache: Mutex<Option<Cached>>,
}

impl EngineEntitlements<SystemClock> {
    /// Wall-clock verifier. Prefer [`EngineEntitlements::with_clock`] with an
    /// [`AntiRollbackClock`](crate::AntiRollbackClock) where the engine can reach the
    /// app's clock ledger.
    pub fn new(cfg: EngineEntitlementsConfig) -> Self {
        Self::with_clock(cfg, SystemClock)
    }
}

impl<C: TrustedClock> EngineEntitlements<C> {
    pub fn with_clock(cfg: EngineEntitlementsConfig, clock: C) -> Self {
        Self {
            cfg,
            clock,
            wall: Box::new(system_now_unix),
            cache: Mutex::new(None),
        }
    }

    /// Replace the wall-clock source (tests).
    #[doc(hidden)]
    pub fn with_wall_clock(
        mut self,
        wall: impl Fn() -> Result<i64, String> + Send + Sync + 'static,
    ) -> Self {
        self.wall = Box::new(wall);
        self
    }

    /// Drop the cached verdict so the next check re-reads and re-verifies the file. This
    /// is the only thing a "licence changed" message from the window should do. It can
    /// only make the next check stricter or equal: it never grants anything.
    pub fn invalidate(&self) {
        if let Ok(mut guard) = self.cache.lock() {
            *guard = None;
        }
    }

    /// The verified entitlement, or why there is none.
    pub fn current(&self) -> Result<Entitlement, EngineEntitlementError> {
        let wall = (self.wall)()
            .map_err(|e| EngineEntitlementError::Clock(ClockRefusal::Unavailable(e)))?;
        let now = self
            .clock
            .trusted_now(wall, false)
            .map_err(EngineEntitlementError::Clock)?;
        if let Ok(guard) = self.cache.lock() {
            if let Some(c) = guard.as_ref() {
                let age = now.saturating_sub(c.verified_at);
                if self.cfg.ttl_secs > 0
                    && (0..self.cfg.ttl_secs).contains(&age)
                    && now < c.entitlement.exp
                {
                    return Ok(c.entitlement.clone());
                }
            }
        }
        let ent = self.verify_stored(now)?;
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(Cached {
                entitlement: ent.clone(),
                verified_at: now,
            });
        }
        Ok(ent)
    }

    /// Require a single feature name.
    pub fn require(&self, feature: &str) -> Result<Entitlement, EngineEntitlementError> {
        self.require_requirement(&LicenseRequirement::feature(feature))
    }

    /// Require the Pro tier.
    pub fn require_pro(&self) -> Result<Entitlement, EngineEntitlementError> {
        self.require_requirement(&LicenseRequirement::pro())
    }

    /// Require any [`LicenseRequirement`]. An invalid requirement fails closed.
    pub fn require_requirement(
        &self,
        requirement: &LicenseRequirement,
    ) -> Result<Entitlement, EngineEntitlementError> {
        let ent = self.current()?;
        if ent.satisfies(requirement) {
            Ok(ent)
        } else {
            Err(EngineEntitlementError::NotEntitled)
        }
    }

    fn verify_stored(&self, now: i64) -> Result<Entitlement, EngineEntitlementError> {
        let token = read_token(&self.cfg.data_root.join(&self.cfg.token_file))?;
        let device = (self.cfg.device)().map_err(EngineEntitlementError::DeviceUnavailable)?;
        if device.trim().is_empty() {
            return Err(EngineEntitlementError::DeviceUnavailable(
                "empty device identity".to_string(),
            ));
        }
        crate::verify_token(&token, &self.cfg.pubkey, &self.cfg.app, &device, now)
            .map_err(EngineEntitlementError::Invalid)
    }
}

fn read_token(path: &Path) -> Result<String, EngineEntitlementError> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(EngineEntitlementError::NoLicense)
        }
        Err(e) => return Err(EngineEntitlementError::Unreadable(e.to_string())),
    };
    let mut raw = String::new();
    file.take(MAX_TOKEN_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|e| EngineEntitlementError::Unreadable(e.to_string()))?;
    if raw.len() as u64 > MAX_TOKEN_BYTES {
        return Err(EngineEntitlementError::Unreadable(
            "token file too large".to_string(),
        ));
    }
    let token = raw.trim();
    if token.is_empty() {
        return Err(EngineEntitlementError::Unreadable(
            "empty token file".into(),
        ));
    }
    Ok(token.to_string())
}
