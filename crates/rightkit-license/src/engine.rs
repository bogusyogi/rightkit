//! Engine-side entitlement verification (Pro entitlement, M13).
//!
//! An engine process (sidecar, daemon, stdio helper, headless CLI) must decide Pro
//! access itself. It must never accept an entitlement *verdict* from its caller: a
//! boolean on a config push, a serialized snapshot on stdin, an environment variable or
//! a command-line flag is attacker-controlled input to a process that holds the
//! protected capability.
//!
//! [`EngineEntitlements`] therefore has no input for a verdict. It reads the signed
//! licence token from the app's own token source (the data-root token file by
//! default, or an opt-in credential-store source; see
//! [`EngineEntitlementsConfig::with_credential_reader`]), authenticates it against
//! the public key
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
//! ## The native client's record (M12)
//! A credential source may return the stored record text instead of a bare token: an envelope
//! (`state` `active` or `tombstone`) or the older bare `@rightkit/license` record. A tombstone
//! (a deactivated licence) is [`EngineEntitlementError::NoLicense`]; an old bare record is
//! active with no revision. With the `client` feature, `CredentialTokenSource::from_record_store`
//! also makes the engine an authorizer of the native client: each read first tries the per-app
//! licence operation lock for 250 ms and checks the rotation fence, and while the lock is held,
//! a rotation marker exists or a write is unresolved the check fails closed as
//! [`EngineEntitlementError::Unreadable`] (never `NoLicense`, never an entitlement). The cached
//! verdict below is still bounded by its TTL, so a deactivation reaches an engine at the latest
//! after [`MAX_CACHE_TTL_SECS`] or at the next [`EngineEntitlements::invalidate`]. With the `client`
//! feature an in-process engine can attach the client's `LicenseInvalidation`
//! (`EngineEntitlements::with_invalidation`), after which a verdict cached before the client's
//! latest change is never served, so a deactivation reaches it at once.
//!
//! [`require_pro`]: EngineEntitlements::require_pro

use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::{
    system_now_unix, verify_token_not_revoked, ClockRefusal, Entitlement, LicenseRequirement,
    RevocationError, RevokedIds, SystemClock, TrustedClock, VerifyError,
};

#[path = "credential_source.rs"]
pub(crate) mod credential_source;

pub use credential_source::{CredentialTokenSource, FileTokenSource, TokenSource};

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
#[non_exhaustive]
pub enum EngineEntitlementError {
    /// No token in the configured token source (Free).
    NoLicense,
    /// The token source failed, or its token is empty, oversized or not UTF-8.
    Unreadable(String),
    /// The engine could not derive its own device identity.
    DeviceUnavailable(String),
    /// Signature, app, device, expiry or lifecycle check failed.
    Invalid(VerifyError),
    /// The trusted clock refused (rollback, missing ledger, storage failure).
    Clock(ClockRefusal),
    /// The licence is authentic and current but does not satisfy the requirement.
    NotEntitled,
    /// The entitlement id is on the engine's incident denylist
    /// ([`EngineEntitlementsConfig::with_revoked_ids`]). Carries the revoked id.
    Revoked(String),
}

impl fmt::Display for EngineEntitlementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLicense => write!(f, "no licence is stored"),
            Self::Unreadable(e) => write!(f, "licence token unreadable: {e}"),
            Self::DeviceUnavailable(e) => write!(f, "device identity unavailable: {e}"),
            Self::Invalid(e) => write!(f, "{e}"),
            Self::Clock(e) => write!(f, "trusted clock refused: {e}"),
            Self::NotEntitled => write!(f, "licence does not include this feature"),
            Self::Revoked(id) => write!(f, "licence '{id}' has been revoked"),
        }
    }
}
impl std::error::Error for EngineEntitlementError {}

type DeviceSource = Box<dyn Fn() -> Result<String, String> + Send + Sync>;
type WallClock = Box<dyn Fn() -> Result<i64, String> + Send + Sync>;
type ClockFlag = Box<dyn Fn() -> bool + Send + Sync>;
type Generation = Box<dyn Fn() -> u64 + Send + Sync>;

/// Static inputs of an engine verifier.
pub struct EngineEntitlementsConfig {
    pubkey: [u8; 32],
    app: String,
    data_root: PathBuf,
    token_file: String,
    token_source: Option<Box<dyn TokenSource>>,
    ttl_secs: i64,
    device: DeviceSource,
    revoked: RevokedIds,
    clock_initialized: Option<ClockFlag>,
    generation: Option<Generation>,
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
            token_source: None,
            ttl_secs: DEFAULT_CACHE_TTL_SECS,
            device: Box::new(device),
            revoked: RevokedIds::new(),
            clock_initialized: None,
            generation: None,
        }
    }

    /// An incident denylist: a token whose authenticated entitlement id is listed is refused as
    /// [`EngineEntitlementError::Revoked`], before the time and lifecycle checks. The list is
    /// engine configuration; nothing a window or a message carries can edit it.
    pub fn with_revoked_ids(mut self, revoked: RevokedIds) -> Self {
        self.revoked = revoked;
        self
    }

    /// Whether the licence record says the anti-rollback ledger was initialised (see
    /// [`AntiRollbackClock::reconcile`](crate::AntiRollbackClock::reconcile)): when it is, a ledger
    /// that has gone missing refuses instead of starting a fresh one. Without this the engine
    /// passes `false`, as it always has.
    pub fn with_clock_initialized(
        mut self,
        flag: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.clock_initialized = Some(Box::new(flag));
        self
    }

    /// A change counter of whatever feeds this verifier (the engine snapshot cell). A verdict
    /// cached under an older value is never served, exactly like a `LicenseInvalidation` bump.
    pub fn with_generation(mut self, generation: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.generation = Some(Box::new(generation));
        self
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

    /// Load the token from `source` instead of the data-root token file. The
    /// file is then never read, so no window->file bridge is needed. Missing
    /// (`Ok(None)`) is NoLicense; a source error fails closed as Unreadable;
    /// signature/app/device/expiry are still always verified.
    pub fn with_token_source(mut self, source: impl TokenSource + 'static) -> Self {
        self.token_source = Some(Box::new(source));
        self
    }

    /// Opt-in credential-store token source: `reader` returns the stored
    /// signed token from the host's OS credential store (`Ok(None)` = none
    /// stored, `Err(_)` = store unreadable). See [`Self::with_token_source`].
    pub fn with_credential_reader(
        self,
        reader: impl Fn() -> Result<Option<String>, String> + Send + Sync + 'static,
    ) -> Self {
        self.with_token_source(CredentialTokenSource::new(reader))
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
    /// The licence-change generation this verdict was verified under (always 0 unless a
    /// `LicenseInvalidation` is attached). A verdict from an older generation is never served.
    generation: u64,
}

/// Verifies the stored licence inside an engine process. See the module docs.
pub struct EngineEntitlements<C = SystemClock> {
    cfg: EngineEntitlementsConfig,
    clock: C,
    wall: WallClock,
    cache: Mutex<Option<Cached>>,
    #[cfg(feature = "client")]
    invalidation: Option<crate::LicenseInvalidation>,
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
            #[cfg(feature = "client")]
            invalidation: None,
        }
    }

    /// Attach the native licence client's change counter (feature `client`). From then on a
    /// verdict cached before the client's latest change is never served: the next check
    /// re-reads and re-verifies the stored record, so a deactivation, refresh or activation
    /// reaches this verifier at once instead of after the cache TTL. An engine verifier that is
    /// rebuilt attaches the same handle again; nothing is carried between instances. The signal
    /// has no data and can only make a check stricter, so it grants nothing.
    #[cfg(feature = "client")]
    pub fn with_invalidation(mut self, invalidation: &crate::LicenseInvalidation) -> Self {
        self.invalidation = Some(invalidation.clone());
        self
    }

    /// The change generation a verdict verified now belongs to.
    fn generation_now(&self) -> u64 {
        let fed = self
            .cfg
            .generation
            .as_ref()
            .map_or(0, |generation| generation());
        #[cfg(feature = "client")]
        {
            fed.wrapping_add(
                self.invalidation
                    .as_ref()
                    .map_or(0, crate::LicenseInvalidation::generation),
            )
        }
        #[cfg(not(feature = "client"))]
        {
            fed
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
        // Read before verifying: a change that lands during the verification leaves the new
        // verdict marked with the older generation, so it is not served afterwards.
        let generation = self.generation_now();
        let wall = (self.wall)()
            .map_err(|e| EngineEntitlementError::Clock(ClockRefusal::Unavailable(e)))?;
        let initialized = self
            .cfg
            .clock_initialized
            .as_ref()
            .is_some_and(|flag| flag());
        let now = self
            .clock
            .trusted_now(wall, initialized)
            .map_err(EngineEntitlementError::Clock)?;
        if let Ok(guard) = self.cache.lock() {
            if let Some(c) = guard.as_ref() {
                let age = now.saturating_sub(c.verified_at);
                if self.cfg.ttl_secs > 0
                    && (0..self.cfg.ttl_secs).contains(&age)
                    && now < c.entitlement.exp
                    && c.generation == generation
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
                generation,
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
        let token = match &self.cfg.token_source {
            Some(source) => credential_source::load_bounded_token(source.as_ref())?,
            None => credential_source::load_bounded_token(&FileTokenSource::new(
                self.cfg.data_root.join(&self.cfg.token_file),
            ))?,
        };
        let device = (self.cfg.device)().map_err(EngineEntitlementError::DeviceUnavailable)?;
        if device.trim().is_empty() {
            return Err(EngineEntitlementError::DeviceUnavailable(
                "empty device identity".to_string(),
            ));
        }
        if self.cfg.revoked.is_empty() {
            return crate::verify_token(&token, &self.cfg.pubkey, &self.cfg.app, &device, now)
                .map_err(EngineEntitlementError::Invalid);
        }
        match verify_token_not_revoked(
            &token,
            &self.cfg.pubkey,
            &self.cfg.app,
            &[device.as_str()],
            now,
            &self.cfg.revoked,
        ) {
            Ok(entitlement) => Ok(entitlement),
            Err(RevocationError::Verify(error)) => Err(EngineEntitlementError::Invalid(error)),
            Err(RevocationError::Revoked { id }) => Err(EngineEntitlementError::Revoked(id)),
        }
    }
}
