//! Opt-in rebuild of an encrypted database that its key cannot open: the database file is there,
//! and either no primary key is stored or the stored one does not open it.
//!
//! Without a [`RebuildPolicy`] none of this runs and `EncryptedStore::open` keeps returning
//! `KeyMissing` / `NoKeyOpens` and touching nothing. With one, the order is fixed and every step is
//! safe to repeat after a crash:
//!
//! 1. record the intent (`<db>.rebuild-journal`, the reason only; no file or key changes);
//! 2. mint a replacement key and persist it as the primary key, read back (a stored primary key
//!    that is usable is reused instead: the credential is fine, the file is not its file);
//! 3. quarantine the unreadable file and its sidecars (renamed beside it, never deleted, by the
//!    crate's journaled quarantine);
//! 4. create the new database under the key;
//! 5. remove the intent record.
//!
//! The key is persisted before the file moves: if persisting fails nothing moved and the next open
//! retries, and if the move fails the key is already stored. The next open finds the intent record,
//! settles a half-done quarantine (`resume_quarantine`) and continues from the real state.
//! Nothing here ever deletes a quarantined file.

use super::error::LifecycleError;
use super::keystore::Key;
use super::rotate::{make_primary, Faults};
use super::store::{
    file_present, is_plaintext_sqlite, key_is_usable, key_store_error, scan_candidates,
    EncryptedStoreConfig, SlotRead,
};
use super::transition::{journal_exists, StoreTransition, TransitionKind};
use crate::recover::{
    quarantine_database_journaled_hooked, resume_quarantine, with_suffix, QuarantineResume,
    QuarantineStep, QUARANTINE_JOURNAL_SUFFIX,
};
use crate::{QuarantineStamp, SidecarPolicy};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const JOURNAL_SUFFIX: &str = ".rebuild-journal";
const JOURNAL_MAGIC: &str = "rightkit-db-rebuild.v1";

/// Mints the replacement key (a valid `DbOptions::key` passphrase). `Err(reason)` stops the rebuild
/// before anything changes.
pub type MintKey = Arc<dyn Fn() -> Result<Key, String> + Send + Sync>;

/// Why a database file is being replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RebuildReason {
    /// The file is there and no primary key is stored (the file outlived its key, for example a
    /// profile copied without its credentials).
    KeyAbsent,
    /// A primary key is stored, and neither it nor any other stored key opens the file (a file from
    /// another install, or damage; SQLCipher cannot tell them apart).
    KeyRejected,
}

impl RebuildReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KeyAbsent => "key-absent",
            Self::KeyRejected => "key-rejected",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "key-absent" => Some(Self::KeyAbsent),
            "key-rejected" => Some(Self::KeyRejected),
            _ => None,
        }
    }
}

/// What a store does when its database file is there but its key cannot open it. Opt-in, per
/// reason; set it with `EncryptedStoreConfig::with_rebuild`. A rebuild moves the old file aside
/// (it is never deleted by this crate), so enabling it means a user whose key is lost gets a
/// working empty store plus the old file, instead of an error on every launch.
///
/// A rebuild never starts while a transition journal (rotation, restore, relocation) is unresolved,
/// while key storage cannot be read, when the file is plaintext SQLite, or when the file merely
/// cannot be opened for a reason that says nothing about the key (permission, lock, I/O).
///
/// Enable both reasons unless the app has a different way out of a half-finished rebuild: a crash
/// after the key was persisted leaves a stored key that does not open the file, which a store with
/// only `on_key_absent` set reads as an error until its intent record (kept for exactly this) lets
/// the next open continue; that record is honoured whatever the switches say.
#[derive(Clone)]
pub struct RebuildPolicy {
    /// Rebuild when the file is there and no primary key is stored.
    pub on_key_absent: bool,
    /// Rebuild when a primary key is stored and no stored key opens the file.
    pub on_key_rejected: bool,
    /// Mints the replacement key when none can be reused.
    pub mint_key: MintKey,
    /// Name of the quarantined file. Default `.corrupt-<unix-ms>`.
    pub quarantine_stamp: QuarantineStamp,
}

impl RebuildPolicy {
    /// A policy with every reason off; turn them on with `with_key_absent` / `with_key_rejected`.
    pub fn new(mint_key: impl Fn() -> Result<Key, String> + Send + Sync + 'static) -> Self {
        Self {
            on_key_absent: false,
            on_key_rejected: false,
            mint_key: Arc::new(mint_key),
            quarantine_stamp: QuarantineStamp::default(),
        }
    }

    pub fn with_key_absent(mut self, on: bool) -> Self {
        self.on_key_absent = on;
        self
    }

    pub fn with_key_rejected(mut self, on: bool) -> Self {
        self.on_key_rejected = on;
        self
    }

    pub fn with_quarantine_stamp(mut self, stamp: QuarantineStamp) -> Self {
        self.quarantine_stamp = stamp;
        self
    }

    fn allows(&self, reason: RebuildReason) -> bool {
        match reason {
            RebuildReason::KeyAbsent => self.on_key_absent,
            RebuildReason::KeyRejected => self.on_key_rejected,
        }
    }
}

/// What a rebuild did. Carries no key material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebuildReport {
    pub reason: RebuildReason,
    /// Where the unreadable main file now is (its sidecars sit beside it under the same suffix).
    /// `None` only when a crash left no record of the name after the move: the file is the
    /// `<db>.corrupt-*` sibling.
    pub quarantined: Option<PathBuf>,
    /// `true` when a new key was minted and stored; `false` when the stored primary key was reused.
    pub minted_key: bool,
}

/// The points of a rebuild where a test can stop the process.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebuildStep {
    /// The intent record is written.
    Journaled,
    /// The key is minted, not yet stored.
    Minted,
    /// The key is stored as the primary key (read back).
    Persisted,
    /// The quarantine journal is written; no file moved.
    QuarantineRecorded,
    /// The main file moved; the sidecars did not.
    MainMoved,
    /// The old file and its sidecars are all aside.
    Quarantined,
    /// The new database exists; the intent record is still there.
    Created,
}

fn simulated(step: RebuildStep) -> LifecycleError {
    LifecycleError::Precondition(format!("simulated process death after {step:?}"))
}

fn journal_path(db: &Path) -> PathBuf {
    with_suffix(db, JOURNAL_SUFFIX)
}

fn write_journal(db: &Path, reason: RebuildReason) -> Result<(), LifecycleError> {
    let body = format!("{JOURNAL_MAGIC}\nreason={}\n", reason.as_str());
    rightkit_fs::atomic_write_private(&journal_path(db), body.as_bytes())?;
    Ok(())
}

/// `Ok(None)` when there is no record; a record that cannot be read is an error (fail closed).
fn read_journal(db: &Path) -> Result<Option<RebuildReason>, LifecycleError> {
    let path = journal_path(db);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(rightkit_fs::FsError::io(&path, error).into()),
    };
    let mut lines = text.lines();
    let reason = (lines.next() == Some(JOURNAL_MAGIC))
        .then(|| lines.next()?.strip_prefix("reason="))
        .flatten()
        .and_then(RebuildReason::parse);
    reason
        .map(Some)
        .ok_or_else(|| LifecycleError::RecoveryRequired {
            detail: format!(
                "the rebuild record {} is unreadable; every file was kept",
                path.display()
            ),
        })
}

fn remove_journal(db: &Path) {
    // A leftover record is harmless: the next open sees a database its key opens and removes it.
    let _ = std::fs::remove_file(journal_path(db));
}

/// Before every open: settle what an earlier open left half done. A quarantine journal or a rebuild
/// record is looked at; with neither, nothing happens.
pub(super) fn settle_interrupted(
    cfg: &EncryptedStoreConfig,
    faults: &dyn Faults,
) -> Result<Option<RebuildReport>, LifecycleError> {
    let path = cfg.db.effective_path();
    let present = |path: PathBuf| std::fs::symlink_metadata(path).is_ok();
    if !present(with_suffix(&path, QUARANTINE_JOURNAL_SUFFIX))
        && !present(journal_path(&cfg.db.path))
    {
        return Ok(None);
    }
    rebuild(cfg, faults, false)
}

/// After `open` failed with `KeyMissing` or `NoKeyOpens`: rebuild if the policy allows it.
/// `Ok(None)` means "not eligible": the caller returns its original error and nothing changed.
pub(super) fn rebuild_after_failure(
    cfg: &EncryptedStoreConfig,
    faults: &dyn Faults,
) -> Result<Option<RebuildReport>, LifecycleError> {
    rebuild(cfg, faults, true)
}

fn rebuild(
    cfg: &EncryptedStoreConfig,
    faults: &dyn Faults,
    fresh: bool,
) -> Result<Option<RebuildReport>, LifecycleError> {
    if fresh && cfg.rebuild.is_none() {
        return Ok(None);
    }
    let transition = StoreTransition::begin(
        TransitionKind::Recover,
        &[cfg.db.path.as_path()],
        &cfg.transition,
    )?;
    let result = rebuild_locked(cfg, faults);
    transition.settle();
    result
}

fn rebuild_locked(
    cfg: &EncryptedStoreConfig,
    faults: &dyn Faults,
) -> Result<Option<RebuildReport>, LifecycleError> {
    let die = |step: RebuildStep| -> Result<(), LifecycleError> {
        if faults.die_rebuild_after(step) {
            Err(simulated(step))
        } else {
            Ok(())
        }
    };
    let path = cfg.db.effective_path();
    // Settle a quarantine an earlier run started, whatever the policy: a missing main file beside
    // a live `-wal` must never be opened.
    let mut quarantined = match resume_quarantine(&path)? {
        QuarantineResume::Completed { quarantined } => Some(quarantined),
        _ => None,
    };
    let Some(policy) = cfg.rebuild.as_ref() else {
        return Ok(None);
    };
    let recorded = read_journal(&cfg.db.path)?;
    if journal_exists(&cfg.db.path) {
        // Never rebuild beside a rotation, restore or relocation that is not resolved.
        return Ok(None);
    }
    let mut session = cfg
        .key_store
        .lock_rotation(cfg.transition.acquire_timeout)
        .map_err(|error| key_store_error("rotation lock", error))?;
    let ks = &mut *session;
    let primary = match ks.read_primary() {
        Ok(primary) => primary,
        Err(error) => return Err(key_store_error("primary", error)),
    };
    let present = file_present(&path)?;
    let reason = if present {
        if is_plaintext_sqlite(&path) {
            return Ok(None);
        }
        let scan = scan_candidates(cfg, &path, ks);
        if scan.opened.is_some() {
            // Some stored key opens it: ordinary recovery owns this, and a finished rebuild's
            // record is stale.
            if recorded.is_some() {
                remove_journal(&cfg.db.path);
            }
            return Ok(None);
        }
        if scan.db_error.is_some() || scan.failure.is_some() {
            // Not evidence about the key.
            return Ok(None);
        }
        let derived = if matches!(scan.slots.primary, SlotRead::Absent) {
            RebuildReason::KeyAbsent
        } else {
            RebuildReason::KeyRejected
        };
        match recorded {
            // An earlier run of this rebuild already decided; finish it.
            Some(reason) => reason,
            None if policy.allows(derived) => derived,
            None => return Ok(None),
        }
    } else {
        // The file is gone: only a rebuild already under way may create it.
        match recorded {
            Some(reason) => reason,
            None => return Ok(None),
        }
    };
    if recorded.is_none() {
        write_journal(&cfg.db.path, reason)?;
    }
    die(RebuildStep::Journaled)?;

    let (key, minted_key) = match primary {
        Some(key) if key_is_usable(&key) => (key, false),
        _ => {
            let key = (policy.mint_key)().map_err(|why| {
                LifecycleError::InvalidConfig(format!("the rebuild key could not be minted: {why}"))
            })?;
            if !key_is_usable(&key) {
                return Err(LifecycleError::InvalidConfig(
                    "the minted rebuild key is not a valid database key".into(),
                ));
            }
            die(RebuildStep::Minted)?;
            // A staged key of an old rotation is dead weight beside the new primary.
            if let Ok(Some(stale)) = ks.read_pending() {
                ks.discard_pending(&stale.revision)
                    .map_err(|error| key_store_error("pending", error))?;
            }
            make_primary(ks, &key, None).map_err(|error| key_store_error("primary", error))?;
            (key, true)
        }
    };
    die(RebuildStep::Persisted)?;

    if present {
        let moved = quarantine_database_journaled_hooked(
            &path,
            policy.quarantine_stamp,
            SidecarPolicy::RenameSidecars,
            &|step| {
                faults.die_rebuild_after(match step {
                    QuarantineStep::Recorded => RebuildStep::QuarantineRecorded,
                    QuarantineStep::MainMoved => RebuildStep::MainMoved,
                })
            },
        );
        match moved {
            Ok(dest) => quarantined = Some(dest),
            // Nothing moved, or the split is journaled and the next open settles it.
            Err(failure) => return Err(failure.error.into()),
        }
    }
    die(RebuildStep::Quarantined)?;

    drop(cfg.keyed(&key).open_writer()?);
    die(RebuildStep::Created)?;
    remove_journal(&cfg.db.path);
    Ok(Some(RebuildReport {
        reason,
        quarantined,
        minted_key,
    }))
}
