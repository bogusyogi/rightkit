//! Engine-side entitlement gate for the daemon (Pro entitlement).
//!
//! Pro comes from a snapshot the licence owner (the desktop shell's `rightkit-license` client)
//! delivers over the daemon's local transport (current-user-only unix socket or named pipe); no
//! token, key, feature list, device id or locator is read from an environment variable after
//! start-up, an argument or any HTTP route. The daemon never opens the owner's credential store
//! and accepts no verdict, feature list, licence key or raw device id from its caller.
//!
//! * Delivery: the owner calls the `license.snapshot` method
//!   (`rightkit_daemon::LICENSE_SNAPSHOT_METHOD`) of this daemon's private IPC service
//!   (`license_ipc`, `engine-license-<port>`) with one `rightkit_credentials::handoff` Reply frame,
//!   base64 in the `frame` parameter, at launch and after every licence change. The daemon
//!   identifies the peer process before it reads a byte of the frame (same user, the owner's
//!   executable pinned by pid and start time, and its code signature, see [`OwnerLocator`] and
//!   [`LicenseGate::deliver_from_owner`]), applies the snapshot to an [`EngineSnapshotCell`] as a
//!   whole replacement and answers the owner's Ack frame in `ack`.
//! * Fail closed: until the first accepted delivery the gate is Free, and an owner read failure, a
//!   malformed snapshot or a refused peer leaves it Free (or as it was, for a refused peer).
//! * Verification: the snapshot is never a verdict. `EngineEntitlements::require_pro` checks the
//!   signature against the suite key, the app, the device binding, the expiry and the
//!   anti-rollback clock. The anti-rollback clock is always on and holds both ledgers the owner can
//!   keep: the mark that travels inside the snapshot (an owner on the credential-store clock) and
//!   the owner's ledger file `<licence dir>/clock.json` (an owner on the file clock), which the
//!   daemon only READS. See `OwnerClocks`.
//! * Liveness: a granting verdict is cached for a short TTL and is dropped whenever a delivery
//!   arrives (the cell's generation, and an explicit drop), so a refresh, revocation, deactivation
//!   or new activation needs no daemon restart.
//! * Launch locators: the licence directory (non-secret, for the file ledger) and the owner's
//!   identity ([`OwnerLocator`]: its executable path and signing identity, also non-secret) are
//!   the only launch input. They are read once at process start by
//!   [`scrub_inherited_licence_env`] (also [`capture_launch_locators`]), which then removes every
//!   licence-named variable from the daemon's own environment. Child processes and `${VAR}`
//!   expansion additionally refuse any licence-named variable
//!   (`rightkit_agent_config::licence_env`). Without a complete owner locator no service is hosted
//!   and the daemon stays Free.

use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rightkit_agent_config::licence_env::{
    is_licence_env_name, ENV_LICENSE_DIR, ENV_LICENSE_OWNER_EXE, ENV_LICENSE_OWNER_SIGNER,
    ENV_LICENSE_OWNER_TEAM,
};
use rightkit_credentials::handoff::{
    receive_snapshot_delivery, write_message, Limits, Message, SnapshotDelivery,
};
use rightkit_daemon::{PeerPolicy, SigningIdentity};
use rightkit_license::{
    system_now_unix, AntiRollbackClock, ClockRefusal, ClockStore, EngineEntitlements,
    EngineEntitlementsConfig, EngineSnapshotCell, Entitlement, OwnerLedgerReader, ProcessAnchor,
    SnapshotLedgerReader, TrustedClock,
};

/// The app identity the suite tokens are signed for: the host's licence app id.
pub fn expected_app() -> &'static str {
    crate::host_identity::host().license_app()
}

/// How long a granting verdict is served before the snapshot is verified again.
const GRANT_TTL: Duration = Duration::from_secs(30);
/// How long a refusal is served.
const DENIAL_TTL: Duration = Duration::from_secs(5);

// ---- launch locators ---------------------------------------------------------------------------

/// Who the licence owner is, as the owner told this daemon at launch (non-secret): its executable
/// and the code-signing identity that must sign it. The snapshot is accepted only from a process
/// that is that executable AND signed by that identity. The environment names are the
/// `rightkit_agent_config::licence_env` constants (`ENV_LICENSE_OWNER_EXE`, `_TEAM`, `_SIGNER`);
/// all carry the licence prefix, so [`scrub_inherited_licence_env`] removes them with the rest and
/// no child process sees them.
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
        let executable = PathBuf::from(value(ENV_LICENSE_OWNER_EXE)?);
        if !executable.is_absolute() {
            return None;
        }
        let mac_team_id = value(ENV_LICENSE_OWNER_TEAM);
        let windows_signer = value(ENV_LICENSE_OWNER_SIGNER);
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

/// Where the owner's anti-rollback ledger file lives, and who the owner is. Nothing here is
/// secret, and no credential namespace or account is passed: the daemon never opens the owner's
/// store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseLocators {
    /// `<app data>/license`: holds the clock ledger file the owner writes and the daemon reads.
    /// `None` for an owner on the credential-store clock, whose mark travels in the snapshot.
    pub license_dir: Option<PathBuf>,
    /// The owner that may deliver the snapshot; `None` means no delivery is accepted.
    pub owner: Option<OwnerLocator>,
}

impl LicenseLocators {
    /// The locators named by `lookup`, or `None` unless the licence directory or a complete owner
    /// is present.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let license_dir = lookup(ENV_LICENSE_DIR)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let owner = OwnerLocator::from_lookup(&lookup);
        if license_dir.is_none() && owner.is_none() {
            return None;
        }
        Some(Self { license_dir, owner })
    }
}

static LAUNCH_LOCATORS: OnceLock<Option<LicenseLocators>> = OnceLock::new();

/// Read the launch locators once, THEN remove EVERY licence-named variable from this process's
/// environment, so no tool, hook, script or MCP server inherits one. The order matters: the
/// locators are licence-named and are gone after the loop. Call first thing in `main`, before any
/// other thread exists. Calling it again keeps the first reading (the environment is already
/// clean), so a host that calls it first and the server entry that calls it again both work.
pub fn scrub_inherited_licence_env() {
    let locators = LicenseLocators::from_lookup(|name| std::env::var(name).ok());
    LAUNCH_LOCATORS.get_or_init(|| locators);
    let inherited: Vec<std::ffi::OsString> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_str().is_some_and(is_licence_env_name))
        .collect();
    for name in inherited {
        std::env::remove_var(name);
    }
}

/// Alias of [`scrub_inherited_licence_env`]: read the launch locators once and remove every
/// licence-named variable from the environment.
pub fn capture_launch_locators() {
    scrub_inherited_licence_env();
}

/// The locators the shell passed this daemon at launch, if any.
pub fn launch_locators() -> Option<&'static LicenseLocators> {
    LAUNCH_LOCATORS.get().and_then(Option::as_ref)
}

// ---- the anti-rollback clock -------------------------------------------------------------------

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

/// The anti-rollback clock over every ledger the owner can keep. Always on; both ledgers are only
/// ever READ (their writes are dropped), so the owner stays their one writer.
///
/// * The mark delivered inside the snapshot (`SnapshotLedgerReader`, an owner on the
///   credential-store clock) is checked whenever the snapshot carried one, and also when there is
///   no ledger file to read, where a missing ledger of an initialised install refuses.
/// * The owner's ledger file (`OwnerLedgerReader`, an owner on the file clock) is checked whenever
///   the licence directory is known. While a delivered mark proves the ledger exists, a missing
///   file is not a refusal, but a file that exists is still held to.
///
/// When both apply, the time served is the later of the two and either refusal refuses.
struct OwnerClocks {
    mark: AntiRollbackClock<SnapshotLedgerReader>,
    delivered: SnapshotLedgerReader,
    file: Option<AntiRollbackClock<OwnerLedgerReader>>,
}

impl OwnerClocks {
    fn new(cell: &EngineSnapshotCell, license_dir: Option<&Path>, wall_now: i64) -> Self {
        Self {
            mark: AntiRollbackClock::new(
                SnapshotLedgerReader::new(cell),
                ProcessAnchor::process(wall_now),
            ),
            delivered: SnapshotLedgerReader::new(cell),
            file: license_dir.map(|dir| {
                AntiRollbackClock::new(
                    OwnerLedgerReader::new(dir),
                    ProcessAnchor::process(wall_now),
                )
            }),
        }
    }
}

impl TrustedClock for OwnerClocks {
    fn trusted_now(&self, wall_now: i64, clock_was_initialized: bool) -> Result<i64, ClockRefusal> {
        let mark_delivered = matches!(self.delivered.load(), Ok(Some(_)));
        let mut served: Option<i64> = None;
        if mark_delivered || self.file.is_none() {
            served = Some(self.mark.trusted_now(wall_now, clock_was_initialized)?);
        }
        if let Some(file) = &self.file {
            let now = file.trusted_now(wall_now, clock_was_initialized && !mark_delivered)?;
            served = Some(served.map_or(now, |earlier| earlier.max(now)));
        }
        served.ok_or(ClockRefusal::Missing)
    }
}

// ---- the gate ----------------------------------------------------------------------------------

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
    /// Whether the previous verdict granted Pro, and until when: the one fact a log line needs to
    /// tell a start from a stop and an expiry from a refusal.
    last: Option<(bool, Option<i64>)>,
    /// Where a licence log line goes (stderr, which the daemon log captures).
    log: Box<dyn Fn(&str) + Send>,
}

/// The one log line for a change of licence verdict (licensing rule 15), or `None` while the
/// verdict is unchanged. It carries the tier, the lifecycle and the reason, and never a token, a
/// key or a device id: the gate only knows whether a verified entitlement exists.
fn transition_line(
    last: Option<(bool, Option<i64>)>,
    granted: bool,
    now: Option<i64>,
) -> Option<String> {
    match (last, granted) {
        (Some((true, _)), true) | (Some((false, _)), false) => None,
        (None | Some((false, _)), true) => Some("[license] tier=pro lifecycle=active".to_string()),
        (None, false) => {
            Some("[license] tier=free lifecycle=none reason=no_verified_entitlement".to_string())
        }
        (Some((true, exp)), false) => {
            let reason = match (exp, now) {
                (Some(exp), Some(now)) if now >= exp => "expired",
                _ => "no_longer_verified",
            };
            Some(format!(
                "[license] tier=free lifecycle=ended reason={reason}"
            ))
        }
    }
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
    deliveries: AtomicU64,
    grant_ttl: Duration,
    denial_ttl: Duration,
}

impl LicenseGate {
    /// A gate over the launch locators ([`launch_locators`]): the owner's ledger file when the
    /// licence directory was given, the mark in the snapshot otherwise. It holds no snapshot, so
    /// it is Free until the owner's first accepted delivery. Opens nothing for writing and no
    /// credential store. When the wall clock cannot be read the gate stays Free.
    pub fn new() -> Self {
        let license_dir = launch_locators().and_then(|locators| locators.license_dir.as_deref());
        Self::build(license_dir).unwrap_or_else(|_| Self::closed())
    }

    /// Open the gate over the owner's ledger. Holds no snapshot yet, so it is not Pro until the
    /// owner delivers one. Opens nothing for writing and no credential store.
    pub fn open(locators: &LicenseLocators) -> Result<Self, String> {
        Self::build(locators.license_dir.as_deref())
    }

    fn build(license_dir: Option<&Path>) -> Result<Self, String> {
        let cell = EngineSnapshotCell::new();
        let now = system_now_unix().map_err(|error| format!("no wall clock: {error}"))?;
        let clock = OwnerClocks::new(&cell, license_dir, now);
        let config = EngineEntitlementsConfig::suite_snapshot(expected_app(), &cell);
        Ok(Self::with_config(
            cell,
            config,
            clock,
            (GRANT_TTL, DENIAL_TTL),
        ))
    }

    /// A gate that grants nothing (no wall clock to verify against). Deliveries are still counted.
    fn closed() -> Self {
        Self::from_verify(
            EngineSnapshotCell::new(),
            Box::new(|| None),
            (GRANT_TTL, DENIAL_TTL),
        )
    }

    /// The gate over an explicit configuration (the production wiring above, and tests).
    pub(crate) fn with_config<C>(
        cell: EngineSnapshotCell,
        config: EngineEntitlementsConfig,
        clock: C,
        ttls: (Duration, Duration),
    ) -> Self
    where
        C: TrustedClock + Send + 'static,
    {
        // The gate caches; the verifier itself verifies on every call.
        let config = config.with_cache_ttl_secs(0);
        let verifier =
            EngineEntitlements::with_clock(config, SharedClock(Arc::new(Mutex::new(clock))));
        Self::from_verify(cell, Box::new(move || verifier.require_pro().ok()), ttls)
    }

    fn from_verify(
        cell: EngineSnapshotCell,
        verify: Box<dyn Fn() -> Option<Entitlement> + Send>,
        (grant_ttl, denial_ttl): (Duration, Duration),
    ) -> Self {
        Self {
            cell,
            state: Mutex::new(GateState {
                verify,
                verdict: None,
                last: None,
                log: Box::new(|line| eprintln!("{line}")),
            }),
            deliveries: AtomicU64::new(0),
            grant_ttl,
            denial_ttl,
        }
    }

    /// Accept the owner's snapshot over a transport that identifies its peer (the daemon's
    /// private IPC): the process on the other end must be the owner (`owner` names its executable
    /// and carries the code-signing verifier; see `HostExpectation::with_verifier`). The peer is
    /// checked before a byte of `frame` is read; a refused peer or a frame that does not decode
    /// changes nothing. Every attempt past the check drops the cached verdict (a delivery takes
    /// effect at once), counts as a delivery and replaces what the cell holds; a snapshot the
    /// cell refuses (owner read failure, malformed) is still acknowledged and leaves the gate
    /// Free. Returns the owner's Ack frame.
    ///
    /// There is no bearer-only delivery (rightkit#1501): the transport identifies its peer.
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
        let mut cursor = Cursor::new(frame);
        let SnapshotDelivery { id, snapshot } =
            receive_snapshot_delivery(&mut cursor, &Limits::default())
                .map_err(|_| DeliveryError::Malformed)?;
        if cursor.position() as usize != frame.len() {
            return Err(DeliveryError::Malformed);
        }
        {
            // A poisoned lock is recovered, never skipped: the verdict must be dropped.
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.verdict = None;
            self.deliveries.fetch_add(1, Ordering::SeqCst);
            if self.cell.apply(snapshot).is_err() {
                // The cell fails closed; the reason is not echoed.
                eprintln!("[license] licence snapshot refused, running as Free");
            }
        }
        let mut ack = Vec::new();
        write_message(&mut ack, &Message::ack(id)).map_err(|_| DeliveryError::Ack)?;
        Ok(ack)
    }

    /// How many snapshots the owner has delivered (accepted peer, decodable frame).
    pub fn deliveries(&self) -> u64 {
        self.deliveries.load(Ordering::SeqCst)
    }

    /// Whether the owner has delivered at least one snapshot.
    pub fn delivered(&self) -> bool {
        self.deliveries() > 0
    }

    /// `"pro"` while a verified Pro entitlement exists, otherwise `"free"`.
    pub fn tier_label(&self) -> &'static str {
        if self.features().is_empty() {
            "free"
        } else {
            "pro"
        }
    }

    /// The lower-cased feature names of the verified Pro entitlement; empty before the first
    /// delivery and on Free, a tombstone, an unverifiable, expired, rolled-back or unreadable
    /// record, and while the owner is writing.
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
        let granted = verdict.exp.is_some();
        if let Some(line) = transition_line(state.last, granted, system_now_unix().ok()) {
            (state.log)(&line);
        }
        state.last = Some((granted, verdict.exp));
        state.verdict = Some(verdict);
        features
    }

    /// Whether the verified Pro entitlement includes `feature`.
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features()
            .contains(&feature.trim().to_ascii_lowercase())
    }
}

impl Default for LicenseGate {
    fn default() -> Self {
        Self::new()
    }
}
