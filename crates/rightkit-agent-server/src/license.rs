//! Engine-side entitlement gate for the daemon (Pro entitlement).
//!
//! The daemon decides Pro itself, but it never reads the owner's credential store: only the owner
//! binary (the desktop shell's `rightkit-license` client) reads the OS store
//! (`rightkit-credentials` README, "Helpers, sidecars and CLIs"). The owner hands the daemon one
//! snapshot (`licence.token`, `licence.device`, `licence.clock`, see
//! `rightkit_license::SNAPSHOT_NAMES`) and the daemon verifies it again, here, under the suite key
//! and its own app identity. The snapshot is not a verdict: the daemon accepts no verdict, feature
//! list, licence key or raw device id from its caller, not by environment, not by argument.
//!
//! * Delivery: the owner POSTs the snapshot as one `rightkit_credentials::handoff` Reply frame
//!   (octet stream, `RKHO`) to `/internal/license-snapshot`, behind the daemon's per-launch bearer
//!   token (the daemon's existing owner channel), at launch and after every licence change. The
//!   daemon applies it to an [`EngineSnapshotCell`] as a whole replacement and answers the owner's
//!   Ack frame ([`LicenseGate::deliver`]).
//! * Fail closed: until the first delivery the gate is not Pro, and an owner read failure or a
//!   malformed snapshot leaves it not Pro until the next good delivery.
//! * Verification: `EngineEntitlements::from_snapshot` (signature against the suite key, app,
//!   device binding the snapshot names, expiry) over an anti-rollback clock on the owner's ledger
//!   (`<licence dir>/clock.json`), which the daemon only READS (`OwnerLedgerReader`).
//! * Liveness: the verdict is cached for a short TTL and also dropped whenever a delivery arrives
//!   (the cell's generation), so a refresh, revocation, deactivation or new activation needs no
//!   daemon restart.
//!
//! The only launch locator is the licence directory (non-secret, for the ledger). It is read once
//! at process start and removed from the daemon's own environment ([`capture_launch_locators`]);
//! child processes and `${VAR}` expansion additionally refuse any licence-named variable
//! (`rightkit_app_settings::agent_config::licence_env`).

use std::collections::HashSet;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rightkit_app_settings::agent_config::licence_env::{is_licence_env_name, ENV_LICENSE_DIR};
use rightkit_credentials::handoff::{
    receive_snapshot_delivery, write_message, Limits, Message, SnapshotDelivery,
};
use rightkit_license::{
    system_now_unix, AntiRollbackClock, ClockRefusal, EngineEntitlements, EngineEntitlementsConfig,
    EngineSnapshotCell, Entitlement, OwnerLedgerReader, ProcessAnchor, TrustedClock,
};

/// The app identity RightKit's suite tokens are signed for.
pub const EXPECTED_APP: &str = "coderight";

/// How long a granting verdict is served before the snapshot is verified again.
const GRANT_TTL: Duration = Duration::from_secs(30);
/// How long a refusal is served.
const DENIAL_TTL: Duration = Duration::from_secs(5);

// ---- launch locators ---------------------------------------------------------------------------

/// Where the owner's anti-rollback ledger lives. Nothing here is secret, and no credential
/// namespace or account is passed: the daemon never opens the owner's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseLocators {
    /// `<app data>/license`: holds the clock ledger the owner writes and the daemon reads.
    pub license_dir: PathBuf,
}

impl LicenseLocators {
    /// The locators named by `lookup`, or `None` unless the licence directory is present and
    /// non-empty.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let dir = lookup(ENV_LICENSE_DIR)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())?;
        Some(Self {
            license_dir: PathBuf::from(dir),
        })
    }
}

static LAUNCH_LOCATORS: OnceLock<Option<LicenseLocators>> = OnceLock::new();

/// Read the launch locators once, then remove EVERY licence-named variable from this process's
/// environment, so no tool, hook, script or MCP server inherits one. Call first thing in `main`,
/// before any other thread exists.
pub fn capture_launch_locators() {
    let locators = LicenseLocators::from_lookup(|name| std::env::var(name).ok());
    let inherited: Vec<std::ffi::OsString> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_str().is_some_and(is_licence_env_name))
        .collect();
    for name in inherited {
        std::env::remove_var(name);
    }
    let _ = LAUNCH_LOCATORS.set(locators);
}

/// The locators the shell passed this daemon at launch, if any.
pub fn launch_locators() -> Option<&'static LicenseLocators> {
    LAUNCH_LOCATORS.get().and_then(Option::as_ref)
}

// ---- the gate ----------------------------------------------------------------------------------

/// One anti-rollback clock behind a lock, so the verifier stays `Send`.
struct SharedClock<C>(Arc<Mutex<C>>);

impl<C> Clone for SharedClock<C> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<C: TrustedClock> TrustedClock for SharedClock<C> {
    fn trusted_now(&self, wall_now: i64, clock_was_initialized: bool) -> Result<i64, ClockRefusal> {
        match self.0.lock() {
            Ok(clock) => clock.trusted_now(wall_now, clock_was_initialized),
            Err(_) => Err(ClockRefusal::Unavailable(
                "clock state poisoned".to_string(),
            )),
        }
    }
}

struct Verdict {
    at: Instant,
    /// The cell's generation the verdict was computed under; a later delivery voids it.
    generation: u64,
    /// `exp` of the granting entitlement (a cached grant is never served past it).
    exp: Option<i64>,
    features: HashSet<String>,
}

struct GateState {
    verify: Box<dyn Fn() -> Option<Entitlement> + Send>,
    verdict: Option<Verdict>,
}

/// Why [`LicenseGate::deliver`] kept nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryError {
    /// The bytes are not exactly one handoff Reply frame.
    Malformed,
    /// The Ack could not be encoded.
    Ack,
}

/// The daemon's licence gate: the verified Pro features of the owner's snapshot, live.
pub struct LicenseGate {
    cell: EngineSnapshotCell,
    state: Mutex<GateState>,
    grant_ttl: Duration,
    denial_ttl: Duration,
}

impl LicenseGate {
    /// Open the gate over the owner's ledger. Holds no snapshot yet, so it is not Pro until the
    /// owner delivers one. Opens nothing for writing and no credential store.
    pub fn open(locators: &LicenseLocators) -> Result<Self, String> {
        let cell = EngineSnapshotCell::new();
        let now = system_now_unix().map_err(|error| format!("no wall clock: {error}"))?;
        let clock = AntiRollbackClock::new(
            OwnerLedgerReader::new(&locators.license_dir),
            ProcessAnchor::process(now),
        );
        let config = EngineEntitlementsConfig::suite_snapshot(EXPECTED_APP, &cell);
        Ok(Self::with_config(
            cell,
            config,
            clock,
            (GRANT_TTL, DENIAL_TTL),
        ))
    }

    /// The gate over an explicit configuration (the production wiring above, and tests).
    pub(crate) fn with_config<C>(
        cell: EngineSnapshotCell,
        config: EngineEntitlementsConfig,
        clock: C,
        (grant_ttl, denial_ttl): (Duration, Duration),
    ) -> Self
    where
        C: TrustedClock + Send + 'static,
    {
        // The gate caches; the verifier itself verifies on every call.
        let config = config.with_cache_ttl_secs(0);
        let verifier =
            EngineEntitlements::with_clock(config, SharedClock(Arc::new(Mutex::new(clock))));
        let verify = move || verifier.require_pro().ok();
        Self {
            cell,
            state: Mutex::new(GateState {
                verify: Box::new(verify),
                verdict: None,
            }),
            grant_ttl,
            denial_ttl,
        }
    }

    /// Accept the owner's snapshot: `frame` is exactly one handoff Reply frame. Replaces
    /// everything the gate holds and returns the owner's Ack frame. The caller must have
    /// authenticated the owner (the daemon's bearer token) first. A frame that does not decode
    /// leaves the gate as it was. A snapshot the cell refuses (owner read failure, malformed) is
    /// still acknowledged, and leaves the gate not Pro.
    pub fn deliver(&self, frame: &[u8]) -> Result<Vec<u8>, DeliveryError> {
        let mut cursor = Cursor::new(frame);
        let SnapshotDelivery { id, snapshot } =
            receive_snapshot_delivery(&mut cursor, &Limits::default())
                .map_err(|_| DeliveryError::Malformed)?;
        if cursor.position() as usize != frame.len() {
            return Err(DeliveryError::Malformed);
        }
        if let Err(error) = self.cell.apply(snapshot) {
            eprintln!("[license] licence snapshot refused, running as Free: {error}");
        }
        let mut ack = Vec::new();
        write_message(&mut ack, &Message::ack(id)).map_err(|_| DeliveryError::Ack)?;
        Ok(ack)
    }

    /// The lower-cased feature names of the verified Pro entitlement; empty before the first
    /// delivery and on Free, an unverifiable, expired, rolled-back or unreadable snapshot.
    pub fn features(&self) -> HashSet<String> {
        let Ok(mut state) = self.state.lock() else {
            return HashSet::new();
        };
        let generation = self.cell.generation();
        if let Some(verdict) = &state.verdict {
            // `exp` is set exactly when the verdict is a grant.
            let ttl = if verdict.exp.is_some() {
                self.grant_ttl
            } else {
                self.denial_ttl
            };
            let unexpired = verdict
                .exp
                .map_or(true, |exp| system_now_unix().is_ok_and(|now| now < exp));
            if verdict.generation == generation && verdict.at.elapsed() < ttl && unexpired {
                return verdict.features.clone();
            }
        }
        let entitlement = (state.verify)();
        let verdict = Verdict {
            at: Instant::now(),
            generation,
            exp: entitlement.as_ref().map(|entitlement| entitlement.exp),
            features: entitlement
                .map(|entitlement| {
                    entitlement
                        .features
                        .iter()
                        .map(|feature| feature.trim().to_ascii_lowercase())
                        .filter(|feature| !feature.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        };
        let features = verdict.features.clone();
        state.verdict = Some(verdict);
        features
    }

    /// Whether the verified Pro entitlement includes `feature`.
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features()
            .contains(&feature.trim().to_ascii_lowercase())
    }
}
