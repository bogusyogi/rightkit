//! `RootCutover`: move an app's data root from an old location to a new one with the writers
//! stopped, the copy verified, the selection persisted and every reader reopened, and with the old
//! root left complete and recoverable (PK07-3).
//!
//! The cutover composes three things and owns none of their internals:
//!
//! * the **file move**: either the restart-safe tree copy of [`AppRoot::migrate_from_with`]
//!   ([`FsMove::Tree`]) or a bounded list of companion files published through one
//!   `rightkit-fs` `FileTransaction` rooted at the new root ([`FsMove::Companions`]);
//! * **lifecycle participants** the app registers ([`CutoverParticipant`]): opaque objects with
//!   five hooks (prepare, quiesce, relocate, reopen, settle). The encrypted-database relocation of
//!   `rightkit-db` is one of them: the app wraps `EncryptedStore::relocate` in a participant. This
//!   crate depends on neither `rightkit-db` nor any credential crate, and `rightkit-db` takes no
//!   daemon dependency;
//! * a **persisted selection**, the record [`CUTOVER_RECORD`] at the new root, which says which
//!   root readers and services must open.
//!
//! # Protocol
//!
//! | # | Step | Selection a reader sees | A crash here leaves |
//! |---|---|---|---|
//! | 1 | take the cutover lock at the new root; resolve pending `FileTransaction`s | old | nothing |
//! | 2 | record `copying` (generation, both roots, both directory identities) | old | the old root, untouched |
//! | 3 | every participant `prepare`, then `quiesce` (writers stopped) | old | quiesced writers die with the process |
//! | 4 | every participant `relocate`; then the file move | old | a partial copy at the new root, never selected |
//! | 5 | re-check both directory identities (path substitution) | old | as 4 |
//! | 6 | record `switched`: **the single decision** | **new** | roll forward: [`RootCutover::recover`] |
//! | 7 | every participant `reopen` at the new root and verify | new | `switched`: reopen again (hooks are idempotent) |
//! | 8 | record `completed`; every participant `settle(Committed)` | new | `completed` |
//!
//! Before step 6 any failure aborts: every participant that was prepared gets
//! `settle(Aborted)` (in reverse order) and resumes at the old root, the record becomes `aborted`
//! with the reason and the old root is exactly as it was. A rerun resumes: the tree copy and the
//! companion step are restart-safe and never overwrite a differing file. After step 6 there is no
//! rollback in this version: the new root is the authority and a failed reopen is reported as
//! [`CutoverError::ReopenPending`]; [`RootCutover::recover`] (run it at startup, before opening
//! any root) finishes it. A participant whose files must not travel with the tree names them in
//! [`CutoverParticipant::owned_paths`]; the tree move leaves those paths out.
//!
//! The old root is never modified or deleted by the cutover. Cleanup is a separate step,
//! [`RootCutover::retire_old_root`], that refuses unless the cutover is `completed` and the old
//! directory is still the directory (by file identity, not by path) that was cut over, and then
//! only renames it aside or sends it to the trash.
//!
//! Readers decide the root with [`RootCutover::status`] / [`RootCutover::selected_root`]: no
//! record or `copying`/`aborted` selects the old root, `switched`/`completed` the new one.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};

use rightkit_fs::atomic::try_sync_dir;
use rightkit_fs::{
    recover_transactions, FileKey, FileTransaction, FsError, TransactionEntry, TransactionOptions,
    TransactionReport,
};
use serde_json::{json, Value};

use crate::app_root::{
    same_content, write_json_atomic, AppRoot, AppRootError, MigrateOptions, MigrationReport,
};
use crate::discovery::try_lock_exclusive;

/// The persisted selection: written at the new root, atomically, at every phase change.
pub const CUTOVER_RECORD: &str = ".rightkit-root-cutover.json";
/// The cross-process cutover lock at the new root (an OS lock, released on process death).
const CUTOVER_LOCK: &str = ".rightkit-cutover.lock";
/// The `rightkit-fs` transaction area; never copied from an old root.
const TXN_AREA: &str = ".rktxn";
const RECORD_VERSION: u64 = 1;

// ---------------------------------------------------------------------------------------------
// Participants
// ---------------------------------------------------------------------------------------------

/// A failure reported by a participant hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantError {
    message: String,
}

impl ParticipantError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Wrap any error by its message.
    pub fn from_error(error: &dyn std::error::Error) -> Self {
        Self::new(error.to_string())
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ParticipantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParticipantError {}

/// The hook that was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CutoverStep {
    Prepare,
    Quiesce,
    Relocate,
    Reopen,
    Settle,
}

impl CutoverStep {
    pub fn as_str(self) -> &'static str {
        match self {
            CutoverStep::Prepare => "prepare",
            CutoverStep::Quiesce => "quiesce",
            CutoverStep::Relocate => "relocate",
            CutoverStep::Reopen => "reopen",
            CutoverStep::Settle => "settle",
        }
    }
}

/// How the cutover ended, as told to [`CutoverParticipant::settle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Settlement {
    /// The new root is the authority and every participant reopened there.
    Committed,
    /// The cutover failed before the switch: resume at the old root.
    Aborted,
}

/// What a participant hook is told about the cutover it is part of.
#[derive(Debug, Clone)]
pub struct CutoverContext {
    id: String,
    generation: u64,
    old_root: PathBuf,
    new_root: PathBuf,
    resumed: bool,
}

impl CutoverContext {
    /// The cutover id (random, fresh for each attempt, persisted in the record).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The generation number: 1 for the first attempt at this new root, one more for each rerun
    /// after an abort or an interruption. A service can fence stale replies with it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The canonical old root. Participants must treat it as read-only.
    pub fn old_root(&self) -> &Path {
        &self.old_root
    }

    /// The canonical new root.
    pub fn new_root(&self) -> &Path {
        &self.new_root
    }

    /// True when this attempt follows an earlier one that did not complete, or finishes a
    /// switched cutover after a restart. Hooks must be idempotent: work an earlier attempt
    /// already finished is verified, not redone or refused.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    /// The root a participant must be using after `settlement`.
    pub fn authoritative(&self, settlement: Settlement) -> &Path {
        match settlement {
            Settlement::Committed => &self.new_root,
            Settlement::Aborted => &self.old_root,
        }
    }
}

/// An opaque lifecycle participant: something the app owns that holds state under the root and
/// must be stopped, moved, reopened or released as part of the cutover (an encrypted database and
/// its key, a settings store, a resident service, a watcher).
///
/// Every hook has a default that does nothing, so a participant implements the ones it needs.
/// Hooks run in registration order (`settle(Aborted)` in reverse), on the caller's thread, one at
/// a time. A panic in a hook is caught and reported as that hook failing.
///
/// Contract: `prepare` validates and acquires what the cutover needs and changes nothing the old
/// root depends on; `quiesce` stops this participant's writers (after it returns `Ok` nothing of
/// this participant writes to the old root, and a read-only `relocate` may rely on it);
/// `relocate` copies this participant's state to the new root, never modifies or removes the old
/// state, and must be idempotent (state already verified at the new root from an earlier attempt
/// is `Ok`); `reopen` opens the participant at the new root and verifies it; `settle` releases
/// what `prepare` acquired and, for [`Settlement::Aborted`], resumes at the old root. `settle` is
/// called for every participant whose `prepare` returned `Ok`, whichever later hook failed, so it
/// must be safe in any state (a participant that quiesced and one that did not alike). A
/// participant whose own `prepare` failed is not settled: it cleans up after itself.
pub trait CutoverParticipant: Send {
    /// A short stable name: ASCII letters, digits, `.`, `_` or `-`, unique within the cutover.
    fn name(&self) -> &str;

    /// Paths, relative to the root, this participant moves by its own means. A tree move leaves
    /// them out ([`MigrateOptions::exclude`]) and a companion list may not touch them.
    fn owned_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn prepare(&mut self, _cx: &CutoverContext) -> Result<(), ParticipantError> {
        Ok(())
    }

    fn quiesce(&mut self, _cx: &CutoverContext) -> Result<(), ParticipantError> {
        Ok(())
    }

    fn relocate(&mut self, _cx: &CutoverContext) -> Result<(), ParticipantError> {
        Ok(())
    }

    fn reopen(&mut self, _cx: &CutoverContext) -> Result<(), ParticipantError> {
        Ok(())
    }

    fn settle(
        &mut self,
        _cx: &CutoverContext,
        _settlement: Settlement,
    ) -> Result<(), ParticipantError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Plan, phases, record, status
// ---------------------------------------------------------------------------------------------

/// How the files under the root travel.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum FsMove {
    /// No file move: the participants carry everything.
    #[default]
    None,
    /// Copy the whole old tree with [`AppRoot::migrate_from_with`] under these options, minus the
    /// participants' [`owned_paths`](CutoverParticipant::owned_paths) and the cutover's own files.
    Tree(MigrateOptions),
    /// Publish these files (paths relative to both roots, regular files, at most 256) through one
    /// `rightkit-fs` `FileTransaction` rooted at the new root. A destination that already holds
    /// identical bytes is accepted (a rerun); one that holds anything else is a
    /// [`CutoverError::Collision`] and nothing is overwritten.
    Companions(Vec<PathBuf>),
}

impl FsMove {
    fn label(&self) -> &'static str {
        match self {
            FsMove::None => "none",
            FsMove::Tree(_) => "tree",
            FsMove::Companions(_) => "companions",
        }
    }
}

/// The persisted phase of a cutover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CutoverPhase {
    /// Started, not switched. The old root is the authority.
    Copying,
    /// The switch is durable. The new root is the authority; participants may still have to
    /// reopen ([`RootCutover::recover`]).
    Switched,
    /// Switched and every participant reopened.
    Completed,
    /// Failed before the switch. The old root is the authority; a rerun is allowed.
    Aborted,
}

impl CutoverPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            CutoverPhase::Copying => "copying",
            CutoverPhase::Switched => "switched",
            CutoverPhase::Completed => "completed",
            CutoverPhase::Aborted => "aborted",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "copying" => CutoverPhase::Copying,
            "switched" => CutoverPhase::Switched,
            "completed" => CutoverPhase::Completed,
            "aborted" => CutoverPhase::Aborted,
            _ => return None,
        })
    }
}

/// Which root readers must open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSelection {
    Old,
    New,
}

/// What the persisted record says (see [`RootCutover::status`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RootStatus {
    pub id: String,
    pub generation: u64,
    pub phase: CutoverPhase,
    pub selected: RootSelection,
    pub old_root: PathBuf,
    pub new_root: PathBuf,
    pub participants: Vec<String>,
    /// Why an `aborted` cutover failed.
    pub error: Option<String>,
    /// Where [`RootCutover::retire_old_root`] put the old root, once it did.
    pub retired: Option<PathBuf>,
}

impl RootStatus {
    /// The root to open.
    pub fn selected_root(&self) -> &Path {
        match self.selected {
            RootSelection::Old => &self.old_root,
            RootSelection::New => &self.new_root,
        }
    }

    /// True when the switch is durable but participants have not all reopened: run
    /// [`RootCutover::recover`] before opening anything.
    pub fn needs_recovery(&self) -> bool {
        self.phase == CutoverPhase::Switched
    }
}

#[derive(Debug, Clone)]
struct Record {
    id: String,
    generation: u64,
    phase: CutoverPhase,
    old_root: PathBuf,
    new_root: PathBuf,
    old_key: FileKey,
    new_key: FileKey,
    participants: Vec<String>,
    fs_move: String,
    error: Option<String>,
    retired: Option<PathBuf>,
}

fn key_json(key: FileKey) -> Value {
    json!({ "volume": key.volume, "index": key.index })
}

fn key_from(value: &Value) -> Option<FileKey> {
    Some(FileKey {
        volume: value.get("volume")?.as_u64()?,
        index: value.get("index")?.as_u64()?,
    })
}

impl Record {
    fn to_json(&self) -> Value {
        json!({
            "version": RECORD_VERSION,
            "id": self.id,
            "generation": self.generation,
            "phase": self.phase.as_str(),
            "old_root": self.old_root.to_str(),
            "new_root": self.new_root.to_str(),
            "old_key": key_json(self.old_key),
            "new_key": key_json(self.new_key),
            "participants": self.participants,
            "fs_move": self.fs_move,
            "error": self.error,
            "retired": self.retired.as_ref().and_then(|p| p.to_str()),
        })
    }

    fn parse(text: &str) -> Result<Self, CutoverError> {
        let bad = |why: &str| CutoverError::RecordUnreadable(why.to_string());
        let v: Value = serde_json::from_str(text).map_err(|e| bad(&e.to_string()))?;
        match v.get("version").and_then(Value::as_u64) {
            Some(RECORD_VERSION) => {}
            Some(n) if n > RECORD_VERSION => {
                return Err(bad("the record was written by a newer version"))
            }
            _ => return Err(bad("missing or unknown record version")),
        }
        let text_of = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let record = Record {
            id: text_of("id").ok_or_else(|| bad("no id"))?,
            generation: v
                .get("generation")
                .and_then(Value::as_u64)
                .ok_or_else(|| bad("no generation"))?,
            phase: text_of("phase")
                .and_then(|p| CutoverPhase::parse(&p))
                .ok_or_else(|| bad("unknown phase"))?,
            old_root: text_of("old_root")
                .map(PathBuf::from)
                .ok_or_else(|| bad("no old_root"))?,
            new_root: text_of("new_root")
                .map(PathBuf::from)
                .ok_or_else(|| bad("no new_root"))?,
            old_key: v
                .get("old_key")
                .and_then(key_from)
                .ok_or_else(|| bad("no old_key"))?,
            new_key: v
                .get("new_key")
                .and_then(key_from)
                .ok_or_else(|| bad("no new_key"))?,
            participants: v
                .get("participants")
                .and_then(Value::as_array)
                .and_then(|a| {
                    a.iter()
                        .map(|n| n.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| bad("invalid participant list"))?,
            fs_move: text_of("fs_move").ok_or_else(|| bad("no fs_move"))?,
            error: text_of("error"),
            retired: text_of("retired").map(PathBuf::from),
        };
        let mut names = BTreeSet::new();
        if record.id.len() != 16
            || !record.id.bytes().all(|b| b.is_ascii_hexdigit())
            || record.generation == 0
            || !matches!(record.fs_move.as_str(), "none" | "tree" | "companions")
            || record
                .participants
                .iter()
                .any(|name| !valid_participant_name(name) || !names.insert(name))
        {
            return Err(bad("invalid cutover identity, generation or participants"));
        }
        for root in [&record.old_root, &record.new_root] {
            if !root.is_absolute() || root.components().any(|c| c == Component::ParentDir) {
                return Err(bad("invalid root path"));
            }
        }
        if record.old_root.starts_with(&record.new_root)
            || record.new_root.starts_with(&record.old_root)
        {
            return Err(bad("overlapping roots"));
        }
        Ok(record)
    }

    fn status(&self) -> RootStatus {
        RootStatus {
            id: self.id.clone(),
            generation: self.generation,
            phase: self.phase,
            selected: match self.phase {
                CutoverPhase::Switched | CutoverPhase::Completed => RootSelection::New,
                CutoverPhase::Copying | CutoverPhase::Aborted => RootSelection::Old,
            },
            old_root: self.old_root.clone(),
            new_root: self.new_root.clone(),
            participants: self.participants.clone(),
            error: self.error.clone(),
            retired: self.retired.clone(),
        }
    }
}

fn read_record(new_root: &Path) -> Result<Option<Record>, CutoverError> {
    let path = new_root.join(CUTOVER_RECORD);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    let mut file = rightkit_fs::open_read_nofollow(&path, &rightkit_fs::ReadOptions::new())?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let record = Record::parse(&text)?;
    let actual = fs::canonicalize(new_root)?;
    if record.new_root != actual {
        return Err(CutoverError::RecordMismatch(
            "record names another destination".into(),
        ));
    }
    check_directory(&actual, record.new_key)?;
    Ok(Some(record))
}

/// Atomic replace of the record, then a directory flush where the platform has one.
fn write_record(new_root: &Path, record: &Record) -> io::Result<()> {
    write_json_atomic(&new_root.join(CUTOVER_RECORD), &record.to_json())?;
    try_sync_dir(new_root)
}

// ---------------------------------------------------------------------------------------------
// Errors and reports
// ---------------------------------------------------------------------------------------------

/// Why a cutover step failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum CutoverError {
    /// The old or new root cannot be used as given (missing, not a real directory, a symlink,
    /// not UTF-8, relative, or the two nest). Nothing was changed.
    InvalidRoot {
        path: PathBuf,
        reason: &'static str,
    },
    /// A participant name is empty, not plain, or used twice, or an owned path is not a plain
    /// relative path.
    BadParticipant(String),
    /// A companion path is not a plain relative path to a regular file in the old root.
    /// Path syntax is checked before participant hooks; file existence and type are checked
    /// at move time, after `relocate`. A failure then settles prepared participants as aborted.
    BadCompanion(PathBuf),
    /// A companion lies on a path a participant owns.
    OwnedPathConflict {
        participant: String,
        path: PathBuf,
    },
    /// Another process or thread is running a cutover for this root.
    Busy(PathBuf),
    /// A destination holds something other than what would be written. Nothing was overwritten.
    Collision(PathBuf),
    /// A root is no longer the directory (by file identity) that was recorded: it was replaced
    /// or moved while the cutover ran.
    PathSubstituted(PathBuf),
    /// The persisted record names other roots than this cutover.
    RecordMismatch(String),
    /// The persisted record cannot be read or is of a newer version. Fails closed: the caller
    /// must not guess which root to open.
    RecordUnreadable(String),
    /// A `FileTransaction` of an earlier run is retained and could not be resolved.
    PendingTransaction(String),
    /// A participant hook failed before the switch; every prepared participant was settled as
    /// aborted and the old root is the authority.
    Participant {
        name: String,
        step: CutoverStep,
        error: ParticipantError,
    },
    /// The cutover failed before the switch and at least one participant also failed to resume
    /// (`settle(Aborted)`); those participants must be restarted by the app.
    AbortIncomplete {
        cause: Box<CutoverError>,
        failures: Vec<(String, ParticipantError)>,
    },
    /// The switch is durable (the new root is the authority) but a participant could not reopen.
    /// Run [`RootCutover::recover`]; hooks are idempotent.
    ReopenPending {
        name: String,
        error: ParticipantError,
        generation: u64,
    },
    /// The tree migration failed or ended unusable.
    Migrate(AppRootError),
    /// A file-system operation failed.
    Fs(FsError),
    /// The companion transaction could not be decided in this process.
    FsRecoveryRequired(String),
    /// The record write at the switch failed and what is on disk could not be read back. The
    /// decision is unknown; participants stay quiesced. Run [`RootCutover::recover`].
    Undecided(io::Error),
    /// The cutover has not completed (`retire_old_root` needs `completed`).
    NotCompleted(CutoverPhase),
    Io(io::Error),
}

impl fmt::Display for CutoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CutoverError::InvalidRoot { path, reason } => {
                write!(f, "root cutover: {}: {reason}", path.display())
            }
            CutoverError::BadParticipant(n) => {
                write!(f, "root cutover: participant {n:?} is not usable")
            }
            CutoverError::BadCompanion(p) => write!(
                f,
                "root cutover: companion {} is not a plain relative path to a regular file",
                p.display()
            ),
            CutoverError::OwnedPathConflict { participant, path } => write!(
                f,
                "root cutover: {} lies on a path participant {participant} owns",
                path.display()
            ),
            CutoverError::Busy(p) => {
                write!(f, "root cutover: another cutover holds {}", p.display())
            }
            CutoverError::Collision(p) => write!(
                f,
                "root cutover: {} holds different content; nothing was overwritten",
                p.display()
            ),
            CutoverError::PathSubstituted(p) => write!(
                f,
                "root cutover: {} is no longer the directory that was recorded",
                p.display()
            ),
            CutoverError::RecordMismatch(why) => write!(f, "root cutover: record mismatch: {why}"),
            CutoverError::RecordUnreadable(why) => {
                write!(f, "root cutover: cutover record unreadable: {why}")
            }
            CutoverError::PendingTransaction(why) => {
                write!(f, "root cutover: an earlier file transaction is pending: {why}")
            }
            CutoverError::Participant { name, step, error } => write!(
                f,
                "root cutover: participant {name} failed to {}: {error}",
                step.as_str()
            ),
            CutoverError::AbortIncomplete { cause, failures } => {
                write!(f, "{cause}; {} participant(s) did not resume", failures.len())
            }
            CutoverError::ReopenPending {
                name,
                error,
                generation,
            } => write!(
                f,
                "root cutover: generation {generation} is switched but participant {name} could not reopen: {error}; run recover"
            ),
            CutoverError::Migrate(e) => write!(f, "root cutover: tree migration: {e}"),
            CutoverError::Fs(e) => write!(f, "root cutover: {e}"),
            CutoverError::FsRecoveryRequired(why) => {
                write!(f, "root cutover: file transaction needs recovery: {why}")
            }
            CutoverError::Undecided(e) => write!(
                f,
                "root cutover: the switch record could not be confirmed ({e}); run recover"
            ),
            CutoverError::NotCompleted(p) => {
                write!(f, "root cutover: the cutover is {}, not completed", p.as_str())
            }
            CutoverError::Io(e) => write!(f, "root cutover: {e}"),
        }
    }
}

impl std::error::Error for CutoverError {}

impl From<io::Error> for CutoverError {
    fn from(e: io::Error) -> Self {
        CutoverError::Io(e)
    }
}

impl From<FsError> for CutoverError {
    fn from(e: FsError) -> Self {
        CutoverError::Fs(e)
    }
}

/// What a finished cutover did.
#[derive(Debug)]
#[non_exhaustive]
pub struct CutoverReport {
    pub id: String,
    pub generation: u64,
    /// Always [`RootSelection::New`] for a returned report.
    pub selected: RootSelection,
    /// The tree migration report ([`FsMove::Tree`]).
    pub migration: Option<MigrationReport>,
    /// Absolute destinations of the companions, published now or already in place
    /// ([`FsMove::Companions`]).
    pub companions: Vec<PathBuf>,
    /// This call finished a switched cutover begun earlier (a restart, or a reopen that failed).
    pub resumed: bool,
    /// The cutover was already completed; nothing ran.
    pub already_completed: bool,
    /// `settle(Committed)` failures. The cutover is complete regardless: settle releases
    /// resources, it does not decide anything.
    pub settle_warnings: Vec<(String, ParticipantError)>,
}

impl CutoverReport {
    fn new(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            generation: record.generation,
            selected: RootSelection::New,
            migration: None,
            companions: Vec::new(),
            resumed: false,
            already_completed: false,
            settle_warnings: Vec::new(),
        }
    }
}

/// What [`RootCutover::recover`] found.
#[derive(Debug)]
#[non_exhaustive]
pub enum RecoverOutcome {
    /// No cutover, or one that never reached the switch (`copying`, `aborted`): the old root is
    /// still the authority and nothing was run.
    Nothing,
    /// A switched cutover was finished.
    Resumed(Box<CutoverReport>),
    /// The cutover was already completed.
    Completed,
}

/// What [`RootCutover::retire_old_root`] does with the old root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetireMode {
    /// Rename the old root to `<name>.rk-retired-<id>` beside it. Recoverable by renaming back;
    /// frees no space.
    RenameAside,
    /// Send the old root to the platform trash (macOS only today: `rightkit_fs::move_to_trash`
    /// reports `Unavailable` on Windows and the call changes nothing there).
    Trash,
}

/// What [`RootCutover::retire_old_root`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Retired {
    /// Where the old root is now.
    pub path: PathBuf,
    /// An earlier call had already retired it (or finished retiring it after a crash).
    pub already: bool,
}

// ---------------------------------------------------------------------------------------------
// RootCutover
// ---------------------------------------------------------------------------------------------

struct Plan {
    old: PathBuf,
    new: PathBuf,
    old_key: FileKey,
    new_key: FileKey,
}

/// The cutover of one app root. Build it with [`RootCutover::new`], register participants, run it.
pub struct RootCutover {
    app: AppRoot,
    old_root: PathBuf,
    fs_move: FsMove,
    participants: Vec<Box<dyn CutoverParticipant>>,
}

fn valid_participant_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn plain_relative(path: &Path) -> bool {
    path.components().count() > 0 && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn new_id() -> Result<String, CutoverError> {
    let mut raw = [0u8; 8];
    getrandom::fill(&mut raw).map_err(|e| CutoverError::Io(io::Error::other(e.to_string())))?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

fn take_lock(new_root: &Path) -> Result<fs::File, CutoverError> {
    let path = new_root.join(CUTOVER_LOCK);
    match try_lock_exclusive(&path) {
        Ok(Some(file)) => Ok(file),
        Ok(None) => Err(CutoverError::Busy(path)),
        Err(e) => Err(CutoverError::Io(e)),
    }
}

/// Resolve any `FileTransaction` an earlier run left at the new root. Retained material is a
/// refusal, never a guess.
fn resolve_transactions(new_root: &Path) -> Result<(), CutoverError> {
    let report = recover_transactions(new_root).map_err(CutoverError::Fs)?;
    if report.is_clean() {
        Ok(())
    } else {
        Err(CutoverError::PendingTransaction(format!(
            "{} transaction(s) retained under {}",
            report.transactions.len(),
            new_root.join(TXN_AREA).display()
        )))
    }
}

/// The directory at `path` must still be a real directory with identity `key`.
fn check_directory(path: &Path, key: FileKey) -> Result<(), CutoverError> {
    let substituted = || CutoverError::PathSubstituted(path.to_path_buf());
    let md = fs::symlink_metadata(path).map_err(|_| substituted())?;
    if !real_directory(&md) {
        return Err(substituted());
    }
    match FileKey::of_path(path) {
        Ok(now) if now == key => Ok(()),
        _ => Err(substituted()),
    }
}

fn real_directory(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    metadata.file_type().is_dir()
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_string())
}

impl RootCutover {
    /// A cutover of `app`'s root (the new root) from `old_root`. `old_root` must be an absolute
    /// path to a real directory that does not contain, and is not inside, the new root.
    pub fn new(app: AppRoot, old_root: impl Into<PathBuf>) -> Self {
        Self {
            app,
            old_root: old_root.into(),
            fs_move: FsMove::None,
            participants: Vec::new(),
        }
    }

    /// How the files travel (default [`FsMove::None`]).
    pub fn fs_move(mut self, fs_move: FsMove) -> Self {
        self.fs_move = fs_move;
        self
    }

    /// Register a participant. Order matters: hooks run in registration order.
    pub fn participant(mut self, participant: impl CutoverParticipant + 'static) -> Self {
        self.participants.push(Box::new(participant));
        self
    }

    // ---- reading the selection -------------------------------------------------------------

    /// The persisted selection of `app`'s root, or `None` when no cutover ever started there (the
    /// old root is the only root). Takes no lock and changes nothing. An unreadable record is an
    /// error, never a guess.
    pub fn status(app: &AppRoot) -> Result<Option<RootStatus>, CutoverError> {
        let Some(record) = read_record(app.root())? else {
            return Ok(None);
        };
        if matches!(record.phase, CutoverPhase::Copying | CutoverPhase::Aborted) {
            check_directory(&record.old_root, record.old_key)?;
        }
        Ok(Some(record.status()))
    }

    /// The root to open: the new root once the switch is durable, otherwise `legacy` (the old
    /// root the app used before any cutover).
    pub fn selected_root(app: &AppRoot, legacy: &Path) -> Result<PathBuf, CutoverError> {
        Ok(match read_record(app.root())? {
            None => legacy.to_path_buf(),
            Some(record) => {
                let (path, key) = match record.phase {
                    CutoverPhase::Switched | CutoverPhase::Completed => {
                        (&record.new_root, record.new_key)
                    }
                    CutoverPhase::Copying | CutoverPhase::Aborted => {
                        (&record.old_root, record.old_key)
                    }
                };
                check_directory(path, key)?;
                path.clone()
            }
        })
    }

    fn validate_resume(&self, record: &Record) -> Result<(), CutoverError> {
        let names: Vec<_> = self
            .participants
            .iter()
            .map(|p| p.name().to_string())
            .collect();
        if names != record.participants
            || (matches!(record.phase, CutoverPhase::Copying | CutoverPhase::Aborted)
                && self.fs_move.label() != record.fs_move)
        {
            return Err(CutoverError::RecordMismatch(
                "participant order or file-move mode changed".into(),
            ));
        }
        if record.retired.is_none() {
            let old = fs::canonicalize(&self.old_root)?;
            if old != record.old_root {
                return Err(CutoverError::RecordMismatch("old root changed".into()));
            }
            check_directory(&old, record.old_key)?;
        }
        Ok(())
    }

    // ---- running ---------------------------------------------------------------------------

    fn plan_new(&self) -> Result<(PathBuf, FileKey), CutoverError> {
        if !real_directory(&fs::symlink_metadata(self.app.root())?) {
            return Err(CutoverError::InvalidRoot {
                path: self.app.root().to_path_buf(),
                reason: "the new root must be a real directory, not a link or reparse point",
            });
        }
        let new = fs::canonicalize(self.app.root()).map_err(CutoverError::Io)?;
        if new.to_str().is_none() {
            return Err(CutoverError::InvalidRoot {
                path: new,
                reason: "the new root path is not UTF-8",
            });
        }
        let key = FileKey::of_path(&new).map_err(CutoverError::Fs)?;
        Ok((new, key))
    }

    fn plan_old(&self, new: PathBuf, new_key: FileKey) -> Result<Plan, CutoverError> {
        let mut seen = BTreeSet::new();
        for p in &self.participants {
            let name = p.name();
            if !valid_participant_name(name) || !seen.insert(name.to_string()) {
                return Err(CutoverError::BadParticipant(name.to_string()));
            }
            if p.owned_paths().iter().any(|o| !plain_relative(o)) {
                return Err(CutoverError::BadParticipant(name.to_string()));
            }
        }
        if let FsMove::Companions(rels) = &self.fs_move {
            for rel in rels {
                if !plain_relative(rel) {
                    return Err(CutoverError::BadCompanion(rel.clone()));
                }
                for p in &self.participants {
                    if p.owned_paths()
                        .iter()
                        .any(|o| rel.starts_with(o) || o.starts_with(rel))
                    {
                        return Err(CutoverError::OwnedPathConflict {
                            participant: p.name().to_string(),
                            path: rel.clone(),
                        });
                    }
                }
            }
        }
        let raw = &self.old_root;
        let invalid = |reason: &'static str| CutoverError::InvalidRoot {
            path: raw.clone(),
            reason,
        };
        if !raw.is_absolute() || raw.components().any(|c| c == Component::ParentDir) {
            return Err(invalid(
                "the old root must be an absolute path without '..'",
            ));
        }
        let md = fs::symlink_metadata(raw).map_err(|_| invalid("the old root does not exist"))?;
        if !real_directory(&md) {
            return Err(invalid("the old root must be a real directory, not a link"));
        }
        let old = fs::canonicalize(raw).map_err(CutoverError::Io)?;
        if old.to_str().is_none() {
            return Err(invalid("the old root path is not UTF-8"));
        }
        if old.starts_with(&new) || new.starts_with(&old) {
            return Err(invalid("the old and new roots are the same or nested"));
        }
        let old_key = FileKey::of_path(&old).map_err(CutoverError::Fs)?;
        Ok(Plan {
            old,
            new,
            old_key,
            new_key,
        })
    }

    /// Run the cutover. Idempotent: a completed cutover returns
    /// [`already_completed`](CutoverReport::already_completed); a switched one is finished (the
    /// roll-forward of [`recover`](Self::recover)); an aborted or interrupted one is attempted
    /// again with the next generation.
    ///
    /// On `Err` before the switch the old root is the authority and every prepared participant
    /// was settled as aborted. [`CutoverError::ReopenPending`] and [`CutoverError::Undecided`]
    /// mean the opposite or unknown: call [`recover`](Self::recover).
    pub fn run(&mut self) -> Result<CutoverReport, CutoverError> {
        let (new, new_key) = self.plan_new()?;
        let _lock = take_lock(&new)?;
        resolve_transactions(&new)?;
        let mut generation = 1;
        let mut resumed = false;
        if let Some(record) = read_record(&new)? {
            self.validate_resume(&record)?;
            if record.new_root != new {
                return Err(CutoverError::RecordMismatch(format!(
                    "the record names {} as the new root",
                    record.new_root.display()
                )));
            }
            match record.phase {
                CutoverPhase::Completed => {
                    check_directory(&new, record.new_key)?;
                    let mut report = CutoverReport::new(&record);
                    report.already_completed = true;
                    return Ok(report);
                }
                CutoverPhase::Switched => {
                    check_directory(&new, record.new_key)?;
                    let mut report = CutoverReport::new(&record);
                    report.resumed = true;
                    return self.finish(&new, record, true, report);
                }
                CutoverPhase::Copying | CutoverPhase::Aborted => {
                    generation = record.generation.checked_add(1).ok_or_else(|| {
                        CutoverError::RecordUnreadable("generation exhausted".into())
                    })?;
                    resumed = true;
                }
            }
        }
        let plan = self.plan_old(new, new_key)?;
        let mut record = Record {
            id: new_id()?,
            generation,
            phase: CutoverPhase::Copying,
            old_root: plan.old.clone(),
            new_root: plan.new.clone(),
            old_key: plan.old_key,
            new_key: plan.new_key,
            participants: self
                .participants
                .iter()
                .map(|p| p.name().to_string())
                .collect(),
            fs_move: self.fs_move.label().to_string(),
            error: None,
            retired: None,
        };
        write_record(&plan.new, &record)?;
        let cx = CutoverContext {
            id: record.id.clone(),
            generation,
            old_root: plan.old.clone(),
            new_root: plan.new.clone(),
            resumed,
        };
        let mut report = CutoverReport::new(&record);
        if let Err((prepared, cause)) = self.before_switch(&plan, &cx, &mut report) {
            return Err(self.abort(&plan, record, &cx, prepared, cause));
        }

        // The decision. Before the write lands the old root is the authority, after it the new.
        let copying = record.clone();
        record.phase = CutoverPhase::Switched;
        if let Err(e) = write_record(&plan.new, &record) {
            // The rename may have landed even though the write reported failure (a failed
            // directory flush). Read back before deciding anything.
            match read_record(&plan.new) {
                Ok(Some(on_disk)) if on_disk.id == record.id => match on_disk.phase {
                    CutoverPhase::Switched => {}
                    CutoverPhase::Copying => {
                        let prepared = self.participants.len();
                        return Err(self.abort(&plan, copying, &cx, prepared, CutoverError::Io(e)));
                    }
                    _ => return Err(CutoverError::Undecided(e)),
                },
                _ => return Err(CutoverError::Undecided(e)),
            }
        }
        self.finish(&plan.new, record, false, report)
    }

    /// Finish a switched cutover after a restart or a failed reopen, without starting a new one.
    /// Run it at startup, before opening any root: it takes the cutover lock, resolves pending
    /// file transactions, and, when the record says `switched`, reopens every participant at the
    /// new root and completes. A cutover that never reached the switch is left alone
    /// ([`RecoverOutcome::Nothing`]: the old root is still the authority).
    pub fn recover(&mut self) -> Result<RecoverOutcome, CutoverError> {
        let (new, _) = self.plan_new()?;
        let _lock = take_lock(&new)?;
        resolve_transactions(&new)?;
        let Some(record) = read_record(&new)? else {
            return Ok(RecoverOutcome::Nothing);
        };
        self.validate_resume(&record)?;
        if record.new_root != new {
            return Err(CutoverError::RecordMismatch(format!(
                "the record names {} as the new root",
                record.new_root.display()
            )));
        }
        match record.phase {
            CutoverPhase::Switched => {
                check_directory(&new, record.new_key)?;
                let mut report = CutoverReport::new(&record);
                report.resumed = true;
                self.finish(&new, record, true, report)
                    .map(|done| RecoverOutcome::Resumed(Box::new(done)))
            }
            CutoverPhase::Completed => Ok(RecoverOutcome::Completed),
            CutoverPhase::Copying | CutoverPhase::Aborted => Ok(RecoverOutcome::Nothing),
        }
    }

    /// Steps 3 to 5. `Err` carries how many participants were prepared (they get
    /// `settle(Aborted)`).
    fn before_switch(
        &mut self,
        plan: &Plan,
        cx: &CutoverContext,
        report: &mut CutoverReport,
    ) -> Result<(), (usize, CutoverError)> {
        let n = self.participants.len();
        let mut prepared = 0;
        for i in 0..n {
            self.hook(i, CutoverStep::Prepare, cx)
                .map_err(|e| (prepared, e))?;
            prepared += 1;
        }
        for i in 0..n {
            self.hook(i, CutoverStep::Quiesce, cx)
                .map_err(|e| (prepared, e))?;
        }
        for i in 0..n {
            self.hook(i, CutoverStep::Relocate, cx)
                .map_err(|e| (prepared, e))?;
        }
        self.move_files(plan, report).map_err(|e| (prepared, e))?;
        // Path substitution: both roots must still be the directories this run recorded.
        check_directory(&plan.old, plan.old_key).map_err(|e| (prepared, e))?;
        check_directory(&plan.new, plan.new_key).map_err(|e| (prepared, e))?;
        Ok(())
    }

    /// Settle every prepared participant as aborted (reverse order), record `aborted`, and return
    /// the cause (with the participants that did not resume attached).
    fn abort(
        &mut self,
        plan: &Plan,
        mut record: Record,
        cx: &CutoverContext,
        prepared: usize,
        cause: CutoverError,
    ) -> CutoverError {
        let mut failures = Vec::new();
        for i in (0..prepared).rev() {
            if let Err(CutoverError::Participant { name, error, .. }) =
                self.settle_hook(i, cx, Settlement::Aborted)
            {
                failures.push((name, error));
            }
        }
        record.phase = CutoverPhase::Aborted;
        record.error = Some(cause.to_string());
        // Best effort: `copying` already selects the old root, so a failed write loses nothing.
        let _ = write_record(&plan.new, &record);
        if failures.is_empty() {
            cause
        } else {
            CutoverError::AbortIncomplete {
                cause: Box::new(cause),
                failures,
            }
        }
    }

    /// Steps 7 and 8: reopen everything at the new root, record `completed`, settle.
    fn finish(
        &mut self,
        new: &Path,
        mut record: Record,
        resumed: bool,
        mut report: CutoverReport,
    ) -> Result<CutoverReport, CutoverError> {
        let cx = CutoverContext {
            id: record.id.clone(),
            generation: record.generation,
            old_root: record.old_root.clone(),
            new_root: record.new_root.clone(),
            resumed,
        };
        for i in 0..self.participants.len() {
            if let Err(e) = self.hook(i, CutoverStep::Reopen, &cx) {
                return Err(match e {
                    CutoverError::Participant { name, error, .. } => CutoverError::ReopenPending {
                        name,
                        error,
                        generation: record.generation,
                    },
                    other => other,
                });
            }
        }
        record.phase = CutoverPhase::Completed;
        record.error = None;
        write_record(new, &record)?;
        for i in 0..self.participants.len() {
            if let Err(CutoverError::Participant { name, error, .. }) =
                self.settle_hook(i, &cx, Settlement::Committed)
            {
                report.settle_warnings.push((name, error));
            }
        }
        report.resumed = report.resumed || resumed;
        Ok(report)
    }

    fn hook(
        &mut self,
        index: usize,
        step: CutoverStep,
        cx: &CutoverContext,
    ) -> Result<(), CutoverError> {
        let participant = &mut self.participants[index];
        let name = participant.name().to_string();
        let result = catch_unwind(AssertUnwindSafe(|| match step {
            CutoverStep::Prepare => participant.prepare(cx),
            CutoverStep::Quiesce => participant.quiesce(cx),
            CutoverStep::Relocate => participant.relocate(cx),
            CutoverStep::Reopen => participant.reopen(cx),
            // `settle` carries a settlement and goes through `settle_hook`.
            CutoverStep::Settle => Ok(()),
        }));
        Self::hook_result(name, step, result)
    }

    fn settle_hook(
        &mut self,
        index: usize,
        cx: &CutoverContext,
        settlement: Settlement,
    ) -> Result<(), CutoverError> {
        let participant = &mut self.participants[index];
        let name = participant.name().to_string();
        let result = catch_unwind(AssertUnwindSafe(|| participant.settle(cx, settlement)));
        Self::hook_result(name, CutoverStep::Settle, result)
    }

    fn hook_result(
        name: String,
        step: CutoverStep,
        result: std::thread::Result<Result<(), ParticipantError>>,
    ) -> Result<(), CutoverError> {
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(CutoverError::Participant { name, step, error }),
            Err(payload) => {
                let error = ParticipantError::new(format!("panicked: {}", panic_text(payload)));
                Err(CutoverError::Participant { name, step, error })
            }
        }
    }

    // ---- the file move ---------------------------------------------------------------------

    fn move_files(&self, plan: &Plan, report: &mut CutoverReport) -> Result<(), CutoverError> {
        match &self.fs_move {
            FsMove::None => Ok(()),
            FsMove::Tree(options) => {
                // The cutover's own files never travel (an old root can itself be an earlier
                // cutover's new root), and neither do the participants' own paths.
                let mut options = options
                    .clone()
                    .exclude(CUTOVER_RECORD)
                    .exclude(CUTOVER_LOCK)
                    .exclude(TXN_AREA);
                for participant in &self.participants {
                    for owned in participant.owned_paths() {
                        options = options.exclude(owned);
                    }
                }
                let migrated = self
                    .app
                    .migrate_for_cutover(&plan.old, &options)
                    .map_err(CutoverError::Migrate)?;
                if !migrated.state().is_usable() {
                    return Err(CutoverError::Migrate(AppRootError::VerifyFailed(
                        plan.old.clone(),
                    )));
                }
                report.migration = Some(migrated);
                Ok(())
            }
            FsMove::Companions(rels) => self.move_companions(plan, rels, report),
        }
    }

    fn move_companions(
        &self,
        plan: &Plan,
        rels: &[PathBuf],
        report: &mut CutoverReport,
    ) -> Result<(), CutoverError> {
        let mut entries = Vec::new();
        let mut pairs = Vec::new();
        for rel in rels {
            let src = plan.old.join(rel);
            let md =
                fs::symlink_metadata(&src).map_err(|_| CutoverError::BadCompanion(rel.clone()))?;
            if !md.is_file() {
                return Err(CutoverError::BadCompanion(rel.clone()));
            }
            // `AppRoot::path` refuses a destination whose ancestors resolve outside the root.
            let dst = self.app.path(rel).map_err(CutoverError::Migrate)?;
            match fs::symlink_metadata(&dst) {
                Ok(existing) if existing.is_file() => {
                    if !same_content(&src, &dst).map_err(CutoverError::Migrate)? {
                        return Err(CutoverError::Collision(dst));
                    }
                }
                Ok(_) => return Err(CutoverError::Collision(dst)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    entries.push(TransactionEntry::create_new(rel.clone(), src.clone()));
                }
                Err(e) => return Err(CutoverError::Io(e)),
            }
            pairs.push((src, dst));
        }
        if !entries.is_empty() {
            let prepared =
                FileTransaction::prepare(&plan.new, entries, TransactionOptions::default())
                    .map_err(CutoverError::Fs)?;
            match prepared.commit().map_err(CutoverError::Fs)? {
                TransactionReport::Committed(_) => {}
                TransactionReport::RolledBack(undone) => {
                    return Err(CutoverError::Fs(undone.cause));
                }
                TransactionReport::RecoveryRequired(stuck) => {
                    return Err(CutoverError::FsRecoveryRequired(format!(
                        "transaction {} retained {} item(s): {}",
                        stuck.id,
                        stuck.retained.len(),
                        stuck.cause
                    )));
                }
                _ => {
                    return Err(CutoverError::FsRecoveryRequired(
                        "unrecognised transaction report".to_string(),
                    ));
                }
            }
        }
        // Verify every companion byte for byte, including those already in place.
        for (src, dst) in &pairs {
            if !same_content(src, dst).map_err(CutoverError::Migrate)? {
                return Err(CutoverError::Collision(dst.clone()));
            }
        }
        report.companions = pairs.into_iter().map(|(_, dst)| dst).collect();
        Ok(())
    }

    // ---- cleanup ---------------------------------------------------------------------------

    /// Retire the old root of a completed cutover: a separate, identity-checked step that never
    /// runs as part of [`run`](Self::run). Refuses unless the record is `completed` and the
    /// directory at the recorded old path still has the file identity recorded when the cutover
    /// started (a path that was replaced is [`CutoverError::PathSubstituted`], never touched), and
    /// the new root still has its recorded identity. Takes the cutover lock.
    ///
    /// It never deletes: [`RetireMode::RenameAside`] renames the old root beside itself,
    /// [`RetireMode::Trash`] uses the platform trash. A crash between the rename and the record
    /// is repaired by calling it again (the aside directory is found by name and identity).
    pub fn retire_old_root(app: &AppRoot, mode: RetireMode) -> Result<Retired, CutoverError> {
        let new = fs::canonicalize(app.root()).map_err(CutoverError::Io)?;
        let _lock = take_lock(&new)?;
        let Some(mut record) = read_record(&new)? else {
            return Err(CutoverError::RecordUnreadable(
                "no cutover record at this root".to_string(),
            ));
        };
        if record.phase != CutoverPhase::Completed {
            return Err(CutoverError::NotCompleted(record.phase));
        }
        if record.new_root != new {
            return Err(CutoverError::RecordMismatch(format!(
                "the record names {} as the new root",
                record.new_root.display()
            )));
        }
        check_directory(&new, record.new_key)?;
        if let Some(path) = record.retired.clone() {
            check_directory(&path, record.old_key)?;
            return Ok(Retired {
                path,
                already: true,
            });
        }
        let old = record.old_root.clone();
        if old.starts_with(&new) || new.starts_with(&old) {
            return Err(CutoverError::InvalidRoot {
                path: old,
                reason: "the old and new roots are the same or nested",
            });
        }
        let aside = {
            let mut name = old.as_os_str().to_os_string();
            name.push(format!(".rk-retired-{}", record.id));
            PathBuf::from(name)
        };
        match fs::symlink_metadata(&old) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Either a previous call renamed it and crashed before the record, or it is gone.
                check_directory(&aside, record.old_key)?;
                record.retired = Some(aside.clone());
                write_record(&new, &record)?;
                return Ok(Retired {
                    path: aside,
                    already: true,
                });
            }
            Err(e) => return Err(CutoverError::Io(e)),
            Ok(_) => {}
        }
        check_directory(&old, record.old_key)?;
        let landed = match mode {
            RetireMode::RenameAside => {
                if fs::symlink_metadata(&aside).is_ok() {
                    return Err(CutoverError::Collision(aside));
                }
                rightkit_fs::rename_exclusive(&old, &aside).map_err(|error| match error {
                    FsError::AlreadyExists(_) => CutoverError::Collision(aside.clone()),
                    other => CutoverError::Fs(other),
                })?;
                aside
            }
            RetireMode::Trash => {
                let trashed = rightkit_fs::move_to_trash(&old).map_err(CutoverError::Fs)?;
                trashed.trashed_as.unwrap_or(trashed.original)
            }
        };
        record.retired = Some(landed.clone());
        write_record(&new, &record)?;
        Ok(Retired {
            path: landed,
            already: false,
        })
    }
}
