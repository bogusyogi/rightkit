//! The owner-to-engine entitlement snapshot (feature `engine-snapshot`).
//!
//! An engine (sidecar, daemon, CLI) must decide Pro itself and must never read the owner's
//! credential store (`rightkit-credentials` README, "Helpers, sidecars and CLIs": only the owner
//! binary reads the OS store). The owner therefore hands it three values through
//! `rightkit_credentials::handoff`, as one snapshot:
//!
//! | name | value |
//! |------|-------|
//! | [`SNAPSHOT_TOKEN`] (`licence.token`) | the signed licence token text |
//! | [`SNAPSHOT_DEVICE`] (`licence.device`) | the device binding the token was issued for (the raw id for a typed key, its hash for a lease) |
//! | [`SNAPSHOT_CLOCK`] (`licence.clock`) | `initialized` once the owner's anti-rollback ledger exists, else `fresh` |
//!
//! The owner builds it with `LicenseClient::engine_snapshot` (feature `client`) when the engine
//! starts and again after every change (`LicenseInvalidation`), and answers an engine's request
//! for it with `LicenseClient::answer_engine_request`. The engine keeps it in an
//! [`EngineSnapshotCell`] and verifies it with [`EngineEntitlements::from_snapshot`]: the suite
//! key, the app, the device binding, expiry, an optional incident denylist
//! ([`EngineEntitlementsConfig::with_revoked_ids`]) and the anti-rollback ledger the owner's
//! client keeps as a file in the shared licence directory ([`DEFAULT_CLOCK_FILE`]). The engine only
//! reads that ledger ([`OwnerLedgerReader`]): the owner is its one writer, as for the token file, so
//! two processes can never take turns claiming the mark and refusing each other.
//!
//! What the snapshot is not: a verdict. The engine has no input for "you are Pro". Everything
//! the snapshot carries is checked again in the engine process, so a forged or replayed snapshot
//! can at most hand it a token that does not verify. The device binding is the one value the
//! engine takes on the owner's word, which is why the transport must authenticate the owner
//! (same app, same user, signed path) before [`EngineSnapshotCell::apply`].
//!
//! The ledger must be the file ledger: an owner that configured
//! `LicenseClientConfig::with_credential_clock` keeps its anti-rollback ledger in the credential
//! store, which an engine cannot read, so such an app moves to the file ledger before it adopts
//! the snapshot.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rightkit_credentials::handoff::{Entry, Snapshot};
use zeroize::Zeroizing;

use crate::engine::TokenSource;
use crate::{
    system_now_unix, AntiRollbackClock, ClockRefusal, ClockState, ClockStore,
    EngineEntitlementError, EngineEntitlements, EngineEntitlementsConfig, FileClockStore,
    FileClockStoreError, ProcessAnchor, MAX_TOKEN_BYTES,
};

/// The signed licence token text.
pub const SNAPSHOT_TOKEN: &str = "licence.token";
/// The device binding the token was issued for.
pub const SNAPSHOT_DEVICE: &str = "licence.device";
/// Whether the owner's anti-rollback ledger exists: [`CLOCK_INITIALIZED`] or [`CLOCK_FRESH`].
pub const SNAPSHOT_CLOCK: &str = "licence.clock";
/// Every name of the snapshot. An engine asks for exactly these.
pub const SNAPSHOT_NAMES: [&str; 3] = [SNAPSHOT_TOKEN, SNAPSHOT_DEVICE, SNAPSHOT_CLOCK];
/// The owner's ledger exists, so a missing ledger in the engine is a refusal.
pub const CLOCK_INITIALIZED: &str = "initialized";
/// The owner has not initialised its ledger yet.
pub const CLOCK_FRESH: &str = "fresh";
/// The ledger file inside the licence directory; the owner's client and the engine open the
/// same file.
pub const DEFAULT_CLOCK_FILE: &str = "clock.json";

const NOT_DELIVERED: &str = "licence snapshot not delivered yet";
const OWNER_READ_FAILED: &str = "licence snapshot: the owner could not read the licence";
const MALFORMED: &str = "licence snapshot is malformed";

/// Why [`EngineSnapshotCell::apply`] kept nothing. In both cases the cell now fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnapshotError {
    /// The owner reported that it could not read the licence.
    OwnerReadFailed,
    /// The snapshot has a token but no usable device binding, or a value that is not text, empty
    /// or too large.
    Malformed,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OwnerReadFailed => OWNER_READ_FAILED,
            Self::Malformed => MALFORMED,
        })
    }
}

impl std::error::Error for SnapshotError {}

enum Held {
    /// Nothing delivered yet: the engine fails closed (`Unreadable`), it is not "no licence".
    NotDelivered,
    /// The owner reported no licence: Free.
    Absent,
    /// The owner could not read the licence, or sent something unusable: fail closed.
    Unavailable(&'static str),
    Present {
        token: Zeroizing<String>,
        device: Zeroizing<String>,
        clock_initialized: bool,
    },
}

struct Inner {
    held: Mutex<Held>,
    /// Counts every [`EngineSnapshotCell::apply`], so a verdict cached before a delivery is never
    /// served after it.
    generation: Mutex<u64>,
}

/// What the engine holds of the owner's snapshot. Cloning shares it, so the delivery thread and
/// the verifier see one value. Never prints a value.
#[derive(Clone)]
pub struct EngineSnapshotCell {
    inner: Arc<Inner>,
}

impl Default for EngineSnapshotCell {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for EngineSnapshotCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match &*self.held() {
            Held::NotDelivered => "not-delivered",
            Held::Absent => "absent",
            Held::Unavailable(_) => "unavailable",
            Held::Present { .. } => "present",
        };
        f.debug_struct("EngineSnapshotCell")
            .field("state", &state)
            .finish()
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn text_of(entry: Option<&Entry>) -> Option<&str> {
    match entry {
        Some(Entry::Present(secret)) => secret.expose_str().ok(),
        _ => None,
    }
}

impl EngineSnapshotCell {
    /// An empty cell. Until the first [`apply`](Self::apply) the engine refuses as unreadable.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                held: Mutex::new(Held::NotDelivered),
                generation: Mutex::new(0),
            }),
        }
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        locked(&self.inner.held)
    }

    /// Replace everything the cell holds with `snapshot` (a whole replacement, like
    /// `Snapshot::replace_into`): a token that is absent or not mentioned is "no licence", an
    /// owner read failure and a malformed snapshot leave the cell failing closed and say why.
    /// The verifier notices a delivery through [`generation`](Self::generation), so no separate
    /// `EngineEntitlements::invalidate` call is needed.
    pub fn apply(&self, snapshot: Snapshot) -> Result<(), SnapshotError> {
        let (held, outcome) = match snapshot.get(SNAPSHOT_TOKEN) {
            None | Some(Entry::Absent) => (Held::Absent, Ok(())),
            Some(Entry::ReadFailed(_)) => (
                Held::Unavailable(OWNER_READ_FAILED),
                Err(SnapshotError::OwnerReadFailed),
            ),
            Some(entry @ Entry::Present(_)) => {
                let token = text_of(Some(entry))
                    .map(str::trim)
                    .filter(|token| !token.is_empty() && token.len() as u64 <= MAX_TOKEN_BYTES);
                let device = text_of(snapshot.get(SNAPSHOT_DEVICE))
                    .map(str::trim)
                    .filter(|device| !device.is_empty());
                match (token, device) {
                    (Some(token), Some(device)) => (
                        Held::Present {
                            token: Zeroizing::new(token.to_owned()),
                            device: Zeroizing::new(device.to_owned()),
                            // Anything but an explicit "fresh" is treated as initialised: a missing
                            // ledger then refuses, which is the stricter reading.
                            clock_initialized: text_of(snapshot.get(SNAPSHOT_CLOCK))
                                != Some(CLOCK_FRESH),
                        },
                        Ok(()),
                    ),
                    _ => (Held::Unavailable(MALFORMED), Err(SnapshotError::Malformed)),
                }
            }
        };
        *self.held() = held;
        let mut generation = locked(&self.inner.generation);
        *generation = generation.wrapping_add(1);
        outcome
    }

    /// Forget everything (the engine fails closed again until the next delivery).
    pub fn clear(&self) {
        *self.held() = Held::NotDelivered;
        let mut generation = locked(&self.inner.generation);
        *generation = generation.wrapping_add(1);
    }

    /// How many deliveries (and clears) the cell has seen. A verifier built with
    /// [`EngineEntitlementsConfig::from_snapshot`] never serves a verdict cached under an older
    /// value.
    pub fn generation(&self) -> u64 {
        *locked(&self.inner.generation)
    }

    fn device(&self) -> Result<String, String> {
        match &*self.held() {
            Held::Present { device, .. } => Ok(device.to_string()),
            _ => Err("no device binding in the licence snapshot".to_owned()),
        }
    }

    /// The record's "ledger initialised" flag; `false` whenever there is no token to verify, so a
    /// missing ledger never turns "no licence" into a clock refusal.
    fn clock_initialized(&self) -> bool {
        matches!(
            &*self.held(),
            Held::Present {
                clock_initialized: true,
                ..
            }
        )
    }
}

impl TokenSource for EngineSnapshotCell {
    fn load_token(&self) -> Result<Option<String>, String> {
        match &*self.held() {
            Held::NotDelivered => Err(NOT_DELIVERED.to_owned()),
            Held::Absent => Ok(None),
            Held::Unavailable(text) => Err((*text).to_owned()),
            Held::Present { token, .. } => Ok(Some(token.to_string())),
        }
    }
}

impl EngineEntitlementsConfig {
    /// A verifier configuration whose token, device binding, ledger flag and change counter come
    /// from `cell`; `pubkey` is the key the token must verify under (tests pass their own; an app
    /// uses [`Self::suite_snapshot`]) and `app` the appKey it must be minted for.
    pub fn from_snapshot(
        pubkey: [u8; 32],
        app: impl Into<String>,
        cell: &EngineSnapshotCell,
    ) -> Self {
        let for_device = cell.clone();
        let for_clock = cell.clone();
        let for_generation = cell.clone();
        Self::new(pubkey, app, std::path::PathBuf::new(), move || {
            for_device.device()
        })
        .with_token_source(cell.clone())
        .with_clock_initialized(move || for_clock.clock_initialized())
        .with_generation(move || for_generation.generation())
    }

    /// [`Self::from_snapshot`] under the Right Suite licence key from the contract
    /// ([`crate::suite_api::LICENSE_PUBLIC_KEY`]), so the engine carries no key of its own.
    pub fn suite_snapshot(app: impl Into<String>, cell: &EngineSnapshotCell) -> Self {
        Self::from_snapshot(crate::suite_api::LICENSE_PUBLIC_KEY, app, cell)
    }
}

/// The owner's anti-rollback ledger, opened by an engine: every read is the real file (under its
/// shared lock, so it never sees half a write) and every write is dropped. The engine still
/// refuses a wall clock that is behind the owner's mark and a ledger that vanished after the owner
/// initialised it; it keeps what it observes in memory only, and never advances the owner's mark.
#[derive(Debug, Clone)]
pub struct OwnerLedgerReader(FileClockStore);

impl OwnerLedgerReader {
    /// The ledger file inside `license_dir` ([`DEFAULT_CLOCK_FILE`]).
    pub fn new(license_dir: impl AsRef<Path>) -> Self {
        Self(FileClockStore::new(
            license_dir.as_ref().join(DEFAULT_CLOCK_FILE),
        ))
    }
}

impl ClockStore for OwnerLedgerReader {
    type Error = FileClockStoreError;

    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        self.0.load()
    }

    fn save(&self, _: &ClockState) -> Result<(), Self::Error> {
        Ok(())
    }

    fn compare_and_save(
        &self,
        _: Option<&ClockState>,
        _: &ClockState,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

impl EngineEntitlements<AntiRollbackClock<OwnerLedgerReader>> {
    /// A verifier over a snapshot configuration ([`EngineEntitlementsConfig::from_snapshot`]) and
    /// the ledger file the owner's client keeps in `license_dir`, read-only ([`OwnerLedgerReader`]).
    /// Refuses with `Clock` when the process clock cannot be read.
    pub fn from_snapshot(
        cfg: EngineEntitlementsConfig,
        license_dir: impl AsRef<Path>,
    ) -> Result<Self, EngineEntitlementError> {
        let wall = system_now_unix()
            .map_err(|e| EngineEntitlementError::Clock(ClockRefusal::Unavailable(e)))?;
        Ok(Self::with_clock(
            cfg,
            AntiRollbackClock::new(
                OwnerLedgerReader::new(license_dir),
                ProcessAnchor::new(wall),
            ),
        ))
    }

    /// The whole engine side in one call: the suite key, `app`, the snapshot in `cell` and the
    /// ledger in `license_dir`.
    pub fn suite_snapshot(
        app: impl Into<String>,
        cell: &EngineSnapshotCell,
        license_dir: impl AsRef<Path>,
    ) -> Result<Self, EngineEntitlementError> {
        Self::from_snapshot(
            EngineEntitlementsConfig::suite_snapshot(app, cell),
            license_dir,
        )
    }
}
