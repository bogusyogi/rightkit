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
//! * Delivery: the owner calls the `license.snapshot` method (`rightkit_daemon::LICENSE_SNAPSHOT_METHOD`)
//!   of this daemon's private IPC service (`license_ipc`), at launch and after every licence
//!   change, with one `rightkit_credentials::handoff` Reply frame (`RKHO`) base64-encoded in the
//!   `frame` parameter. The daemon identifies the peer process before it reads a byte of the frame
//!   (same user, the owner's executable pinned by pid and start time, and its code signature; see
//!   [`OwnerLocator`] and [`LicenseGate::deliver_from_owner`]), applies the snapshot to an
//!   [`EngineSnapshotCell`] as a whole replacement and answers the owner's Ack frame in `ack`. The
//!   loopback HTTP route `/internal/license-snapshot`, which only had the per-launch bearer token,
//!   is retired (rightkit#1501).
//! * Fail closed: until the first delivery the gate is not Pro, and an owner read failure or a
//!   malformed snapshot leaves it not Pro until the next good delivery.
//! * Verification: `EngineEntitlements::from_snapshot` (signature against the suite key, app,
//!   device binding the snapshot names, expiry) over an anti-rollback clock on the owner's ledger
//!   (`<licence dir>/clock.json`), which the daemon only READS (`OwnerLedgerReader`).
//! * Liveness: the verdict is cached for a short TTL and also dropped whenever a delivery arrives
//!   (the cell's generation), so a refresh, revocation, deactivation or new activation needs no
//!   daemon restart.
//!
//! The launch locators are the licence directory (non-secret, for the ledger) and the owner's
//! identity ([`OwnerLocator`]: its executable path and signing identity, also non-secret). They are
//! read once at process start and removed from the daemon's own environment
//! ([`capture_launch_locators`]);
//! child processes and `${VAR}` expansion additionally refuse any licence-named variable
//! (`rightkit_agent_config::licence_env`).

use std::collections::HashSet;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rightkit_agent_config::licence_env::{is_licence_env_name, ENV_LICENSE_DIR};
use rightkit_credentials::handoff::{
    receive_snapshot_delivery, write_message, Limits, Message, SnapshotDelivery,
};
use rightkit_daemon::{PeerPolicy, SigningIdentity};
use rightkit_license::{
    system_now_unix, AntiRollbackClock, ClockRefusal, EngineEntitlements, EngineEntitlementsConfig,
    EngineSnapshotCell, Entitlement, OwnerLedgerReader, ProcessAnchor, TrustedClock,
};

/// The app identity RightKit's suite tokens are signed for: the host's licence app id.
pub fn expected_app() -> &'static str {
    crate::host_identity::host().license_app()
}

/// How long a granting verdict is served before the snapshot is verified again.
const GRANT_TTL: Duration = Duration::from_secs(30);
/// How long a refusal is served.
const DENIAL_TTL: Duration = Duration::from_secs(5);

// ---- launch locators ---------------------------------------------------------------------------

/// The environment names of the owner identity locators. Both carry the licence prefix, so
/// [`capture_launch_locators`] removes them with the rest and no child process sees them.
const ENV_OWNER_EXE: &str = "RIGHTKIT_LICENSE_OWNER_EXE";
const ENV_OWNER_MAC_TEAM: &str = "RIGHTKIT_LICENSE_OWNER_TEAM";
const ENV_OWNER_WINDOWS_SIGNER: &str = "RIGHTKIT_LICENSE_OWNER_SIGNER";

/// Who the licence owner is, as the owner told this daemon at launch (non-secret): its executable
/// and the code-signing identity that must sign it. The snapshot is accepted only from a process
/// that is that executable AND signed by that identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerLocator {
    /// The owner's executable (absolute).
    pub executable: PathBuf,
    /// macOS: the Apple Developer team identifier that signs the owner.
    pub mac_team_id: Option<String>,
    /// Windows: the common name of the Authenticode signer of the owner.
    pub windows_signer: Option<String>,
}

impl OwnerLocator {
    /// The owner named by `lookup`: an absolute executable path and at least one signing identity,
    /// or `None`. A missing identity is `None` and never an unsigned owner, so without a full
    /// locator no delivery is accepted and the daemon stays Free.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let value = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let executable = PathBuf::from(value(ENV_OWNER_EXE)?);
        if !executable.is_absolute() {
            return None;
        }
        let mac_team_id = value(ENV_OWNER_MAC_TEAM);
        let windows_signer = value(ENV_OWNER_WINDOWS_SIGNER);
        if mac_team_id.is_none() && windows_signer.is_none() {
            return None;
        }
        Some(Self {
            executable,
            mac_team_id,
            windows_signer,
        })
    }

    /// The peer policy this owner implies: its executable, signed by its identity. `None` when the
    /// locator names no signing identity.
    pub fn policy(&self) -> Option<PeerPolicy> {
        let mut identities = Vec::new();
        if let Some(team) = &self.mac_team_id {
            identities.push(SigningIdentity::MacTeamId(team.clone()));
        }
        if let Some(name) = &self.windows_signer {
            identities.push(SigningIdentity::WindowsCommonName(name.clone()));
        }
        let mut identities = identities.into_iter();
        let mut policy = PeerPolicy::signed_by(identities.next()?);
        for identity in identities {
            policy = policy.or_signed_by(identity);
        }
        Some(policy.only_executable(self.executable.clone()))
    }
}

/// Where the owner's anti-rollback ledger lives, and who the owner is. Nothing here is secret, and
/// no credential namespace or account is passed: the daemon never opens the owner's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseLocators {
    /// `<app data>/license`: holds the clock ledger the owner writes and the daemon reads.
    pub license_dir: PathBuf,
    /// The owner that may deliver the snapshot; `None` means no delivery is accepted.
    pub owner: Option<OwnerLocator>,
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
            owner: OwnerLocator::from_lookup(&lookup),
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

/// Why [`LicenseGate::deliver_from_owner`] kept nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryError {
    /// The bytes are not exactly one handoff Reply frame.
    Malformed,
    /// The Ack could not be encoded.
    Ack,
    /// The delivering process is not the owner: its identity could not be established or does
    /// not match the owner's executable and signing identity. Nothing was applied.
    Unauthenticated,
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
        let config = EngineEntitlementsConfig::suite_snapshot(expected_app(), &cell);
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

    /// As [`deliver`](Self::deliver), over a transport that identifies its peer (the daemon's
    /// private IPC): the process on the other end must be the owner. `owner` names its executable
    /// and carries the code-signing verifier; see `HostExpectation::with_verifier`.
    pub fn deliver_from_owner(
        &self,
        cx: &rightkit_daemon::contract::CallContext,
        owner: &rightkit_daemon::HostExpectation,
        frame: &[u8],
    ) -> Result<Vec<u8>, DeliveryError> {
        // The process behind this private-IPC connection must be the owner binary (same user, the
        // owner's executable, pinned by pid and start time, and `owner`'s signing verifier) before
        // a byte of the frame is read. Refused deliveries change nothing.
        cx.verify_peer_executable(owner).map_err(|error| {
            eprintln!("[license] licence snapshot delivery refused: {error}");
            DeliveryError::Unauthenticated
        })?;
        self.deliver(frame)
    }

    /// Accept the owner's snapshot: `frame` is exactly one handoff Reply frame. Replaces
    /// everything the gate holds and returns the owner's Ack frame. The caller must have
    /// authenticated the owner first; this is private to the crate for that reason, and the only
    /// caller is [`deliver_from_owner`](Self::deliver_from_owner). A frame that does not decode
    /// leaves the gate as it was. A snapshot the cell refuses (owner read failure, malformed) is
    /// still acknowledged, and leaves the gate not Pro.
    ///
    /// There is no bearer-only delivery any more (rightkit#1501): the loopback HTTP route is
    /// retired and the transport that remains identifies its peer.
    fn deliver(&self, frame: &[u8]) -> Result<Vec<u8>, DeliveryError> {
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
                .is_none_or(|exp| system_now_unix().is_ok_and(|now| now < exp));
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
