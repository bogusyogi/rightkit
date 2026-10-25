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
use rightkit_fs::atomic::{atomic_write_with_access_denied_retry, AccessDeniedRetry, RetryPolicy};
use rightkit_fs::{AtomicWriteOptions, FileKey};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const JOURNAL_MAGIC: &str = "rightkit-db-transition.v1";
// Older readers must refuse relocation metadata rather than apply legacy abandon semantics.
const RELOCATION_JOURNAL_MAGIC: &str = "rightkit-db-transition.v2";

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
    pub relocation: Option<RelocationJournal>,
}

#[derive(Clone, Debug)]
pub(crate) struct RelocationJournal {
    pub candidate: RootIdentity,
    pub transaction_root: RootIdentity,
    pub transaction_id: Option<String>,
    pub published: Option<RootIdentity>,
    pub fs_committed: bool,
    pub switched: bool,
    pub key_store_residue: bool,
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
    out.push_str(if journal.relocation.is_some() {
        RELOCATION_JOURNAL_MAGIC
    } else {
        JOURNAL_MAGIC
    });
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
    if let Some(relocation) = &journal.relocation {
        push_root(&mut out, "candidate", &relocation.candidate)?;
        push_root(&mut out, "transaction_root", &relocation.transaction_root)?;
        if let Some(published) = &relocation.published {
            push_root(&mut out, "published", published)?;
        }
        push_line(
            &mut out,
            "transaction_id",
            relocation.transaction_id.as_deref().unwrap_or("-"),
        );
        push_line(
            &mut out,
            "fs_committed",
            if relocation.fs_committed { "1" } else { "0" },
        );
        push_line(
            &mut out,
            "switched",
            if relocation.switched { "1" } else { "0" },
        );
        push_line(
            &mut out,
            "key_store_residue",
            if relocation.key_store_residue {
                "1"
            } else {
                "0"
            },
        );
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
    let version = lines.next()?;
    let relocation_v2 = version == RELOCATION_JOURNAL_MAGIC;
    if version != JOURNAL_MAGIC && !relocation_v2 {
        return None;
    }
    let mut fields = HashMap::new();
    for line in lines {
        let (key, value) = line.split_once('=')?;
        if relocation_v2
            && !matches!(
                key,
                "kind"
                    | "state"
                    | "phase"
                    | "retain_previous"
                    | "pending"
                    | "id"
                    | "old_path"
                    | "old_file"
                    | "old_dir"
                    | "new_path"
                    | "new_file"
                    | "new_dir"
                    | "candidate_path"
                    | "candidate_file"
                    | "candidate_dir"
                    | "transaction_root_path"
                    | "transaction_root_file"
                    | "transaction_root_dir"
                    | "published_path"
                    | "published_file"
                    | "published_dir"
                    | "transaction_id"
                    | "fs_committed"
                    | "switched"
                    | "key_store_residue"
            )
        {
            return None;
        }
        if fields.insert(key.to_string(), unescape(value)).is_some() && relocation_v2 {
            return None;
        }
    }
    let pending = fields.get("pending")?;
    let boolean = |name: &str| -> Option<bool> {
        match fields.get(name)?.as_str() {
            "0" => Some(false),
            "1" => Some(true),
            _ if !relocation_v2 => Some(false),
            _ => None,
        }
    };
    let journal = Journal {
        kind: TransitionKind::parse(fields.get("kind")?)?,
        state: JournalState::parse(fields.get("state")?)?,
        phase: fields.get("phase")?.clone(),
        retain_previous: boolean("retain_previous")?,
        pending: if pending == "-" {
            None
        } else {
            Some(pending.clone())
        },
        id: fields.get("id")?.parse().ok()?,
        old_root: parse_root(&fields, "old")?,
        new_root: parse_root(&fields, "new"),
        relocation: if fields.contains_key("candidate_path") {
            Some(RelocationJournal {
                candidate: parse_root(&fields, "candidate")?,
                transaction_root: parse_root(&fields, "transaction_root")?,
                transaction_id: match fields.get("transaction_id") {
                    Some(id) if id == "-" => None,
                    Some(id) if !id.is_empty() => Some(id.clone()),
                    _ if relocation_v2 => return None,
                    _ => None,
                },
                published: if fields.contains_key("published_path") {
                    Some(parse_root(&fields, "published")?)
                } else {
                    None
                },
                fs_committed: boolean("fs_committed")?,
                switched: boolean("switched")?,
                key_store_residue: boolean("key_store_residue")?,
            })
        } else {
            None
        },
    };
    if relocation_v2 {
        let metadata = journal.relocation.as_ref()?;
        if journal.kind != TransitionKind::Relocate
            || journal.new_root.is_none()
            || metadata.transaction_root.file.is_none()
            || [
                &journal.old_root,
                journal.new_root.as_ref()?,
                &metadata.candidate,
                &metadata.transaction_root,
            ]
            .iter()
            .any(|root| root.path.as_os_str().is_empty())
            || (fields.keys().any(|key| key.starts_with("published_"))
                && metadata.published.is_none())
            || metadata
                .published
                .as_ref()
                .is_some_and(|root| root.path.as_os_str().is_empty() || root.file.is_none())
            || ((metadata.switched || metadata.key_store_residue) && !metadata.fs_committed)
            || (metadata.fs_committed
                && metadata
                    .published
                    .as_ref()
                    .and_then(|root| root.file)
                    .is_none())
        {
            return None;
        }
    }
    Some(journal)
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

#[cfg(test)]
mod relocation_journal_tests {
    use super::*;

    fn journal() -> Journal {
        let root = |path: &str| RootIdentity {
            path: PathBuf::from(path),
            file: Some(FileKey {
                volume: 1,
                index: 2,
            }),
            dir: None,
        };
        Journal {
            kind: TransitionKind::Relocate,
            state: JournalState::Active,
            phase: "quiesced".into(),
            retain_previous: false,
            pending: None,
            id: 9,
            old_root: root("old.db"),
            new_root: Some(RootIdentity {
                file: None,
                ..root("new.db")
            }),
            relocation: Some(RelocationJournal {
                candidate: RootIdentity {
                    file: None,
                    ..root("candidate.db")
                },
                transaction_root: root("transaction-root"),
                transaction_id: None,
                published: None,
                fs_committed: false,
                switched: false,
                key_store_residue: false,
            }),
        }
    }

    #[test]
    fn relocation_metadata_requires_v2_while_legacy_transitions_stay_v1() {
        let mut value = journal();
        let encoded = encode(&value).unwrap();
        assert_eq!(encoded.lines().next(), Some(RELOCATION_JOURNAL_MAGIC));
        assert!(decode(&encoded).unwrap().relocation.is_some());
        let metadata = value.relocation.as_mut().unwrap();
        metadata.fs_committed = true;
        metadata.switched = true;
        metadata.key_store_residue = true;
        metadata.published = Some(value.old_root.clone());
        let committed = decode(&encode(&value).unwrap()).unwrap();
        assert!(committed.relocation.unwrap().switched);
        value.relocation = None;
        value.kind = TransitionKind::Rotate;
        let legacy = encode(&value).unwrap();
        assert_eq!(legacy.lines().next(), Some(JOURNAL_MAGIC));
        assert_eq!(decode(&legacy).unwrap().kind, TransitionKind::Rotate);
        assert!(decode(&legacy.replacen(JOURNAL_MAGIC, RELOCATION_JOURNAL_MAGIC, 1)).is_none());
    }

    #[test]
    fn malformed_relocation_v2_never_decodes_as_recoverable_authority() {
        let encoded = encode(&journal()).unwrap();
        for malformed in [
            encoded.replace("fs_committed=0", "fs_committed=true"),
            encoded.replace("switched=0", "switched=1"),
            encoded.replace("fs_committed=0", "fs_committed=1"),
            encoded.replace("key_store_residue=0", "key_store_residue=1"),
            encoded.replace("candidate_file=-\n", ""),
            encoded.replace("new_dir=-\n", ""),
            encoded.replace("transaction_id=-\n", ""),
            format!("{encoded}switched=0\n"),
            format!("{encoded}unknown=1\n"),
            format!("{encoded}published_file=1:2\n"),
        ] {
            assert!(
                decode(&malformed).is_none(),
                "accepted malformed journal: {malformed}"
            );
        }
    }
}

/// The bounded opt-in that lets a Windows publish step ride out `ERROR_ACCESS_DENIED` (and the
/// sharing and lock violations) while a scanner or indexer still holds a file it just saw. The
/// bound is `crate::transient`'s; after it the original error is returned unchanged.
pub(super) fn access_denied_retry() -> AccessDeniedRetry {
    AccessDeniedRetry::new(RetryPolicy::new(
        crate::transient::ATTEMPTS,
        crate::transient::DELAY,
    ))
}

/// Remove the journal beside `db`; a missing one is fine. A Windows delete refusal that clears by
/// itself (a scanner holding the file) is retried a bounded number of times.
pub(crate) fn remove_journal(db: &Path) -> Result<(), LifecycleError> {
    let path = journal_path(db);
    match crate::transient::retry_transient(|| std::fs::remove_file(&path)) {
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

    /// Write `journal` atomically (private, synced) beside every guarded root. Every write after
    /// the first replaces the previous journal, which on Windows a scanner holding it without
    /// delete sharing refuses with a transient `ERROR_ACCESS_DENIED`, so the replace is retried a
    /// bounded number of times ([`access_denied_retry`]).
    pub(crate) fn write_journal(&self, journal: &Journal) -> Result<(), LifecycleError> {
        let body = encode(journal)?;
        for root in &self.roots {
            atomic_write_with_access_denied_retry(
                &journal_path(root),
                AtomicWriteOptions {
                    private: true,
                    ..AtomicWriteOptions::default()
                },
                access_denied_retry(),
                |file| file.write_all(body.as_bytes()),
            )?;
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
