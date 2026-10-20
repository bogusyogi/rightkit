//! The store transition guard and its journal.
//!
//! [`StoreTransition`] is the one cross-process guard for open-time reconciliation, key rotation,
//! recovery, restore and relocation. It is built on the crate's existing store-wide advisory lock
//! (`<db>.store-lock`, [`crate::StoreLock`]): cooperating openers hold the shared lock for as long
//! as they have connections open, and a transition holds the exclusive lock, so it can only start
//! when no cooperating process (and, after the in-process drain, no thread) is using the store.
//!
//! - Acquisition is bounded by [`TransitionOptions::acquire_timeout`]. A busy store returns
//!   [`LifecycleError::Busy`] before any file or key is changed.
//! - The guard is held by the value and released only when it drops. Every operation in this module
//!   is synchronous, so a transition never gives up the guard while one of its own effects is still
//!   in flight. If the process dies instead, the OS releases the lock and the journal below
//!   remains: the next opener sees it and must recover before ordinary use.
//! - The journal `<db>.transition-journal` names the old and new root identities (path plus the
//!   filesystem's file and directory identity). A relocation takes the guard of both roots and
//!   writes the journal beside both, so an opener at either path meets it. It holds no secrets.
//! - Lock order is fixed: transition guard, then the key store's rotation lock, then the database
//!   operation. Several guards (a relocation) are taken in path order under the same bounded wait.

use super::error::LifecycleError;
use crate::{DbError, StoreLock};
use rightkit_fs::FileKey;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const JOURNAL_MAGIC: &str = "rightkit-db-transition.v1";

/// What a transition is doing, for the journal and for a host's stop notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransitionKind {
    Open,
    Rotate,
    Recover,
    Restore,
    Relocate,
}

impl TransitionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Rotate => "rotate",
            Self::Recover => "recover",
            Self::Restore => "restore",
            Self::Relocate => "relocate",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "open" => Self::Open,
            "rotate" => Self::Rotate,
            "recover" => Self::Recover,
            "restore" => Self::Restore,
            "relocate" => Self::Relocate,
            _ => return None,
        })
    }
}

/// Bounded acquisition of the guard.
#[derive(Clone, Debug)]
pub struct TransitionOptions {
    /// How long to wait for the guard (and for the key store's rotation lock) before returning
    /// [`LifecycleError::Busy`]. `Duration::ZERO` tries once.
    pub acquire_timeout: Duration,
    /// Poll interval while waiting.
    pub poll: Duration,
    /// Recorded in the lock owner file so a contender can say who holds it.
    pub label: String,
}

impl Default for TransitionOptions {
    fn default() -> Self {
        Self {
            acquire_timeout: Duration::from_secs(5),
            poll: Duration::from_millis(50),
            label: "rightkit-db".to_string(),
        }
    }
}

impl TransitionOptions {
    pub(crate) fn lock_options(&self, kind: TransitionKind) -> rightkit_fs::LockOptions {
        rightkit_fs::LockOptions {
            timeout: self.acquire_timeout,
            poll: self.poll,
            label: format!("{}:{}", self.label, kind.as_str()),
        }
    }
}

/// A root the journal names: its path and the filesystem identity of the file and its directory
/// when they existed at capture time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootIdentity {
    pub path: PathBuf,
    pub file: Option<FileKey>,
    pub dir: Option<FileKey>,
}

impl RootIdentity {
    pub fn capture(path: &Path) -> Self {
        let file = FileKey::of_path(path).ok();
        let dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .and_then(|parent| FileKey::of_path(parent).ok());
        Self {
            path: path.to_path_buf(),
            file,
            dir,
        }
    }
}

/// Where a journaled transition stands. Recovery decides from the real key and database state, not
/// from this alone; it is for diagnosis and for the `retain_previous` choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JournalState {
    Active,
    /// The outcome could not be classified. Ordinary writes stay blocked; recovery material is
    /// retained.
    Indeterminate,
    /// The change took effect; only cleanup is left.
    CommittedCleanup,
}

impl JournalState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Indeterminate => "indeterminate",
            Self::CommittedCleanup => "committed-cleanup",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "active" => Self::Active,
            "indeterminate" => Self::Indeterminate,
            "committed-cleanup" => Self::CommittedCleanup,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Journal {
    pub kind: TransitionKind,
    pub state: JournalState,
    /// The phase name (see `Phase::as_str` for rotations).
    pub phase: String,
    /// Rotation only: keep the previous key in the recovery slot after the rotation settles.
    pub retain_previous: bool,
    /// The revision of the staged key, when one was staged.
    pub pending: Option<String>,
    pub id: u64,
    pub old_root: RootIdentity,
    pub new_root: Option<RootIdentity>,
}

pub(crate) fn next_id() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// `<db>.transition-journal`.
pub(crate) fn journal_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(".transition-journal");
    PathBuf::from(name)
}

pub(crate) fn journal_exists(db: &Path) -> bool {
    std::fs::symlink_metadata(journal_path(db)).is_ok()
}

fn escape(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
}

fn unescape(text: &str) -> String {
    text.replace("%0A", "\n")
        .replace("%0D", "\r")
        .replace("%25", "%")
}

fn push_line(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push('=');
    out.push_str(&escape(value));
    out.push('\n');
}

fn key_text(key: Option<FileKey>) -> String {
    match key {
        Some(key) => format!("{}:{}", key.volume, key.index),
        None => "-".to_string(),
    }
}

/// `None` is a malformed value; `Some(None)` is an absent identity.
fn parse_key(text: &str) -> Option<Option<FileKey>> {
    if text == "-" {
        return Some(None);
    }
    let (volume, index) = text.split_once(':')?;
    Some(Some(FileKey {
        volume: volume.parse().ok()?,
        index: index.parse().ok()?,
    }))
}

fn push_root(out: &mut String, prefix: &str, root: &RootIdentity) -> Result<(), LifecycleError> {
    let path = root.path.to_str().ok_or_else(|| {
        LifecycleError::InvalidConfig(format!(
            "{} is not valid UTF-8 and cannot be journaled",
            root.path.display()
        ))
    })?;
    push_line(out, &format!("{prefix}_path"), path);
    push_line(out, &format!("{prefix}_file"), &key_text(root.file));
    push_line(out, &format!("{prefix}_dir"), &key_text(root.dir));
    Ok(())
}

fn encode(journal: &Journal) -> Result<String, LifecycleError> {
    let mut out = String::new();
    out.push_str(JOURNAL_MAGIC);
    out.push('\n');
    push_line(&mut out, "kind", journal.kind.as_str());
    push_line(&mut out, "state", journal.state.as_str());
    push_line(&mut out, "phase", &journal.phase);
    push_line(
        &mut out,
        "retain_previous",
        if journal.retain_previous { "1" } else { "0" },
    );
    push_line(
        &mut out,
        "pending",
        journal.pending.as_deref().unwrap_or("-"),
    );
    push_line(&mut out, "id", &journal.id.to_string());
    push_root(&mut out, "old", &journal.old_root)?;
    if let Some(new_root) = &journal.new_root {
        push_root(&mut out, "new", new_root)?;
    }
    Ok(out)
}

fn parse_root(fields: &HashMap<String, String>, prefix: &str) -> Option<RootIdentity> {
    let path = fields.get(&format!("{prefix}_path"))?;
    let file = parse_key(fields.get(&format!("{prefix}_file"))?)?;
    let dir = parse_key(fields.get(&format!("{prefix}_dir"))?)?;
    Some(RootIdentity {
        path: PathBuf::from(path),
        file,
        dir,
    })
}

fn decode(text: &str) -> Option<Journal> {
    let mut lines = text.lines();
    if lines.next()? != JOURNAL_MAGIC {
        return None;
    }
    let mut fields = HashMap::new();
    for line in lines {
        let (key, value) = line.split_once('=')?;
        fields.insert(key.to_string(), unescape(value));
    }
    let pending = fields.get("pending")?;
    Some(Journal {
        kind: TransitionKind::parse(fields.get("kind")?)?,
        state: JournalState::parse(fields.get("state")?)?,
        phase: fields.get("phase")?.clone(),
        retain_previous: fields.get("retain_previous")? == "1",
        pending: if pending == "-" {
            None
        } else {
            Some(pending.clone())
        },
        id: fields.get("id")?.parse().ok()?,
        old_root: parse_root(&fields, "old")?,
        new_root: parse_root(&fields, "new"),
    })
}

/// Read the journal beside `db`. A missing journal is `Ok(None)`. An unreadable or malformed one is
/// `RecoveryRequired` and is never deleted: it may be the only record of an unfinished transition.
pub(crate) fn read_journal(db: &Path) -> Result<Option<Journal>, LifecycleError> {
    let path = journal_path(db);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(LifecycleError::RecoveryRequired {
                detail: format!("cannot read {}: {error}", path.display()),
            })
        }
    };
    match decode(&text) {
        Some(journal) => Ok(Some(journal)),
        None => Err(LifecycleError::RecoveryRequired {
            detail: format!("{} is not a valid transition journal", path.display()),
        }),
    }
}

/// Remove the journal beside `db`; a missing one is fine.
pub(crate) fn remove_journal(db: &Path) -> Result<(), LifecycleError> {
    let path = journal_path(db);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            rightkit_fs::atomic::sync_dir(
                path.parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new(".")),
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LifecycleError::Fs(rightkit_fs::FsError::io(&path, error))),
    }
}

/// Map a store-lock failure: a held or timed-out lock is `Busy`, anything else is the error itself.
pub(crate) fn map_lock_error(error: DbError, what: &str) -> LifecycleError {
    let busy = matches!(
        &error,
        DbError::Fs(rightkit_fs::FsError::Locked { .. })
            | DbError::Fs(rightkit_fs::FsError::LockTimeout { .. })
    );
    if busy {
        LifecycleError::busy(format!("{what}: {error}"))
    } else {
        LifecycleError::Db(error)
    }
}

/// Take the shared store lock a cooperating opener holds while it has connections open.
pub(crate) fn acquire_shared(
    db: &Path,
    options: &TransitionOptions,
) -> Result<StoreLock, LifecycleError> {
    StoreLock::shared(db, &options.lock_options(TransitionKind::Open))
        .map_err(|error| map_lock_error(error, "the store is being changed by another process"))
}

/// The exclusive guard over one or more store roots, plus the journal beside each.
///
/// Dropping it releases every lock; [`StoreTransition::settle`] is the same drop, spelled out at
/// the point the caller's effects are finished.
#[derive(Debug)]
pub struct StoreTransition {
    kind: TransitionKind,
    roots: Vec<PathBuf>,
    _guards: Vec<StoreLock>,
}

impl StoreTransition {
    /// Take the exclusive guard of every root, in path order, each within
    /// [`TransitionOptions::acquire_timeout`]. A conflicting holder (a shared opener in any
    /// cooperating process, or another transition) returns [`LifecycleError::Busy`]; nothing has
    /// been changed and any guard already taken is released.
    pub fn begin(
        kind: TransitionKind,
        roots: &[&Path],
        options: &TransitionOptions,
    ) -> Result<Self, LifecycleError> {
        let mut ordered: Vec<PathBuf> = roots.iter().map(|root| root.to_path_buf()).collect();
        ordered.sort();
        ordered.dedup();
        let lock_options = options.lock_options(kind);
        let mut guards = Vec::with_capacity(ordered.len());
        for root in &ordered {
            let guard = StoreLock::exclusive(root, &lock_options).map_err(|error| {
                map_lock_error(
                    error,
                    &format!("another process or reader is using {}", root.display()),
                )
            })?;
            guards.push(guard);
        }
        Ok(Self {
            kind,
            roots: ordered,
            _guards: guards,
        })
    }

    pub fn kind(&self) -> TransitionKind {
        self.kind
    }

    /// The roots this guard covers, in the order they were taken.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Write `journal` atomically (private, synced) beside every guarded root.
    pub(crate) fn write_journal(&self, journal: &Journal) -> Result<(), LifecycleError> {
        let body = encode(journal)?;
        for root in &self.roots {
            rightkit_fs::atomic::atomic_write_private(&journal_path(root), body.as_bytes())?;
        }
        Ok(())
    }

    /// Remove the journal beside every guarded root. Every root is attempted; the first failure
    /// is returned.
    pub(crate) fn clear_journal(&self) -> Result<(), LifecycleError> {
        let mut first = None;
        for root in &self.roots {
            if let Err(error) = remove_journal(root) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Release the guard now that the caller's effects have settled.
    pub fn settle(self) {}
}
