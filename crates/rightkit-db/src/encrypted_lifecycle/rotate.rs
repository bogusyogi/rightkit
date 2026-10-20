//! `rotate_key` and `recover_rotation` over the existing `rekey`.
//!
//! # Rotation
//!
//! Under the transition guard (the store has already closed its writer and pools and released its
//! own shared guard) and the key store's rotation lock, in this order, each phase journaled before
//! the next effect:
//!
//! 1. `Guarded`: journal written beside the database.
//! 2. `OldVerified`: the current key opens the file and equals the primary key in protected storage.
//! 3. `OldRetained`: the current key is written to the recovery slot and read back.
//! 4. `NewStaged`: the new key is staged as the pending key and read back.
//! 5. `Rekeying`: journaled before the database changes; then `PRAGMA rekey`.
//! 6. `Rekeyed`: **the database is read back with both keys**, whatever `rekey` returned. The new
//!    key opening the file means the database committed (even if `rekey` returned an error); only
//!    the old key opening it means nothing committed; neither is `Indeterminate`.
//! 7. The caller's validation callback runs on the rekeyed store (a failure rolls the database back).
//! 8. `Promoted`: the exact staged revision is promoted, then the primary slot is **read back**.
//!    An unsettled mutation retains both keys and the journal without rollback. Once mutation
//!    settlement is known, primary equals the new key is committed; primary still equals the old
//!    key is a verified noncommit and the database is
//!    rolled back to the old key (and read back); anything else is `Indeterminate`.
//! 9. `Cleaned`: the pending key is discarded. After a final readback proves that the key storage
//!    returns opens the database, the recovery slot is cleared (unless `retain_previous`) and the
//!    journal removed. A failure in this step is cleanup-only: it never rolls back a committed
//!    rotation (`CommittedCleanupPending`), and the journal stays so recovery finishes it.
//!
//! At every point at least one key stored durably (primary, pending or recovery) opens the file.
//!
//! # Recovery
//!
//! `recover_rotation` ignores what the journal says about phases and decides from the real state:
//! it tries every stored key (primary, pending, mapped legacy staging, recovery) against the file.
//! The key that opens it is made canonical (a pending key is promoted; a legacy staging or recovery
//! key is staged and promoted), leftovers are cleaned up, and nothing is deleted while it is the
//! only key that opens the file. If no key opens the file, or storage cannot be read, recovery
//! reports why and changes nothing.

use super::error::LifecycleError;
use super::keystore::{
    Key, KeySource, KeyStoreError, KeyStoreErrorKind, PendingRevision, ProtectedKeySession,
};
use super::store::{
    db_unavailable, file_present, is_plaintext_sqlite, key_store_error, probe, scan_candidates,
    EncryptedStore, EncryptedStoreConfig, PendingRead, Probe, Scan, SlotRead,
};
use super::transition::{
    map_lock_error, next_id, read_journal, remove_journal, Journal, JournalState, RootIdentity,
    StoreTransition, TransitionKind,
};
use crate::{DbError, IntegrityCheck, InterruptedRestore, StoreLock};
use rusqlite::OpenFlags;
use std::fmt;
use std::path::{Path, PathBuf};

// ----- phases and the test seam ---------------------------------------------------------------

/// The durable phases of a rotation, in order. A journal records the last one reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Guarded,
    OldVerified,
    OldRetained,
    NewStaged,
    Rekeying,
    Rekeyed,
    Promoted,
    Cleaned,
    Settled,
}

impl Phase {
    pub const ALL: [Phase; 9] = [
        Phase::Guarded,
        Phase::OldVerified,
        Phase::OldRetained,
        Phase::NewStaged,
        Phase::Rekeying,
        Phase::Rekeyed,
        Phase::Promoted,
        Phase::Cleaned,
        Phase::Settled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Guarded => "guarded",
            Self::OldVerified => "old-verified",
            Self::OldRetained => "old-retained",
            Self::NewStaged => "new-staged",
            Self::Rekeying => "rekeying",
            Self::Rekeyed => "rekeyed",
            Self::Promoted => "promoted",
            Self::Cleaned => "cleaned",
            Self::Settled => "settled",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|phase| phase.as_str() == text)
    }
}

/// Returned by the test seam when a simulated process death stopped a rotation. Nothing after the
/// named phase ran: no rollback, no cleanup, no resume.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Killed(pub Phase);

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RekeyStep {
    /// Old key to new key.
    Forward,
    /// New key back to the old key (rollback).
    Rollback,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RekeyFault {
    None,
    /// Fail before the database is touched.
    FailBefore,
    /// Apply the rekey, then report an error (the commit happened anyway).
    FailAfter,
}

/// Fault injection for tests. Production code never implements it. A simulated death is
/// [`Faults::die_after`] returning `true` for a phase: the rotation stops there exactly as a dead
/// process would, leaving the journal, the key storage and the database as they are.
#[doc(hidden)]
pub trait Faults: Send + Sync {
    fn die_after(&self, phase: Phase) -> bool {
        let _ = phase;
        false
    }

    fn rekey(&self, step: RekeyStep) -> RekeyFault {
        let _ = step;
        RekeyFault::None
    }

    /// Runs after the rekey and before the database is read back.
    fn before_readback(&self, db: &Path) {
        let _ = db;
    }

    /// Runs while read-pool admission holds the live-state lock, before registry insertion.
    fn before_pool_registration(&self) {}
}

pub(super) struct NoFaults;

impl Faults for NoFaults {}

// ----- requests and reports -------------------------------------------------------------------

/// What to rotate to.
pub struct RotationRequest {
    /// The next key. It must be a valid key for this crate (`DbOptions::key` charset).
    pub new_key: Key,
    /// Keep the previous key in the recovery slot after the rotation settles (for backups made
    /// under it). Only the immediately previous key is kept. Default `false`.
    pub retain_previous: bool,
}

impl RotationRequest {
    pub fn new(new_key: impl Into<String>) -> Self {
        Self {
            new_key: Key::new(new_key.into()),
            retain_previous: false,
        }
    }

    pub fn retaining_previous(mut self) -> Self {
        self.retain_previous = true;
        self
    }
}

impl fmt::Debug for RotationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RotationRequest")
            .field("new_key", &"<redacted>")
            .field("retain_previous", &self.retain_previous)
            .finish()
    }
}

/// How a rotation ended, classified from readback and never from an error value alone.
#[derive(Debug)]
#[non_exhaustive]
pub enum RotationReport {
    /// The database and protected storage agree on the new key and cleanup finished.
    Committed,
    /// The new key is committed (database and primary key) but a cleanup step failed: a pending
    /// key, the recovery slot or the journal remains. Nothing is rolled back; `recover_rotation`
    /// finishes the cleanup.
    CommittedCleanupPending { pending: Vec<String> },
    /// The old key is still the key in the database and in protected storage. `error` says why;
    /// `cleanup_pending` lists staged items that could not be removed (recovery removes them). A
    /// `Busy` error means nothing was touched.
    NotCommitted {
        error: LifecycleError,
        cleanup_pending: Vec<String>,
    },
    /// The state could not be classified (for example the database could not be read back, or the
    /// primary key slot could not be read after promotion). Both viable keys are retained, ordinary
    /// writes stay blocked, and `recover_rotation` decides from the real state.
    Indeterminate { detail: String },
}

impl RotationReport {
    pub fn is_committed(&self) -> bool {
        matches!(self, Self::Committed | Self::CommittedCleanupPending { .. })
    }
}

fn not_committed(error: LifecycleError) -> RotationReport {
    RotationReport::NotCommitted {
        error,
        cleanup_pending: Vec::new(),
    }
}

/// What recovery changed.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryStep {
    /// A pending key was discarded (the key that opens the file was already canonical).
    PendingDiscarded,
    /// The pending key opened the file and was promoted to primary.
    PendingPromoted,
    /// A legacy staging key was the sole viable key and was moved to the canonical slot.
    LegacyStagingMigrated {
        alias: String,
    },
    /// The legacy staging item was deleted after canonical storage and a reopen readback.
    LegacyStagingDeleted {
        alias: String,
    },
    /// The recovery slot's key was the sole viable key and was restored as primary.
    PrimaryRestoredFromRecovery,
    /// The recovery slot was cleared after the rotation it protected was proven consistent.
    RecoverySlotCleared,
    JournalCleared,
    /// An interrupted restore was finished through `recover_interrupted_restore_locked`.
    RestoreRecovered(InterruptedRestore),
    /// An interrupted relocation was abandoned: the old root stays the store; a copy at the new
    /// root, if any, is retained.
    RelocationAbandoned {
        old_root: PathBuf,
        new_root: Option<PathBuf>,
    },
}

/// Why recovery changed nothing and ordinary use stays blocked.
#[derive(Debug)]
#[non_exhaustive]
pub enum BlockedReason {
    StoreMissing,
    NotEncrypted,
    KeyMissing,
    /// A key slot could not be read, so a key that would open the file may exist unseen.
    KeyStoreUnavailable {
        slot: String,
        error: KeyStoreError,
    },
    /// Keys are stored and none opens the file (wrong key or damaged file).
    NoKeyOpens {
        tried: Vec<KeySource>,
    },
    DatabaseUnavailable(DbError),
    Restore(String),
    Journal(String),
    Busy(String),
    Other(String),
}

impl BlockedReason {
    pub(super) fn into_error(self, path: &Path) -> LifecycleError {
        match self {
            Self::StoreMissing => LifecycleError::StoreMissing {
                path: path.to_path_buf(),
            },
            Self::NotEncrypted => LifecycleError::NotEncrypted {
                path: path.to_path_buf(),
            },
            Self::KeyMissing => LifecycleError::KeyMissing,
            Self::KeyStoreUnavailable { slot, error } => LifecycleError::key_store(slot, error),
            Self::NoKeyOpens { tried } => LifecycleError::NoKeyOpens {
                path: path.to_path_buf(),
                tried,
            },
            Self::DatabaseUnavailable(error) => db_unavailable(path, error),
            Self::Restore(detail) | Self::Journal(detail) | Self::Other(detail) => {
                LifecycleError::RecoveryRequired { detail }
            }
            Self::Busy(reason) => LifecycleError::Busy { reason },
        }
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum RecoveryOutcome {
    /// Nothing needed doing: no journal, no leftovers, the primary key opens the file.
    Consistent,
    /// Leftovers were reconciled and the store is consistent.
    Reconciled,
    /// The store is consistent but some cleanup failed (`RecoveryReport::pending_cleanup`).
    CleanupPending,
    /// The file opens with a key from `opened_with`, but key storage could not be made canonical.
    /// The store is usable and the retained keys are untouched.
    Degraded,
    /// Nothing was changed and ordinary use stays blocked.
    Blocked(BlockedReason),
}

#[derive(Debug)]
pub struct RecoveryReport {
    pub outcome: RecoveryOutcome,
    /// Which stored key opened the file.
    pub opened_with: Option<KeySource>,
    pub steps: Vec<RecoveryStep>,
    /// Cleanup that failed and remains (the journal stays so a later recovery retries it).
    pub pending_cleanup: Vec<String>,
}

impl RecoveryReport {
    fn new() -> Self {
        Self {
            outcome: RecoveryOutcome::Consistent,
            opened_with: None,
            steps: Vec::new(),
            pending_cleanup: Vec::new(),
        }
    }

    fn blocked(reason: BlockedReason) -> Self {
        let mut report = Self::new();
        report.outcome = RecoveryOutcome::Blocked(reason);
        report
    }

    pub(super) fn from_error(error: LifecycleError) -> Self {
        let reason = if error.is_busy() {
            BlockedReason::Busy(error.to_string())
        } else {
            BlockedReason::Other(error.to_string())
        };
        Self::blocked(reason)
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self.outcome, RecoveryOutcome::Blocked(_))
    }
}

// ----- the public operations ------------------------------------------------------------------

impl EncryptedStore {
    /// Rotate the database key. See the module documentation for the phases and the
    /// classification. `Busy` (the host or a reader or writer would not drain, or another process
    /// holds the guard) is returned as `NotCommitted` before anything changed.
    pub fn rotate_key(&self, request: RotationRequest) -> RotationReport {
        match rotate_inner(self, request, &NoFaults) {
            Ok(report) => report,
            Err(Killed(phase)) => RotationReport::Indeterminate {
                detail: format!("stopped after {}", phase.as_str()),
            },
        }
    }

    /// [`EncryptedStore::rotate_key`] with the test seam: a simulated process death or an injected
    /// rekey fault. Not part of the supported API.
    #[doc(hidden)]
    pub fn rotate_key_with_faults(
        &self,
        request: RotationRequest,
        faults: &dyn Faults,
    ) -> Result<RotationReport, Killed> {
        rotate_inner(self, request, faults)
    }

    /// Reconcile key storage with the database after an interrupted or partial rotation, an
    /// interrupted restore or relocation, or a legacy staging key. Safe to call any time; with
    /// nothing to do it reports `Consistent`. The store is drained for the call and reopened after.
    pub fn recover_rotation(&self) -> RecoveryReport {
        let cfg = self.config();
        if let Err(error) = self.drain(TransitionKind::Recover, true) {
            return RecoveryReport::from_error(error);
        }
        let report = match recover_with_guard(&cfg, &NoFaults) {
            Ok(report) => report,
            Err(error) => RecoveryReport::from_error(error),
        };
        // A failed reopen leaves the store blocked with the reason.
        let _ = self.resume();
        report
    }
}

/// Recover a store that cannot be opened (for example after an indeterminate rotation) without
/// opening it: takes the transition guard, reconciles, releases. Fails as `Blocked(Busy)` when
/// another opener holds the store.
pub fn recover_rotation(config: &EncryptedStoreConfig) -> RecoveryReport {
    let config = match config.clone().checked() {
        Ok(config) => config,
        Err(error) => return RecoveryReport::from_error(error),
    };
    match recover_with_guard(&config, &NoFaults) {
        Ok(report) => report,
        Err(error) => RecoveryReport::from_error(error),
    }
}

// ----- rotation -------------------------------------------------------------------------------

enum End {
    Committed(Vec<String>),
    NotCommitted(LifecycleError, Vec<String>),
    Indeterminate(String),
}

fn rotate_inner(
    store: &EncryptedStore,
    request: RotationRequest,
    faults: &dyn Faults,
) -> Result<RotationReport, Killed> {
    if let Err(error) = crate::validate_key(Some(request.new_key.as_str())) {
        return Ok(not_committed(LifecycleError::Db(error)));
    }
    let cfg = store.config();
    let (old, source, degraded) = match store.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => return Ok(not_committed(error)),
    };
    if source != KeySource::Primary {
        return Ok(not_committed(LifecycleError::Precondition(
            "the store was opened with a key that is not the canonical primary key; call recover_rotation first"
                .into(),
        )));
    }
    if let Some(note) = degraded {
        return Ok(not_committed(LifecycleError::Precondition(format!(
            "key storage has unfinished cleanup ({note}); call recover_rotation first"
        ))));
    }
    if old.as_str() == request.new_key.as_str() {
        return Ok(not_committed(LifecycleError::Precondition(
            "the new key equals the current key".into(),
        )));
    }
    if let Err(error) = store.drain(TransitionKind::Rotate, false) {
        return Ok(not_committed(error));
    }
    let transition = match StoreTransition::begin(
        TransitionKind::Rotate,
        &[cfg.db.path.as_path()],
        &cfg.transition,
    ) {
        Ok(transition) => transition,
        Err(error) => {
            let _ = store.resume();
            return Ok(not_committed(error));
        }
    };
    let rotation = match cfg.key_store.lock_rotation(cfg.transition.acquire_timeout) {
        Ok(lock) => lock,
        Err(error) => {
            drop(transition);
            let _ = store.resume();
            return Ok(not_committed(key_store_error("rotation lock", error)));
        }
    };
    let journal = Journal {
        kind: TransitionKind::Rotate,
        state: JournalState::Active,
        phase: Phase::Guarded.as_str().to_string(),
        retain_previous: request.retain_previous,
        pending: None,
        id: next_id(),
        old_root: RootIdentity::capture(&cfg.db.path),
        new_root: None,
    };
    let mut run = Run {
        cfg: &cfg,
        ks: rotation,
        faults,
        tr: transition,
        path: cfg.db.effective_path(),
        old,
        new: request.new_key.clone(),
        retain: request.retain_previous,
        journal,
        rev: None,
        prior_recovery: None,
        recovery_written: false,
        pending_ours: false,
        cleanup: Vec::new(),
    };
    // A simulated death returns here with the guard released and nothing else done.
    let end = run.execute()?;
    drop(run);
    Ok(match end {
        End::Indeterminate(detail) => {
            store.block(detail.clone());
            RotationReport::Indeterminate { detail }
        }
        End::NotCommitted(error, cleanup_pending) => {
            let _ = store.resume();
            RotationReport::NotCommitted {
                error,
                cleanup_pending,
            }
        }
        End::Committed(pending) => {
            let _ = store.resume();
            if pending.is_empty() {
                RotationReport::Committed
            } else {
                RotationReport::CommittedCleanupPending { pending }
            }
        }
    })
}

struct Run<'a> {
    cfg: &'a EncryptedStoreConfig,
    // Owned session drops before `tr`, including interruption and rollback paths.
    ks: Box<dyn ProtectedKeySession + Send + 'static>,
    faults: &'a dyn Faults,
    tr: StoreTransition,
    path: PathBuf,
    old: Key,
    new: Key,
    retain: bool,
    journal: Journal,
    rev: Option<PendingRevision>,
    /// What the recovery slot held before this rotation overwrote it.
    prior_recovery: Option<Key>,
    recovery_written: bool,
    /// The pending slot was empty before this rotation staged into it, so anything in it is ours.
    pending_ours: bool,
    cleanup: Vec<String>,
}

impl<'a> Run<'a> {
    fn persist(&mut self, phase: Phase) -> Result<(), LifecycleError> {
        self.journal.phase = phase.as_str().to_string();
        self.tr.write_journal(&self.journal)
    }

    fn reached(&self, phase: Phase) -> Result<(), Killed> {
        if self.faults.die_after(phase) {
            Err(Killed(phase))
        } else {
            Ok(())
        }
    }

    fn probe_key(&self, key: &Key) -> Probe {
        probe(
            &self.cfg.db,
            &self.path,
            key,
            IntegrityCheck::Quick,
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
    }

    /// Undo what was staged and end as `NotCommitted`. Only called while the primary key still
    /// opens the database, so every staged item is redundant.
    fn abort(&mut self, error: LifecycleError) -> End {
        if matches!(&error, LifecycleError::KeyStore { source, .. }
            if source.kind == KeyStoreErrorKind::Unsettled)
        {
            return self.indeterminate(format!("protected key mutation has not settled: {error}"));
        }
        let mut left = Vec::new();
        if self.pending_ours {
            match self.ks.read_pending() {
                Ok(Some(pending)) => {
                    if let Err(discard) = self.ks.discard_pending(&pending.revision) {
                        if discard.kind == KeyStoreErrorKind::Unsettled {
                            return self.indeterminate(format!(
                                "pending cleanup has not settled: {discard}"
                            ));
                        }
                        left.push(format!("discard the staged key: {discard}"));
                    }
                }
                Ok(None) => {}
                Err(read) => left.push(format!("read the staged key: {read}")),
            }
        }
        // Failed pending cleanup cannot authorize removing the retained old key.
        if self.recovery_written && left.is_empty() {
            let restored = match &self.prior_recovery {
                Some(prior) => self.ks.write_recovery(prior),
                None => self.ks.clear_recovery(),
            };
            if let Err(restore) = restored {
                if restore.kind == KeyStoreErrorKind::Unsettled {
                    return self
                        .indeterminate(format!("recovery restoration has not settled: {restore}"));
                }
                left.push(format!("restore the recovery slot: {restore}"));
            }
        }
        if left.is_empty() {
            if let Err(clear) = self.tr.clear_journal() {
                left.push(format!("remove the transition journal: {clear}"));
            }
        }
        End::NotCommitted(error, left)
    }

    fn indeterminate(&mut self, detail: String) -> End {
        self.journal.state = JournalState::Indeterminate;
        let _ = self.tr.write_journal(&self.journal);
        End::Indeterminate(detail)
    }

    /// `PRAGMA rekey` from `from` to `to`. `Some(error)` is what the call reported; the caller
    /// reads the database back and never trusts this alone.
    fn run_rekey(&self, from: &Key, to: &Key, step: RekeyStep) -> Option<DbError> {
        let fault = self.faults.rekey(step);
        if fault == RekeyFault::FailBefore {
            return Some(DbError::Backup("injected rekey failure".into()));
        }
        let mut options = self.cfg.keyed(from);
        options.integrity = IntegrityCheck::Off;
        options.integrity_check_on_open = false;
        let conn = match options.open_existing_at(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE) {
            Ok(conn) => conn,
            Err(error) => return Some(error),
        };
        let result = crate::rekey(&conn, to.as_str());
        drop(conn);
        match result {
            Err(error) => Some(error),
            Ok(()) if fault == RekeyFault::FailAfter => {
                Some(DbError::Backup("injected failure after the rekey".into()))
            }
            Ok(()) => None,
        }
    }

    /// The database is on the new key but the primary key is still the old one (a failed
    /// validation, or a verified noncommit of the promotion): rekey back and read back.
    fn rollback(&mut self, error: LifecycleError) -> End {
        let rekey_error = self.run_rekey(&self.new, &self.old, RekeyStep::Rollback);
        match self.probe_key(&self.old) {
            Probe::Opens => match self.ks.read_primary() {
                Ok(Some(primary)) if primary.as_str() == self.old.as_str() => self.abort(error),
                Ok(_) => self.indeterminate(
                    "the primary key changed while the database was rolled back".into(),
                ),
                Err(read) => self.indeterminate(format!(
                    "the primary key could not be read back after the rollback: {read}"
                )),
            },
            _ => {
                let reported =
                    rekey_error.map_or_else(|| "no error".to_string(), |e| e.to_string());
                self.indeterminate(format!(
                    "the database could not be rolled back to the old key ({reported}); the new key stays staged as the pending key and the old key stays retained"
                ))
            }
        }
    }

    fn execute(&mut self) -> Result<End, Killed> {
        let cfg = self.cfg;

        // 1. Guarded.
        if let Err(error) = self.persist(Phase::Guarded) {
            return Ok(self.abort(error));
        }
        self.reached(Phase::Guarded)?;

        // 2. The current key opens the file and is the canonical primary key.
        match self.probe_key(&self.old) {
            Probe::Opens => {}
            Probe::KeyRejected => {
                return Ok(self.abort(LifecycleError::Precondition(
                    "the current key no longer opens the database".into(),
                )))
            }
            Probe::Other(error) => {
                let path = self.path.clone();
                return Ok(self.abort(db_unavailable(&path, error)));
            }
        }
        match self.ks.read_primary() {
            Ok(Some(primary)) if primary.as_str() == self.old.as_str() => {}
            Ok(_) => {
                return Ok(self.abort(LifecycleError::KeyStoreMismatch {
                    detail: "the primary key in protected storage differs from the key that opens the database"
                        .into(),
                }))
            }
            Err(error) => return Ok(self.abort(key_store_error("primary", error))),
        }
        if let Err(error) = self.persist(Phase::OldVerified) {
            return Ok(self.abort(error));
        }
        self.reached(Phase::OldVerified)?;

        // 3. Retain the old key durably before anything can change.
        self.prior_recovery = match self.ks.read_recovery() {
            Ok(prior) => prior,
            Err(error) => return Ok(self.abort(key_store_error("recovery", error))),
        };
        self.recovery_written = true;
        if let Err(error) = self.ks.write_recovery(&self.old) {
            return Ok(self.abort(key_store_error("recovery", error)));
        }
        match self.ks.read_recovery() {
            Ok(Some(read)) if read.as_str() == self.old.as_str() => {}
            Ok(_) => {
                return Ok(self.abort(LifecycleError::Precondition(
                    "the previous key could not be read back from the recovery slot".into(),
                )))
            }
            Err(error) => return Ok(self.abort(key_store_error("recovery", error))),
        }
        if let Err(error) = self.persist(Phase::OldRetained) {
            return Ok(self.abort(error));
        }
        self.reached(Phase::OldRetained)?;

        // 4. Stage the new key.
        match self.ks.read_pending() {
            Ok(None) => {}
            Ok(Some(_)) => {
                return Ok(self.abort(LifecycleError::Precondition(
                    "a pending key already exists; call recover_rotation first".into(),
                )))
            }
            Err(error) => return Ok(self.abort(key_store_error("pending", error))),
        }
        self.pending_ours = true;
        let revision = match self.ks.write_pending(&self.new) {
            Ok(revision) => revision,
            Err(error) => return Ok(self.abort(key_store_error("pending", error))),
        };
        self.rev = Some(revision.clone());
        match self.ks.read_pending() {
            Ok(Some(read)) if read.key.as_str() == self.new.as_str() => {}
            Ok(_) => {
                return Ok(self.abort(LifecycleError::Precondition(
                    "the staged key could not be read back".into(),
                )))
            }
            Err(error) => return Ok(self.abort(key_store_error("pending", error))),
        }
        self.journal.pending = Some(revision.0.clone());
        if let Err(error) = self.persist(Phase::NewStaged) {
            return Ok(self.abort(error));
        }
        self.reached(Phase::NewStaged)?;

        // 5. Journal the intent before the database changes.
        if let Err(error) = self.persist(Phase::Rekeying) {
            return Ok(self.abort(error));
        }
        self.reached(Phase::Rekeying)?;

        // 6. Rekey, then read the database back with both keys.
        let rekey_error = self.run_rekey(&self.old, &self.new, RekeyStep::Forward);
        self.faults.before_readback(&self.path);
        match self.probe_key(&self.new) {
            Probe::Opens => {}
            Probe::KeyRejected => {
                return Ok(match self.probe_key(&self.old) {
                    Probe::Opens => {
                        let error = match rekey_error {
                            Some(error) => LifecycleError::Db(error),
                            None => LifecycleError::Precondition(
                                "the rekey reported success but had no effect".into(),
                            ),
                        };
                        self.abort(error)
                    }
                    Probe::KeyRejected => self.indeterminate(
                        "the database opens with neither the old nor the new key after the rekey"
                            .into(),
                    ),
                    Probe::Other(error) => self.indeterminate(format!(
                        "the old key could not be checked after the rekey: {error}"
                    )),
                });
            }
            Probe::Other(error) => {
                return Ok(self.indeterminate(format!(
                    "the new key could not be checked after the rekey: {error}"
                )))
            }
        }
        // From here the database holds the new key.
        let _ = self.persist(Phase::Rekeyed);
        self.reached(Phase::Rekeyed)?;

        // 7. The caller's own validation of the rekeyed store.
        if let Some(validate) = &cfg.validate {
            let options = cfg.keyed(&self.new);
            if let Err(reason) = validate(&options) {
                return Ok(self.rollback(LifecycleError::ValidationFailed(reason)));
            }
        }

        // 8. Promote the exact staged revision, then let the primary slot's readback decide.
        let Some(revision) = self.rev.clone() else {
            return Ok(self.indeterminate("the staged revision was lost".into()));
        };
        let promoted = self.ks.promote_pending(&revision);
        if let Err(error) = &promoted {
            if error.kind == KeyStoreErrorKind::Unsettled {
                return Ok(self.indeterminate(format!("promotion has not settled: {error}")));
            }
        }
        match self.ks.read_primary() {
            Ok(Some(primary)) if primary.as_str() == self.new.as_str() => {}
            Ok(Some(primary)) if primary.as_str() == self.old.as_str() => {
                let error = match promoted {
                    Err(error) => key_store_error("promotion", error),
                    Ok(()) => LifecycleError::Precondition(
                        "promotion reported success but the primary key did not change".into(),
                    ),
                };
                return Ok(self.rollback(error));
            }
            Ok(_) => {
                return Ok(self.indeterminate(
                    "the primary key slot holds an unexpected value after promotion".into(),
                ))
            }
            Err(error) => {
                return Ok(self.indeterminate(format!(
                    "the primary key could not be read back after promotion: {error}"
                )))
            }
        }
        let _ = self.persist(Phase::Promoted);
        self.reached(Phase::Promoted)?;

        // 9. Prove consistency from what storage returns, then clean up.
        let canonical = match self.ks.read_primary() {
            Ok(Some(primary)) if primary.as_str() == self.new.as_str() => primary,
            Ok(_) => {
                return Ok(self.indeterminate("the primary key changed after promotion".into()))
            }
            Err(error) => {
                return Ok(self.indeterminate(format!(
                    "the primary key could not be read for the final check: {error}"
                )))
            }
        };
        match self.probe_key(&canonical) {
            Probe::Opens => {}
            Probe::KeyRejected => {
                return Ok(
                    self.indeterminate("the stored primary key does not open the database".into())
                )
            }
            Probe::Other(error) => {
                return Ok(self.indeterminate(format!(
                    "the final readback could not open the database: {error}"
                )))
            }
        }
        // Cleanup from here never rolls back.
        match self.ks.read_pending() {
            Ok(None) => {}
            Ok(Some(pending)) => {
                if let Err(error) = self.ks.discard_pending(&pending.revision) {
                    if error.kind == KeyStoreErrorKind::Unsettled {
                        return Ok(
                            self.indeterminate(format!("pending cleanup has not settled: {error}"))
                        );
                    }
                    self.cleanup
                        .push(format!("discard the pending key: {error}"));
                }
            }
            Err(error) => self.cleanup.push(format!("read the pending key: {error}")),
        }
        self.journal.state = JournalState::CommittedCleanup;
        let _ = self.persist(Phase::Cleaned);
        self.reached(Phase::Cleaned)?;
        if !self.retain {
            if let Err(error) = self.ks.clear_recovery() {
                if error.kind == KeyStoreErrorKind::Unsettled {
                    return Ok(
                        self.indeterminate(format!("recovery cleanup has not settled: {error}"))
                    );
                }
                self.cleanup
                    .push(format!("clear the recovery slot: {error}"));
            }
        }
        if self.cleanup.is_empty() {
            if let Err(error) = self.tr.clear_journal() {
                self.cleanup
                    .push(format!("remove the transition journal: {error}"));
            }
        }
        self.reached(Phase::Settled)?;
        Ok(End::Committed(std::mem::take(&mut self.cleanup)))
    }
}

// ----- recovery -------------------------------------------------------------------------------

/// Take the exclusive guard, reconcile, release. `Err` only when the guard or the rotation lock
/// could not be taken (nothing changed); every other outcome is in the report.
pub(super) fn recover_with_guard(
    cfg: &EncryptedStoreConfig,
    faults: &dyn Faults,
) -> Result<RecoveryReport, LifecycleError> {
    let transition = StoreTransition::begin(
        TransitionKind::Recover,
        &[cfg.db.path.as_path()],
        &cfg.transition,
    )?;
    let report = recover_locked(cfg, faults);
    transition.settle();
    report
}

fn clear_journals(roots: &[&Path]) -> Result<(), LifecycleError> {
    let mut first = None;
    for root in roots {
        if let Err(error) = remove_journal(root) {
            first.get_or_insert(error);
        }
    }
    first.map_or(Ok(()), Err)
}

fn abandon_relocation(
    cfg: &EncryptedStoreConfig,
    journal: &Journal,
) -> Result<RecoveryStep, LifecycleError> {
    let mut roots = vec![journal.old_root.path.clone()];
    if let Some(new_root) = &journal.new_root {
        roots.push(new_root.path.clone());
    }
    roots.sort();
    roots.dedup();
    let lock_options = cfg.transition.lock_options(TransitionKind::Recover);
    let mut guards = Vec::new();
    for root in roots.iter().filter(|root| **root != cfg.db.path) {
        match StoreLock::exclusive(root, &lock_options) {
            Ok(guard) => guards.push(guard),
            Err(error) => {
                let mapped = map_lock_error(error, "the other relocation root is in use");
                if mapped.is_busy() {
                    return Err(mapped);
                }
                // A root that cannot hold a lock file (its directory is gone) holds no opener.
            }
        }
    }
    let paths: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    clear_journals(&paths)?;
    drop(guards);
    Ok(RecoveryStep::RelocationAbandoned {
        old_root: journal.old_root.path.clone(),
        new_root: journal.new_root.as_ref().map(|root| root.path.clone()),
    })
}

/// Stage `key` (unless `staged` already names it) and promote it to primary; the primary slot's
/// readback is the only success.
pub(super) fn make_primary(
    ks: &mut dyn ProtectedKeySession,
    key: &Key,
    staged: Option<&PendingRevision>,
) -> Result<(), KeyStoreError> {
    let revision = match staged {
        Some(revision) => revision.clone(),
        None => ks.write_pending(key)?,
    };
    let promoted = ks.promote_pending(&revision);
    if let Err(error) = &promoted {
        if error.kind == KeyStoreErrorKind::Unsettled {
            return Err(error.clone());
        }
    }
    match ks.read_primary() {
        Ok(Some(primary)) if primary.as_str() == key.as_str() => Ok(()),
        Ok(_) => Err(promoted
            .err()
            .unwrap_or_else(|| KeyStoreError::other("promotion did not change the primary key"))),
        Err(error) => Err(error),
    }
}

fn blocked_from_scan(scan: Scan) -> BlockedReason {
    if let Some(error) = scan.db_error {
        return BlockedReason::DatabaseUnavailable(error);
    }
    if let Some((slot, error)) = scan.failure {
        return BlockedReason::KeyStoreUnavailable { slot, error };
    }
    if scan.candidates == 0 {
        return BlockedReason::KeyMissing;
    }
    BlockedReason::NoKeyOpens { tried: scan.tried }
}

/// Reconcile under a held guard. Takes the key store's rotation lock (guard, then rotation lock,
/// then database).
pub(super) fn recover_locked(
    cfg: &EncryptedStoreConfig,
    faults: &dyn Faults,
) -> Result<RecoveryReport, LifecycleError> {
    let _ = faults;
    let mut session = cfg
        .key_store
        .lock_rotation(cfg.transition.acquire_timeout)
        .map_err(|error| key_store_error("rotation lock", error))?;
    let ks = &mut *session;
    let mut report = RecoveryReport::new();

    // An interrupted restore decides which file the keys must open, so it goes first.
    match crate::recover_interrupted_restore_locked(&cfg.db) {
        Ok(Some(restored)) => report.steps.push(RecoveryStep::RestoreRecovered(restored)),
        Ok(None) => {}
        Err(error) => {
            report.outcome = RecoveryOutcome::Blocked(BlockedReason::Restore(error.to_string()));
            return Ok(report);
        }
    }

    let mut journal = match read_journal(&cfg.db.path) {
        Ok(journal) => journal,
        Err(error) => {
            report.outcome = RecoveryOutcome::Blocked(BlockedReason::Journal(error.to_string()));
            return Ok(report);
        }
    };
    let relocation = journal
        .as_ref()
        .filter(|journal| journal.kind == TransitionKind::Relocate)
        .cloned();
    if let Some(relocation) = relocation {
        match abandon_relocation(cfg, &relocation) {
            Ok(step) => report.steps.push(step),
            Err(error) => {
                report.outcome = RecoveryOutcome::Blocked(BlockedReason::Busy(error.to_string()));
                return Ok(report);
            }
        }
        journal = None;
    }

    let path = cfg.db.effective_path();
    match file_present(&path) {
        Ok(true) => {}
        Ok(false) => {
            report.outcome = RecoveryOutcome::Blocked(BlockedReason::StoreMissing);
            return Ok(report);
        }
        Err(error) => {
            report.outcome = RecoveryOutcome::Blocked(BlockedReason::Other(error.to_string()));
            return Ok(report);
        }
    }
    if is_plaintext_sqlite(&path) {
        report.outcome = RecoveryOutcome::Blocked(BlockedReason::NotEncrypted);
        return Ok(report);
    }

    let mut scan = scan_candidates(cfg, &path, ks);
    let Some((source, key)) = scan.opened.take() else {
        report.outcome = RecoveryOutcome::Blocked(blocked_from_scan(scan));
        return Ok(report);
    };
    report.opened_with = Some(source.clone());
    let slots = &scan.slots;
    let mut degraded: Option<String> = None;

    // Make the key that opens the file the canonical primary key.
    match &source {
        KeySource::Primary => {}
        KeySource::Pending => {
            if let PendingRead::Key(pending) = &slots.pending {
                match make_primary(ks, &key, Some(&pending.revision)) {
                    Ok(()) => report.steps.push(RecoveryStep::PendingPromoted),
                    Err(reason) => degraded = Some(reason.to_string()),
                }
            }
        }
        KeySource::LegacyStaging(_) | KeySource::Recovery => {
            // A pending key that does not open the file is dead weight: the file is not on it.
            if let PendingRead::Key(stale) = &slots.pending {
                if let Err(error) = ks.discard_pending(&stale.revision) {
                    degraded = Some(format!(
                        "the stale pending key could not be discarded: {error}"
                    ));
                }
            }
            if degraded.is_none() {
                match make_primary(ks, &key, None) {
                    Ok(()) => report.steps.push(match &source {
                        KeySource::LegacyStaging(alias) => RecoveryStep::LegacyStagingMigrated {
                            alias: alias.clone(),
                        },
                        _ => RecoveryStep::PrimaryRestoredFromRecovery,
                    }),
                    Err(reason) => degraded = Some(reason.to_string()),
                }
            }
        }
    }

    if degraded.is_none() {
        // Leftover pending key: the file opens with the primary key, so it is redundant. The
        // guard and the rotation lock are held, so no rotation of ours is using it.
        match &source {
            KeySource::Pending => {
                // Promotion may have left the pending marker behind.
                match ks.read_pending() {
                    Ok(Some(pending)) => match ks.discard_pending(&pending.revision) {
                        Ok(()) => report.steps.push(RecoveryStep::PendingDiscarded),
                        Err(error) => report
                            .pending_cleanup
                            .push(format!("discard the pending key: {error}")),
                    },
                    Ok(None) => {}
                    Err(error) => report
                        .pending_cleanup
                        .push(format!("read the pending key: {error}")),
                }
            }
            _ => match &slots.pending {
                PendingRead::Key(pending) => match ks.discard_pending(&pending.revision) {
                    Ok(()) => report.steps.push(RecoveryStep::PendingDiscarded),
                    Err(error) => report
                        .pending_cleanup
                        .push(format!("discard the pending key: {error}")),
                },
                PendingRead::Failed(error) => report
                    .pending_cleanup
                    .push(format!("read the pending key: {error}")),
                PendingRead::Absent => {}
            },
        }

        // A legacy staging item that holds the key which now opens the file goes only after
        // canonical storage and a reopen readback prove recovery stays possible without it. This
        // also retries a deletion that failed earlier. An item holding any other key is left alone.
        if report.pending_cleanup.is_empty() && cfg.delete_legacy_after_migration {
            let holders: Vec<&String> = slots
                .legacy
                .iter()
                .filter(|(_, slot)| matches!(slot, SlotRead::Key(held) if held.as_str() == key.as_str()))
                .map(|(alias, _)| alias)
                .collect();
            if !holders.is_empty() {
                let reopened = match ks.read_primary() {
                    Ok(Some(primary)) => {
                        primary.as_str() == key.as_str()
                            && matches!(
                                probe(
                                    &cfg.db,
                                    &path,
                                    &primary,
                                    IntegrityCheck::Quick,
                                    OpenFlags::SQLITE_OPEN_READ_WRITE
                                ),
                                Probe::Opens
                            )
                    }
                    _ => false,
                };
                for alias in holders {
                    if !reopened {
                        report.pending_cleanup.push(format!(
                            "the legacy staging item {alias} was kept: the canonical key did not reopen the database"
                        ));
                        continue;
                    }
                    match ks.delete_legacy_staging(alias) {
                        Ok(()) => report.steps.push(RecoveryStep::LegacyStagingDeleted {
                            alias: alias.clone(),
                        }),
                        Err(error) => report
                            .pending_cleanup
                            .push(format!("delete the legacy staging item {alias}: {error}")),
                    }
                }
            }
        }

        // The recovery slot protected a rotation that is now proven consistent (or holds the key
        // that was just restored as primary), so it is redundant.
        let rotation_settled = journal.as_ref().is_some_and(|journal| {
            journal.kind == TransitionKind::Rotate
                && !journal.retain_previous
                && Phase::parse(&journal.phase).is_some_and(|phase| phase >= Phase::OldRetained)
        });
        let from_recovery = matches!(source, KeySource::Recovery);
        if report.pending_cleanup.is_empty()
            && (rotation_settled || from_recovery)
            && matches!(slots.recovery, SlotRead::Key(_))
        {
            match ks.clear_recovery() {
                Ok(()) => report.steps.push(RecoveryStep::RecoverySlotCleared),
                Err(error) => report
                    .pending_cleanup
                    .push(format!("clear the recovery slot: {error}")),
            }
        }

        if report.pending_cleanup.is_empty() && journal.is_some() {
            match clear_journals(&[cfg.db.path.as_path()]) {
                Ok(()) => report.steps.push(RecoveryStep::JournalCleared),
                Err(error) => report
                    .pending_cleanup
                    .push(format!("remove the transition journal: {error}")),
            }
        }
    }

    report.outcome = if degraded.is_some() {
        if let Some(reason) = degraded {
            report.pending_cleanup.push(reason);
        }
        RecoveryOutcome::Degraded
    } else if !report.pending_cleanup.is_empty() {
        RecoveryOutcome::CleanupPending
    } else if report.steps.is_empty() {
        RecoveryOutcome::Consistent
    } else {
        RecoveryOutcome::Reconciled
    };
    Ok(report)
}
