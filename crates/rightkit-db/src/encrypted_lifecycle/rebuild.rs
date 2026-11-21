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
//!
//! Three more things live on [`RebuildHooks`] (kept apart from the policy so its public shape is
//! that of 0.5.8), all opt-in and all host-neutral (the host supplies callbacks and names, this
//! crate knows no application):
//!
//! - a durable notice ([`RebuildHooks::with_quarantine_notice`]) that runs inside the quarantine
//!   transaction, after the quarantine journal is written and before the first file moves, so state
//!   the host binds to a quarantine is on disk before the quarantine can exist;
//! - adoption of a quarantine record the host kept in its own format before this crate journaled
//!   quarantines ([`RebuildHooks::with_legacy_journal`]), finished or undone before anything opens;
//! - quarantine of orphan sidecars ([`RebuildHooks::with_orphan_sidecars`]): a `-wal`, `-shm` or
//!   `-journal` with content beside a main file that is missing or empty, which SQLite would
//!   otherwise delete when it creates the new database. The move list is durable
//!   (`<db>.orphan-quarantine`), so a stop after the first move is finished by the next open.
//!
//! Order on every open, before any writer connection exists: settle every journal (the host's, the
//! quarantine journal, the rebuild record), then protect orphan sidecars. Each move path first
//! takes the exclusive guard, refuses when a rotation, restore or relocation journal owns the files,
//! reads the key store (an unreadable one stops the open), and refuses a move set that aliases or
//! leaves the database's directory, all before any file changes.

use super::error::LifecycleError;
use super::keystore::{Key, ProtectedKeySession};
use super::rotate::{make_primary, Faults};
use super::store::{
    file_present, is_plaintext_sqlite, key_is_usable, key_store_error, scan_candidates,
    EncryptedStoreConfig, SlotRead,
};
use super::transition::{journal_path as transition_journal_path, StoreTransition, TransitionKind};
use crate::recover::{
    quarantine_database_journaled_notified, recorded_quarantine, resume_quarantine, with_suffix,
    QuarantineResume, QuarantineStep, QUARANTINE_JOURNAL_SUFFIX,
};
use crate::{DbError, QuarantineStamp, SidecarPolicy};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

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

/// One file move of a quarantine: `source` is where the file was, `destination` the name it takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedMove {
    pub source: PathBuf,
    pub destination: PathBuf,
}

/// What a quarantine is about to move aside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum QuarantineKind {
    /// A database file that no key opens, with its sidecars (a rebuild).
    Database,
    /// `-wal` / `-shm` / `-journal` files with content and no main file beside them.
    OrphanSidecars,
}

/// What the quarantine notice is told. It names every file the quarantine moves, so the host can
/// record them durably before any of them moves.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct QuarantineNotice {
    /// The database path the store serves (its effective path).
    pub database: PathBuf,
    pub kind: QuarantineKind,
    /// Why the database is being replaced: `Some` for [`QuarantineKind::Database`] (`None` when an
    /// interrupted quarantine that no rebuild record explains is being finished), `None` for orphan
    /// sidecars (they are not a rebuild and carry no key verdict).
    pub reason: Option<RebuildReason>,
    /// Every move, main file first when there is one. Names are final: the quarantine takes exactly
    /// these, unless the notice is refused (nothing moves) or the process dies first (the next open
    /// notifies again with the names then chosen, so a host record is keyed by destination and
    /// tolerates a name that was never created).
    pub moves: Vec<RecordedMove>,
    /// `true` when an earlier run recorded this quarantine in `<db>.quarantine-journal` and
    /// stopped; the notice comes before the move is finished or undone.
    pub resumed: bool,
}

/// The host's durable record of a quarantine. It runs under the exclusive store guard, inside the
/// quarantine transaction, before the first file moves. `Err(reason)` abandons the quarantine with
/// every file where it was (the open then fails and the next open tries again). It may be called
/// again for the same quarantine, so it must be idempotent by `moves`' destinations.
pub type QuarantineNotify = Arc<dyn Fn(&QuarantineNotice) -> Result<(), String> + Send + Sync>;

/// A quarantine record the host kept in its own format, from before this crate journaled
/// quarantines (`<db>.quarantine-journal`). On open, before anything else, the store asks for the
/// recorded-but-unfinished moves of its database file set, finishes or undoes them, and tells the
/// host which were consumed, so the host can drop them from its record.
///
/// `pending` is called once before the exclusive store guard is taken, to learn whether anything is
/// owed (the guard is not taken for an empty record), and again under the guard, followed by
/// `settled`. A moves list may name other databases' files: only moves whose source is the store's
/// main file or one of its `-wal`, `-shm`, `-journal` sidecars are acted on.
pub trait LegacyQuarantineJournal: Send + Sync {
    /// The recorded moves not yet known to be finished. `Err` means the record exists and cannot be
    /// read: the open fails closed with every file kept.
    fn pending(&self, database: &Path) -> Result<Vec<RecordedMove>, String>;

    /// The store finished or undid `consumed` and nothing is owed for them any more. `landed` are
    /// the destinations that exist afterwards. The host removes `consumed` from its record and may
    /// list `landed` in its own ledger. `Err` fails the open; the next open finds the moves done
    /// and calls this again.
    fn settled(
        &self,
        database: &Path,
        consumed: &[RecordedMove],
        landed: &[PathBuf],
    ) -> Result<(), String>;
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
///
/// The public fields are exactly those of 0.5.8, so a struct literal written against it still
/// compiles. The quarantine hooks live in [`RebuildHooks`], set with
/// `EncryptedStoreConfig::with_rebuild_hooks`.
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

/// The quarantine hooks of an encrypted store, kept apart from [`RebuildPolicy`] so that policy
/// keeps the public shape of 0.5.8. All opt-in; set with `EncryptedStoreConfig::with_rebuild_hooks`.
/// They work with or without a rebuild policy (a policy is only needed for its
/// `quarantine_stamp`, which names orphan sidecar quarantines; without one the default stamp is
/// used).
#[derive(Clone, Default)]
pub struct RebuildHooks {
    on_quarantine: Option<QuarantineNotify>,
    orphan_sidecars: bool,
    legacy_journal: Option<Arc<dyn LegacyQuarantineJournal>>,
}

impl RebuildHooks {
    /// Every hook off.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record every quarantine durably, inside the quarantine transaction. The callback runs after
    /// `<db>.quarantine-journal` is written and before the first file moves (for orphan sidecars,
    /// after `<db>.orphan-quarantine` is written and before the first move), so app state bound to
    /// a quarantine cannot be lost to a crash: if the callback returns `Err` nothing moves; if the
    /// process dies after it, the host record exists and the next open finishes or undoes the move.
    /// An interrupted quarantine that the next open finishes is announced again before it is
    /// finished (`resumed`). See [`QuarantineNotify`].
    pub fn with_quarantine_notice(
        mut self,
        notify: impl Fn(&QuarantineNotice) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.on_quarantine = Some(Arc::new(notify));
        self
    }

    /// Move aside `-wal`, `-shm` and `-journal` files that have content when the main file is
    /// missing or empty, before the store opens. SQLite deletes a write-ahead log it finds beside a
    /// missing main file when it creates the new database, so rows committed there would be lost.
    /// The files are renamed to `<sidecar name><suffix>` with the policy's `quarantine_stamp`
    /// (`-1`, `-2`, ... when a name is taken), never deleted. The whole move list is written
    /// durably to `<db>.orphan-quarantine` first, then announced through
    /// [`Self::with_quarantine_notice`] (kind [`QuarantineKind::OrphanSidecars`]), then moved; a
    /// list found on a later open is validated, announced again (`resumed`), finished and removed,
    /// whatever this switch says. Not attempted while a transition journal is unresolved.
    pub fn with_orphan_sidecars(mut self, on: bool) -> Self {
        self.orphan_sidecars = on;
        self
    }

    /// Adopt a quarantine record the host kept itself. See [`LegacyQuarantineJournal`].
    pub fn with_legacy_journal(mut self, journal: Arc<dyn LegacyQuarantineJournal>) -> Self {
        self.legacy_journal = Some(journal);
        self
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

/// Before every open and before any writer connection exists: adopt the host's own quarantine
/// record, settle what an earlier open left half done (a quarantine journal or a rebuild record),
/// then protect orphan sidecars. The order is fixed: every journal first, orphan protection last,
/// and all of it before the store opens a writer (`rebuild_locked` also protects orphans right
/// before it creates the new database, under its own guard). With none of them present, and no
/// host record or orphan option configured, nothing happens.
pub(super) fn settle_interrupted(
    cfg: &EncryptedStoreConfig,
    faults: &dyn Faults,
) -> Result<Option<RebuildReport>, LifecycleError> {
    adopt_legacy_journal(cfg)?;
    let path = cfg.db.effective_path();
    let report = if presence(&with_suffix(&path, QUARANTINE_JOURNAL_SUFFIX))?
        || presence(&journal_path(&cfg.db.path))?
    {
        rebuild(cfg, faults, false)?
    } else {
        None
    };
    protect_orphans(cfg)?;
    Ok(report)
}

/// `Ok(true)` when anything, a dangling link included, is at `path`; `Ok(false)` only when the
/// path is NotFound. Any other error fails closed: a path that cannot be examined is never taken
/// for an absent one.
fn presence(path: &Path) -> Result<bool, LifecycleError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(fs_error(path, error)),
    }
}

const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
const RESTORE_JOURNAL_SUFFIX: &str = ".restore-journal";

fn notice_refused(why: String) -> LifecycleError {
    LifecycleError::RecoveryRequired {
        detail: format!("the quarantine notification failed: {why}; every file was kept"),
    }
}

/// Every move between `db` and its quarantined name `dest`: the main file, and each sidecar that is
/// at either end (not yet moved, or already moved by an interrupted run).
fn moves_between(db: &Path, dest: &Path) -> Result<Vec<RecordedMove>, LifecycleError> {
    let mut moves = vec![RecordedMove {
        source: db.to_path_buf(),
        destination: dest.to_path_buf(),
    }];
    for sidecar in SIDECARS {
        let (from, to) = (with_suffix(db, sidecar), with_suffix(dest, sidecar));
        if presence(&from)? || presence(&to)? {
            moves.push(RecordedMove {
                source: from,
                destination: to,
            });
        }
    }
    Ok(moves)
}

fn fs_error(path: &Path, error: std::io::Error) -> LifecycleError {
    LifecycleError::Fs(rightkit_fs::FsError::io(path, error))
}

/// Tell the host about a quarantine, when it asked to be told. `Err` means nothing may move.
fn announce(
    cfg: &EncryptedStoreConfig,
    database: &Path,
    kind: QuarantineKind,
    reason: Option<RebuildReason>,
    moves: Vec<RecordedMove>,
    resumed: bool,
) -> Result<(), LifecycleError> {
    if let Some(notify) = cfg
        .hooks
        .as_ref()
        .and_then(|hooks| hooks.on_quarantine.as_ref())
    {
        notify(&QuarantineNotice {
            database: database.to_path_buf(),
            kind,
            reason,
            moves,
            resumed,
        })
        .map_err(notice_refused)?;
    }
    Ok(())
}

// ----- authority and ownership ---------------------------------------------------------------

/// Read the credential authority before any quarantine move runs: take the key store's rotation
/// lock and read the primary slot. An unreadable key store fails the open with every file kept, so
/// a move never runs while the credential state is unknown (an empty slot is a known state). The
/// session is returned so the caller keeps the lock until its moves are done (lock order: guard,
/// then the rotation lock).
fn credential_authority(
    cfg: &EncryptedStoreConfig,
) -> Result<(Box<dyn ProtectedKeySession + Send>, Option<Key>), LifecycleError> {
    let mut session = cfg
        .key_store
        .lock_rotation(cfg.transition.acquire_timeout)
        .map_err(|error| key_store_error("rotation lock", error))?;
    let primary = session
        .read_primary()
        .map_err(|error| key_store_error("primary", error))?;
    Ok((session, primary))
}

/// `Ok(true)` when an unresolved rotation, restore or relocation journal owns the database files:
/// its recovery decides what happens to them, not a quarantine. Absent means NotFound only.
fn transition_owns(cfg: &EncryptedStoreConfig) -> Result<bool, LifecycleError> {
    let effective = cfg.db.effective_path();
    for db in [cfg.db.path.as_path(), effective.as_path()] {
        if presence(&transition_journal_path(db))?
            || presence(&with_suffix(db, RESTORE_JOURNAL_SUFFIX))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn owned_by_transition(what: &str) -> LifecycleError {
    LifecycleError::RecoveryRequired {
        detail: format!(
            "an unresolved rotation, restore or relocation journal owns the database files; {what} was not run and every file was kept"
        ),
    }
}

// ----- the move set check --------------------------------------------------------------------

fn folded_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
}

fn folded_component(component: Component<'_>) -> String {
    component.as_os_str().to_string_lossy().to_ascii_lowercase()
}

/// True when both paths sit in the same directory, written the same way (names compared ignoring
/// ASCII case, as a case-insensitive file system would).
fn same_directory(a: &Path, b: &Path) -> bool {
    match (a.parent(), b.parent()) {
        (Some(x), Some(y)) => x
            .components()
            .map(folded_component)
            .eq(y.components().map(folded_component)),
        _ => false,
    }
}

/// Refuse a move set before any file changes. Every source is the database (only when
/// `allow_main`) or one of its sidecars, in the database's directory; every destination is in that
/// directory, its name starts with the database file name and is not the database or a sidecar
/// name; no source or destination repeats; no destination equals a source (so no move can alias a
/// name onto itself or onto another move's source). Names are compared ignoring ASCII case.
fn validate_move_set(
    db: &Path,
    moves: &[RecordedMove],
    allow_main: bool,
) -> Result<(), LifecycleError> {
    let refuse = |detail: String| LifecycleError::RecoveryRequired {
        detail: format!("{detail}; the move set was refused and no file was changed"),
    };
    let db_name =
        folded_name(db).ok_or_else(|| refuse(format!("{} has no file name", db.display())))?;
    let live: Vec<String> = SIDECARS
        .iter()
        .map(|sidecar| format!("{db_name}{sidecar}"))
        .collect();
    let mut sources: Vec<String> = Vec::new();
    let mut destinations: Vec<String> = Vec::new();
    for recorded in moves {
        let (Some(source), Some(destination)) = (
            folded_name(&recorded.source),
            folded_name(&recorded.destination),
        ) else {
            return Err(refuse(format!(
                "{} -> {} names no file",
                recorded.source.display(),
                recorded.destination.display()
            )));
        };
        let is_live_source = live.contains(&source) || (allow_main && source == db_name);
        if !same_directory(&recorded.source, db) || !is_live_source {
            return Err(refuse(format!(
                "{} is not the database or one of its sidecars",
                recorded.source.display()
            )));
        }
        if !same_directory(&recorded.destination, db) {
            return Err(refuse(format!(
                "{} is outside the database directory",
                recorded.destination.display()
            )));
        }
        if !destination.starts_with(&db_name)
            || destination == db_name
            || live.contains(&destination)
            || destination == source
        {
            return Err(refuse(format!(
                "{} is not a quarantine name for {}",
                recorded.destination.display(),
                db.display()
            )));
        }
        if sources.contains(&source) {
            return Err(refuse(format!(
                "{} is the source of two moves",
                recorded.source.display()
            )));
        }
        if destinations.contains(&destination) {
            return Err(refuse(format!(
                "{} is the destination of two moves",
                recorded.destination.display()
            )));
        }
        sources.push(source);
        destinations.push(destination);
    }
    if let Some(clash) = destinations.iter().find(|name| sources.contains(name)) {
        return Err(refuse(format!(
            "{clash} is both a destination and a source of the set"
        )));
    }
    Ok(())
}

/// True when `source` and `destination` are two different entries of their directory: both names
/// are listed there. Names that a case-insensitive or normalizing file system folds into one entry
/// are listed once, so they are not two entries.
fn distinct_entries(source: &Path, destination: &Path) -> Result<bool, LifecycleError> {
    let (Some(dir), Some(from), Some(to)) =
        (source.parent(), source.file_name(), destination.file_name())
    else {
        return Ok(false);
    };
    if from == to {
        return Ok(false);
    }
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let (mut saw_from, mut saw_to) = (false, false);
    for entry in std::fs::read_dir(dir).map_err(|error| fs_error(dir, error))? {
        let entry = entry.map_err(|error| fs_error(dir, error))?;
        let name = entry.file_name();
        saw_from |= name.as_os_str() == from;
        saw_to |= name.as_os_str() == to;
    }
    Ok(saw_from && saw_to)
}

/// Settle one recorded move of a set that passed [`validate_move_set`]. Idempotent: a move is done
/// when its source is gone; a destination that is the SAME file as the source, under two distinct
/// directory entries (a link landed, the unlink did not), is finished by removing the source name;
/// two DIFFERENT files fail closed with both kept. Absent means NotFound only.
/// What [`settle_one_move`] did, so a failed settle can undo exactly that.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Settled {
    /// Nothing to do: the source was already gone.
    Nothing,
    /// The source was renamed to the destination.
    Renamed,
    /// The destination was already a second hard link of the source; the source name was removed.
    LinkCleaned,
}

/// `Ok(true)` only for a regular file that is not a symbolic link or other reparse point.
fn regular_entry(path: &Path) -> Result<bool, LifecycleError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_file()),
        Err(error) => Err(fs_error(path, error)),
    }
}

/// Settle one move and report what changed.
fn settle_one_move(recorded: &RecordedMove) -> Result<Settled, LifecycleError> {
    let refuse = |detail: String| LifecycleError::RecoveryRequired { detail };
    let (source, destination) = (&recorded.source, &recorded.destination);
    // Already moved, or externally vanished: nothing to do.
    if !presence(source)? {
        return Ok(Settled::Nothing);
    }
    if !presence(destination)? {
        return rightkit_fs::rename_exclusive(source, destination)
            .map(|()| Settled::Renamed)
            .map_err(|error| {
                refuse(format!(
                    "quarantine move {} -> {}: {error}; every file was kept",
                    source.display(),
                    destination.display()
                ))
            });
    }
    // Both names must be regular files, never a link: `FileKey::of_path` follows links, so a
    // destination symlink to the live source would look like the same file, and removing the
    // source would destroy the only copy and leave a dangling link.
    if !regular_entry(source)? || !regular_entry(destination)? {
        return Err(refuse(format!(
            "{} or {} is not a regular file; both were kept",
            source.display(),
            destination.display()
        )));
    }
    let same = rightkit_fs::FileKey::of_path(source)
        .and_then(|a| rightkit_fs::FileKey::of_path(destination).map(|b| a == b))
        .map_err(|error| refuse(format!("file identity {}: {error}", source.display())))?;
    if !same {
        return Err(refuse(format!(
            "quarantine destination {} holds unrelated bytes; {} left live",
            destination.display(),
            source.display()
        )));
    }
    if !distinct_entries(source, destination)? {
        return Err(refuse(format!(
            "{} and {} are not two directory entries of one file; {} left live",
            source.display(),
            destination.display(),
            source.display()
        )));
    }
    std::fs::remove_file(source)
        .map(|()| Settled::LinkCleaned)
        .map_err(|error| fs_error(source, error))
}

// ----- the host's own quarantine record -----------------------------------------------------

/// The moves of `all` whose source is `db` or one of its sidecars.
fn moves_of(db: &Path, all: Vec<RecordedMove>) -> Vec<RecordedMove> {
    let set: Vec<PathBuf> = std::iter::once(db.to_path_buf())
        .chain(SIDECARS.iter().map(|sidecar| with_suffix(db, sidecar)))
        .collect();
    all.into_iter()
        .filter(|recorded| set.iter().any(|member| member == &recorded.source))
        .collect()
}

fn adopt_legacy_journal(cfg: &EncryptedStoreConfig) -> Result<(), LifecycleError> {
    let Some(journal) = cfg
        .hooks
        .as_ref()
        .and_then(|hooks| hooks.legacy_journal.as_ref())
    else {
        return Ok(());
    };
    let path = cfg.db.effective_path();
    let read = || -> Result<Vec<RecordedMove>, LifecycleError> {
        journal
            .pending(&path)
            .map(|all| moves_of(&path, all))
            .map_err(|why| LifecycleError::RecoveryRequired {
                detail: format!(
                    "the host's quarantine record cannot be read ({why}); every file was kept"
                ),
            })
    };
    // Nothing owed: no guard, no write.
    if read()?.is_empty() {
        return Ok(());
    }
    let transition = StoreTransition::begin(
        TransitionKind::Recover,
        &[cfg.db.path.as_path()],
        &cfg.transition,
    )?;
    let result = (|| -> Result<(), LifecycleError> {
        let owed = read()?;
        if owed.is_empty() {
            return Ok(());
        }
        // Under the exclusive guard, before any move: a journal that owns the files, a move set
        // that aliases, and a credential state that cannot be read all stop the open.
        if transition_owns(cfg)? {
            return Err(owned_by_transition("the host's quarantine settlement"));
        }
        validate_move_set(&path, &owed, true)?;
        let (_session, _primary) = credential_authority(cfg)?;
        let landed = settle_recorded_moves(&owed)?;
        journal
            .settled(&path, &owed, &landed)
            .map_err(|why| LifecycleError::RecoveryRequired {
                detail: format!(
                    "the host could not record that its quarantine moves finished ({why}); the files are settled and the next open reports them again"
                ),
            })
    })();
    transition.settle();
    result
}

/// Execute recorded-but-unfinished moves of a set that passed [`validate_move_set`], each through
/// [`settle_one_move`]. On a failure only the moves THIS call changed are rolled back to their live
/// names (newest first); a move this call never reached is left exactly as it was, and the host's
/// validated record stays for the next open to replay forward. Returns the destinations that exist.
fn settle_recorded_moves(owed: &[RecordedMove]) -> Result<Vec<PathBuf>, LifecycleError> {
    let mut changed: Vec<(&RecordedMove, Settled)> = Vec::new();
    let result = (|| -> Result<(), LifecycleError> {
        for recorded in owed {
            match settle_one_move(recorded)? {
                Settled::Nothing => {}
                done => changed.push((recorded, done)),
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        // Best effort: a rollback move that fails leaves a duplicate the next open resolves. A
        // name that cannot be examined is left alone.
        for (recorded, done) in changed.into_iter().rev() {
            if !matches!(presence(&recorded.source), Ok(false)) {
                continue;
            }
            match done {
                // Undo a rename by renaming back; the destination name did not exist before.
                Settled::Renamed => {
                    let _ = rightkit_fs::rename_exclusive(&recorded.destination, &recorded.source);
                }
                // The destination existed before as a second link: restore the source name as a
                // link again and keep the destination.
                Settled::LinkCleaned => {
                    let _ = std::fs::hard_link(&recorded.destination, &recorded.source);
                }
                Settled::Nothing => {}
            }
        }
        return Err(error);
    }
    let mut landed = Vec::new();
    for recorded in owed {
        if presence(&recorded.destination)? {
            landed.push(recorded.destination.clone());
        }
    }
    Ok(landed)
}

// ----- orphan sidecars ---------------------------------------------------------------------

const ORPHAN_LIST_SUFFIX: &str = ".orphan-quarantine";
const ORPHAN_LIST_MAGIC: &str = "rightkit-db-orphan-quarantine.v1";

/// `<db>.orphan-quarantine`: the durable move list of an orphan sidecar quarantine.
fn orphan_list_path(db: &Path) -> PathBuf {
    with_suffix(db, ORPHAN_LIST_SUFFIX)
}

fn escape_field(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\t', "%09")
}

fn unescape_field(text: &str) -> String {
    text.replace("%09", "\t")
        .replace("%0D", "\r")
        .replace("%0A", "\n")
        .replace("%25", "%")
}

fn path_text(path: &Path) -> Result<&str, LifecycleError> {
    path.to_str()
        .ok_or_else(|| LifecycleError::RecoveryRequired {
            detail: format!(
                "{} is not valid UTF-8, so an orphan sidecar move list cannot name it; every file was kept",
                path.display()
            ),
        })
}

/// Write the move list atomically and privately before the notice and before the first move.
fn write_orphan_list(db: &Path, moves: &[RecordedMove]) -> Result<(), LifecycleError> {
    let mut body = format!(
        "{ORPHAN_LIST_MAGIC}\ndatabase={}\n",
        escape_field(path_text(db)?)
    );
    for recorded in moves {
        body.push_str(&format!(
            "move={}\t{}\n",
            escape_field(path_text(&recorded.source)?),
            escape_field(path_text(&recorded.destination)?)
        ));
    }
    body.push_str("end\n");
    rightkit_fs::atomic_write_private(&orphan_list_path(db), body.as_bytes())?;
    Ok(())
}

/// `Ok(None)` when there is no list; a list that cannot be read or does not parse is an error
/// (fail closed, every file kept).
fn read_orphan_list(db: &Path) -> Result<Option<Vec<RecordedMove>>, LifecycleError> {
    let path = orphan_list_path(db);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(fs_error(&path, error)),
    };
    let bad = |why: &str| LifecycleError::RecoveryRequired {
        detail: format!(
            "the orphan sidecar move list {} is unusable ({why}); every file was kept",
            path.display()
        ),
    };
    let mut lines = text.lines();
    if lines.next() != Some(ORPHAN_LIST_MAGIC) {
        return Err(bad("unknown format"));
    }
    let database = lines
        .next()
        .and_then(|line| line.strip_prefix("database="))
        .map(unescape_field)
        .ok_or_else(|| bad("no database line"))?;
    if Path::new(&database) != db {
        return Err(bad("it names another database"));
    }
    let mut moves = Vec::new();
    let mut ended = false;
    for line in lines {
        if ended {
            return Err(bad("text after the end line"));
        }
        if line == "end" {
            ended = true;
            continue;
        }
        let (from, to) = line
            .strip_prefix("move=")
            .and_then(|rest| rest.split_once('\t'))
            .ok_or_else(|| bad("a line is not a move"))?;
        moves.push(RecordedMove {
            source: PathBuf::from(unescape_field(from)),
            destination: PathBuf::from(unescape_field(to)),
        });
    }
    if !ended || moves.is_empty() {
        return Err(bad("it is truncated"));
    }
    Ok(Some(moves))
}

fn remove_orphan_list(db: &Path) -> Result<(), LifecycleError> {
    let path = orphan_list_path(db);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(fs_error(&path, error)),
    }
}

/// `Ok(true)` when the main file is there with bytes in it.
fn main_has_content(path: &Path) -> Result<bool, LifecycleError> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.len() > 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(fs_error(path, error)),
    }
}

/// The sidecars that are there, when at least one of them has content.
fn orphan_sidecars(db: &Path) -> Result<Option<Vec<PathBuf>>, LifecycleError> {
    let mut present = Vec::new();
    let mut has_content = false;
    for sidecar in SIDECARS {
        let path = with_suffix(db, sidecar);
        match std::fs::metadata(&path) {
            Ok(meta) => {
                has_content |= meta.is_file() && meta.len() > 0;
                present.push(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(fs_error(&path, error)),
        }
    }
    Ok(has_content.then_some(present))
}

/// Protect orphan sidecars (after every journal is settled, before any writer opens): finish a
/// durable move list an earlier run left, whatever the switch says; with the switch on, start a new
/// quarantine when the main file is missing or empty and a sidecar has content. Takes the exclusive
/// guard only when there is something to do.
fn protect_orphans(cfg: &EncryptedStoreConfig) -> Result<(), LifecycleError> {
    let path = cfg.db.effective_path();
    if !presence(&orphan_list_path(&path))? {
        let wanted = cfg
            .hooks
            .as_ref()
            .is_some_and(|hooks| hooks.orphan_sidecars);
        if !wanted || main_has_content(&path)? || orphan_sidecars(&path)?.is_none() {
            return Ok(());
        }
    }
    let transition = StoreTransition::begin(
        TransitionKind::Recover,
        &[cfg.db.path.as_path()],
        &cfg.transition,
    )?;
    let result = (|| -> Result<(), LifecycleError> {
        let (_session, _primary) = credential_authority(cfg)?;
        protect_orphans_locked(cfg, &path)
    })();
    transition.settle();
    result
}

/// [`protect_orphans`] under the exclusive guard, with the key store's rotation lock held by the
/// caller. Everything is re-examined here with fresh eyes.
fn protect_orphans_locked(cfg: &EncryptedStoreConfig, path: &Path) -> Result<(), LifecycleError> {
    let list_present = presence(&orphan_list_path(path))?;
    // Whatever is here belongs to an unresolved transition: its recovery decides, not this.
    if transition_owns(cfg)? {
        return if list_present {
            Err(owned_by_transition("the orphan sidecar quarantine"))
        } else {
            Ok(())
        };
    }
    if list_present {
        let Some(moves) = read_orphan_list(path)? else {
            return Ok(());
        };
        validate_move_set(path, &moves, false)?;
        announce(
            cfg,
            path,
            QuarantineKind::OrphanSidecars,
            None,
            moves.clone(),
            true,
        )?;
        finish_orphan_moves(&moves)?;
        return remove_orphan_list(path);
    }
    if !cfg
        .hooks
        .as_ref()
        .is_some_and(|hooks| hooks.orphan_sidecars)
    {
        return Ok(());
    }
    if main_has_content(path)? {
        return Ok(());
    }
    let Some(present) = orphan_sidecars(path)? else {
        return Ok(());
    };
    let stamp = cfg
        .rebuild
        .as_ref()
        .map_or_else(QuarantineStamp::default, |policy| policy.quarantine_stamp);
    let suffix = stamp.suffix(SystemTime::now());
    if suffix.is_empty() || suffix.contains(['/', '\\', '\0']) {
        return Err(LifecycleError::Db(DbError::InvalidQuarantineSuffix(suffix)));
    }
    // One suffix for the whole set, `-1`, `-2`, ... until no destination name is taken.
    let mut chosen = None;
    for attempt in 0..100u32 {
        let candidate = if attempt == 0 {
            suffix.clone()
        } else {
            format!("{suffix}-{attempt}")
        };
        let mut taken = false;
        for from in &present {
            if presence(&with_suffix(from, &candidate))? {
                taken = true;
                break;
            }
        }
        if !taken {
            chosen = Some(candidate);
            break;
        }
    }
    let tag = chosen.ok_or_else(|| {
        fs_error(
            path,
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "every orphan sidecar quarantine name is taken",
            ),
        )
    })?;
    let moves: Vec<RecordedMove> = present
        .iter()
        .map(|from| RecordedMove {
            source: from.clone(),
            destination: with_suffix(from, &tag),
        })
        .collect();
    validate_move_set(path, &moves, false)?;
    // The durable move list first: a stop after the first move still leaves the rest of the set
    // owed, which a re-derivation from the files could no longer see.
    write_orphan_list(path, &moves)?;
    if let Err(refused) = announce(
        cfg,
        path,
        QuarantineKind::OrphanSidecars,
        None,
        moves.clone(),
        false,
    ) {
        // Nothing moved: drop the list. If that fails the list stays and the next open finishes
        // it (announcing it again).
        let _ = remove_orphan_list(path);
        return Err(refused);
    }
    finish_orphan_moves(&moves)?;
    remove_orphan_list(path)
}

/// Move every listed sidecar. A stop leaves the list in place, so the next open continues.
fn finish_orphan_moves(moves: &[RecordedMove]) -> Result<(), LifecycleError> {
    for recorded in moves {
        settle_one_move(recorded)?;
    }
    Ok(())
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
    // Credential authority first: no resumed move, notice or rebuild step runs while the key store
    // cannot be read. The rotation lock is held to the end (lock order: guard, then this lock).
    let (mut session, primary) = credential_authority(cfg)?;
    let ks = &mut *session;
    // Ownership before anything else touches the files: an unresolved rotation, restore or
    // relocation decides them, so no notice, resumed move or rebuild step runs beside it.
    if transition_owns(cfg)? {
        return Ok(None);
    }
    // An earlier run's quarantine is announced again before it is finished or undone, so the host
    // record exists whichever way the journal settles. A refusal leaves the journal and every file.
    if cfg
        .hooks
        .as_ref()
        .is_some_and(|hooks| hooks.on_quarantine.is_some())
    {
        if let Some(dest) = recorded_quarantine(&path) {
            let moves = moves_between(&path, &dest)?;
            announce(
                cfg,
                &path,
                QuarantineKind::Database,
                read_journal(&cfg.db.path)?,
                moves,
                true,
            )?;
        }
    }
    // Settle a quarantine an earlier run started, whatever the policy: a missing main file beside
    // a live `-wal` must never be opened.
    let mut quarantined = match resume_quarantine(&path)? {
        QuarantineResume::Completed { quarantined } => Some(quarantined),
        _ => None,
    };
    // Orphan sidecars are protected before the first candidate probe: a probe opens a writer, and
    // a writer over a zero-length main file beside a live `-wal` would take that log as its own.
    protect_orphans_locked(cfg, &path)?;
    let Some(policy) = cfg.rebuild.as_ref() else {
        return Ok(None);
    };
    let recorded = read_journal(&cfg.db.path)?;
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
        let moved = quarantine_database_journaled_notified(
            &path,
            policy.quarantine_stamp,
            SidecarPolicy::RenameSidecars,
            &|step| {
                faults.die_rebuild_after(match step {
                    QuarantineStep::Recorded => RebuildStep::QuarantineRecorded,
                    QuarantineStep::MainMoved => RebuildStep::MainMoved,
                })
            },
            // Inside the quarantine transaction: the journal is written, nothing has moved.
            &|dest| {
                let moves = moves_between(&path, dest).map_err(|error| error.to_string())?;
                match cfg
                    .hooks
                    .as_ref()
                    .and_then(|hooks| hooks.on_quarantine.as_ref())
                {
                    Some(notify) => notify(&QuarantineNotice {
                        database: path.clone(),
                        kind: QuarantineKind::Database,
                        reason: Some(reason),
                        moves,
                        resumed: false,
                    }),
                    None => Ok(()),
                }
            },
        );
        match moved {
            Ok(dest) => quarantined = Some(dest),
            // Nothing moved, or the split is journaled and the next open settles it.
            Err(failure) => return Err(failure.error.into()),
        }
    }
    die(RebuildStep::Quarantined)?;

    // Orphan sidecars are protected before the writer that would create the database over them
    // (under this rebuild's guard and key lock).
    protect_orphans_locked(cfg, &path)?;
    drop(cfg.keyed(&key).open_writer()?);
    die(RebuildStep::Created)?;
    remove_journal(&cfg.db.path);
    Ok(Some(RebuildReport {
        reason,
        quarantined,
        minted_key,
    }))
}
