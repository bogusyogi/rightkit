//! Crash-recoverable multi-file publication: [`FileTransaction::prepare`], [`PreparedTransaction::commit`]
//! and [`recover_transactions`] (feature `transaction`).
//!
//! A transaction publishes several files under one root (or, see "Several roots", under several) so that a crash or failure at any point
//! leaves either the old state, the new state, or a recoverable state that
//! [`recover_transactions`] resolves deterministically. It is **not** all-file atomic visibility: a
//! reader that opens two of the files can see one old and one new while a commit runs. Consumers
//! that need coherent multi-file reads publish a generation directory plus an atomic pointer, or
//! use their own reader/writer protocol. What it guarantees is recoverability and that cooperating
//! writers of one root are serialized.
//!
//! ```no_run
//! use rightkit_fs::transaction::{FileTransaction, TransactionEntry, TransactionOptions, TransactionReport};
//! use std::path::Path;
//!
//! let root = Path::new("/data/app");
//! let prepared = FileTransaction::prepare(
//!     root,
//!     vec![
//!         TransactionEntry::new("settings.json", "/tmp/stage/settings.json"),
//!         TransactionEntry::create_new("notes/new.txt", "/tmp/stage/new.txt").private(),
//!     ],
//!     TransactionOptions::default(),
//! )?;
//! match prepared.commit()? {
//!     TransactionReport::Committed(done) => println!("published {} files", done.published.len()),
//!     TransactionReport::RolledBack(undone) => println!("nothing changed: {}", undone.cause),
//!     TransactionReport::RecoveryRequired(stuck) => println!("run recover_transactions: {}", stuck.cause),
//!     _ => {}
//! }
//! # Ok::<(), rightkit_fs::FsError>(())
//! ```
//!
//! # Several roots
//!
//! Entries may live under different roots (folders the user chose, each possibly reached through
//! a link), with one commit decision for all of them. [`TransactionEntry::in_root`] names an
//! entry's root; an entry without one is under the root passed to [`FileTransaction::prepare`],
//! the **coordinating root**. Only the coordinating root holds the protocol's records: the
//! operation lock, `.rktxn/<id>/journal` and `.rktxn/<id>/commit`. Every other root holds staged
//! and backup names only, beside the destinations they belong to, so a media folder never gains a
//! `.rktxn` directory. Pass the app's own data directory as the coordinating root and the user's
//! folders as the other roots.
//!
//! * Each root is resolved once at preparation (links are allowed in the root path, never in the
//!   relative part) and recorded in the journal with its file identity; the journal then uses the
//!   `rktxn-journal 2` format. A single-root transaction keeps the version 1 format and layout, so
//!   journals written before this existed recover unchanged.
//! * Roots of one transaction may not be the same directory under two names (they are one root)
//!   and may not be inside one another; duplicate or overlapping destinations are refused per
//!   root.
//! * The commit protocol is the table below, run over all entries in entry order; the marker in
//!   the coordinating root is the one decision for every root. [`PreparedTransaction::commit_then`]
//!   runs its `after` before that marker, so a caller's own record and the files of every root
//!   are all new or all old.
//! * [`recover_transactions`] on the coordinating root resolves the whole transaction from that
//!   journal: it finds each other root again by its recorded path and proves it is the recorded
//!   directory before touching anything in it. A root that cannot be reached, or is another
//!   directory now, keeps the transaction [`RecoveryOutcome::Retained`]: no root is restored,
//!   completed or deleted until it is available (the coordinating root takes no new transaction
//!   meanwhile). Recovery on another root alone finds nothing to do; call it on the coordinating
//!   root at start-up.
//! * Only the coordinating root's lock is taken, so another process that writes the same
//!   non-coordinating folder outside this protocol is not serialized with it; the identity checks
//!   of every entry (`ExpectedPrior`, the recorded prior file) still detect a changed destination
//!   and fail closed. Cooperating writers must share one coordinating root.
//! * A leftover of a committed transaction that no longer matches its identity, in a
//!   non-coordinating root, is renamed in its own directory to `<name>.retained` (not moved to the
//!   coordinating root, which may be another volume).
//!
//! # Layout
//!
//! Everything the protocol owns is under `<root>/.rktxn/`, an owner-only directory:
//!
//! | Path | Holds |
//! |---|---|
//! | `.rktxn/lock` | the operation lock ([`FileLock`], exclusive). `commit` and `recover_transactions` take it; it is never deleted |
//! | `.rktxn/<id>/journal` | the journal: entries, the identity (file key, size, modification time) of each prior destination and each staged copy, a CRC-32. Written complete and flushed before any live destination is touched (rewritten once, when the staged copies exist) |
//! | `.rktxn/<id>/commit` | the commit marker, bound to the journal's CRC. Its existence is the single decision that the transaction is committed |
//! | `.rktxn/retained/<id>/` | material of a **committed** transaction that no longer matched its recorded identity (a backup somebody edited in place), moved here instead of being deleted or blocking the root. Never swept, never read by the protocol |
//! | `<dir of destination>/.<name>.rktxn-<id>-<n>s` | the staged copy of entry `n`, in the destination's own directory, so the publish is a same-filesystem rename and the file carries that directory's ACL |
//! | `<dir of destination>/.<name>.rktxn-<id>-<n>b` | the rollback material for entry `n`: a hard link to (or the renamed) prior file |
//!
//! Journal, marker and temp names are never swept by age. Nothing in the protocol deletes material
//! it cannot match to a recorded identity.
//!
//! # Commit protocol
//!
//! Notation: *flush* is a durable write (file flushed, rename, directory synced on Unix,
//! write-through move on Windows).
//!
//! | # | Step | What a crash after it leaves | Recovery |
//! |---|---|---|---|
//! | 1 | Take the operation lock; refuse if an earlier transaction directory exists | nothing | n/a (the refusal is `Err(FsError::Locked)`) |
//! | 2 | Re-verify every destination against [`ExpectedPrior`], every candidate against its prepared identity | nothing | n/a |
//! | 3 | Create `.rktxn/<id>`; flush the journal, phase `staging` | an empty journal-only directory | discarded |
//! | 4 | For each entry: create missing parent directories, copy the candidate to the staged name through [`AtomicWriter`] (flushed, `NoReplace`, private when the entry is), verify the candidate did not change while it was read. The new file's file key is **journaled before it gets its staged name** (journal rewritten, phase `staging`) | staged copies only; no live destination touched | discarded: a staged name is removed only if it matches the journaled key, anything else is retained |
//! | 5 | Flush the journal again, phase `staged`, with the staged identities | as 4, journal complete | rolled back (every entry is untouched) |
//! | 6 | Per entry, in order: verify the destination still has its recorded identity; make the backup (hard link, else exclusive rename aside); publish the staged copy (replace, or exclusive when the destination was absent) | some entries published | rolled back |
//! | 7 | Flush the commit marker | all published, marker durable | completed (cleanup only) |
//! | 8 | Remove backups, then the journal, then the marker, then the directory; the transaction directory is synced after the journal unlink and again after the marker unlink. A backup or staged name that no longer matches its identity is moved to `.rktxn/retained/<id>/` | committed, some leftovers | completed |
//!
//! The decision point is step 7. Before it a crash or a failure rolls back; after it the commit is
//! final and only cleanup remains. Rollback never needs the journal to say which entry was
//! reached: progress is read from which of the destination, the staged name and the backup name
//! exist, and each is checked against its recorded identity before it is touched (`classify`
//! is the pure decision table, unit-tested for every combination).
//!
//! # Outcomes
//!
//! [`PreparedTransaction::commit`] returns `Err` only when **nothing was changed** (including a
//! failure before the journal exists, or one whose cleanup left nothing behind). From the first
//! journal write on it returns a [`TransactionReport`]: [`Committed`](TransactionReport::Committed),
//! [`RolledBack`](TransactionReport::RolledBack) (a failure after publication started, undone and
//! verified), or [`RecoveryRequired`](TransactionReport::RecoveryRequired) (the state could not be
//! decided or restored in this process; the journal and every piece of rollback material are
//! kept). A post-publication failure is never reported as "nothing happened".
//!
//! # What is and is not defended
//!
//! * Entries are confined: a destination is a bounded relative path (no `..`, no root or drive, at
//!   most [`MAX_RELATIVE_BYTES`] bytes and 64 components, UTF-8), no component may be a symlink or
//!   reparse point, a destination may not be inside `.rktxn`, duplicate and overlapping destinations
//!   (`a` and `a/b`) are refused. Parents are re-resolved at commit and at recovery.
//! * Nothing is removed on a name alone, and identity is proven again before **every** delete
//!   attempt, so a name that changes hands during a retry's pause is never deleted. A staged copy is deleted by discard only if the journal
//!   holds the identity of the file this transaction created there (its file key is journaled
//!   before the file is given the staged name; phase `staged` journals hold the full identity). A
//!   file at a staged name without a record, or with another identity (another filesystem
//!   mounted below the directory, a file somebody put there), is retained and reported.
//! * A destination or candidate that changed since it was recorded is [`FsError::Conflict`]. A
//!   destination that someone replaced *between the last check and the rename* is not detected:
//!   no platform rename compares content. The window is one syscall wide and the replaced
//!   version is the retained backup, so recovery and rollback find it by identity and report it
//!   instead of overwriting it.
//! * Identity is the file key (device number and file index), size and modification time,
//!   always compared in full. A file index is unique only within one filesystem, and a device
//!   number is not proof of which filesystem it is across a restart, so **any** device change
//!   the journal cannot account for is fail-closed: a root that has the recorded file index but
//!   another device number, or a journaled file that reports another device number, is retained
//!   (the journal and every piece of rollback material stay, nothing is classified as published,
//!   removed or restored) on every platform. The consequence is that a Unix reboot or remount
//!   that renumbers the device leaves the transaction retained until a person resolves it; see
//!   [`RecoveryOutcome::Retained`]. A filesystem whose file index changes on rename (some
//!   FAT-class volumes) is retained the same way. That is the safe direction, not data loss.
//! * A committed transaction's leftover that no longer matches (the usual cause: an editor wrote
//!   through the hard link that was the backup) is moved into `.rktxn/retained/<id>/` and
//!   reported, so the commit decision never blocks the next commit. Before the marker exists a
//!   mismatch still retains the whole transaction: nothing is decided yet and a person chooses.
//! * On volumes without hard links, exclusive rename may reserve an empty destination name
//!   before moving a file. A crash inside that fallback can leave an unjournaled empty backup
//!   or destination. Recovery retains it together with the journal and rollback material;
//!   it never assumes an empty file belongs to this transaction. A person must resolve this
//!   ambiguity before another transaction can use the root.
//! * Destinations and candidates must not carry POSIX `fcntl` record locks held by this process:
//!   staging and identity probes open and close these files, and closing any descriptor releases
//!   this process's record locks on that inode. Use the separate transaction operation lock.
//! * Files under Windows cloud-sync placeholders carry a reparse point and are refused like
//!   symlinks.
//! * Missing parent directories of a destination are created durably and are not removed by a
//!   rollback (an empty directory is not a prior state worth a delete).
//! * The Windows replace of a file whose DACL was customised gives the new file the directory's
//!   inherited DACL, as [`AtomicWriter`] does for `Privacy::Inherit`. Unix keeps the prior file's
//!   mode bits. `private` entries are owner-only on both.
//! * Crash orphans of the staged copy itself (`.<staged name>.rktmp-<pid>-...`, left only by a
//!   kill inside the copy) are ordinary [`AtomicWriter`] temps; [`crate::sweep_stale_temps`]
//!   reclaims them.
//! * A candidate is read and copied, never moved or modified. A large candidate is read twice
//!   (once by the caller to write it, once to stage it); a caller that can write the candidate
//!   directly under the root should still stage here, because the copy is what the journal binds.
//!
//! # Lock order
//!
//! The operation lock is the last lock in the crate's order: [`crate::write_locks::KeyedWriteLocks`]
//! slot, then its process lock, then this lock. A transaction takes no keyed lock. Take two
//! roots' locks in path order. The lock is not re-entrant: calling `commit` or
//! `recover_transactions` for a root from inside another call on that root waits for the
//! timeout and returns [`FsError::LockTimeout`].
//!
//! # For `rightkit-db` and the daemon
//!
//! `EncryptedStore::relocate` copies the database and key itself (its own verified backup) and moves
//! no companions; the companions of a root (settings, logs, caches) go through one transaction
//! rooted at the **new** root, with the old root's files as candidates. The old root is never
//! touched by this module, so it stays complete and openable until the caller's separate,
//! identity-checked cleanup. `RootCutover` runs `recover_transactions(new_root)` at startup before
//! deciding which root to open.

use crate::atomic::{
    create_dir_all_durable, publish_file_with, publish_file_with_access_denied_retry, try_sync_dir,
    AccessDeniedRetry, AtomicWriteOptions, AtomicWriter, Privacy, ReplaceMode, RetryPolicy,
};
use crate::error::{FsError, Result};
use crate::file_key::FileKey;
use crate::lock::{FileLock, LockOptions};
use crate::plain::canonicalize_plain;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const AREA: &str = ".rktxn";
const LOCK_NAME: &str = "lock";
const RETAINED_DIR: &str = "retained";
const JOURNAL: &str = "journal";
const COMMIT: &str = "commit";
const JOURNAL_MAGIC: &str = "rktxn-journal 1";
/// A journal of a transaction whose entries span several roots (see the module docs).
const JOURNAL_MAGIC_MULTI: &str = "rktxn-journal 2";
const COMMIT_MAGIC: &str = "rktxn-commit 1";
const SIDE_MARKER: &str = ".rktxn-";
const SIDE_PREFIX_MAX: usize = 64;
const COPY_BUFFER: usize = 64 * 1024;
const MAX_COMPONENTS: usize = 64;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

/// Most entries one transaction may hold.
pub const MAX_ENTRIES: usize = 256;
/// Longest relative destination, in bytes of UTF-8.
pub const MAX_RELATIVE_BYTES: usize = 1024;

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------------------------
// Protocol steps
// ---------------------------------------------------------------------------------------------

macro_rules! define_step {
    ($vis:vis) => {
        /// A boundary of the commit and recovery protocol.
        #[derive(Clone, Debug, PartialEq, Eq)]
        #[non_exhaustive]
        #[allow(dead_code)]
        $vis enum Step {
            /// Before the first journal (phase `staging`) is flushed.
            BeforeJournalStaging,
            /// After the first journal is durable.
            AfterJournalStaging,
            /// Before the staged copy of entry `n` is made.
            BeforeStage(usize),
            /// The staged file of entry `n` exists under its writer's temporary name; its file key
            /// is about to be journaled. The staged name does not exist yet.
            BeforeStageClaim(usize),
            /// The file key of entry `n`'s staged file is durable in the journal (phase
            /// `staging`); the staged name still does not exist.
            AfterStageClaim(usize),
            /// After the staged copy of entry `n` is durable.
            AfterStage(usize),
            /// Before the journal is rewritten with the staged identities (phase `staged`).
            BeforeJournalStaged,
            /// After that journal is durable.
            AfterJournalStaged,
            /// Before the backup of entry `n`'s prior destination is made.
            BeforeBackup(usize),
            /// After the destination of entry `n` was re-verified and immediately before its backup
            /// is made (a hard link, or the exclusive rename aside). A write made here lands
            /// between the check and the link: the link then shares the changed inode.
            BeforeLink(usize),
            /// After the backup of entry `n` exists and was checked against the recorded identity.
            AfterBackup(usize),
            /// Before the staged copy of entry `n` is renamed into place.
            BeforePublish(usize),
            /// After the staged copy of entry `n` is in place (and its directory synced).
            AfterPublish(usize),
            /// Before the commit marker is flushed.
            BeforeCommitMarker,
            /// After the commit marker is durable: the transaction is committed.
            AfterCommitMarker,
            /// Before the backup of entry `n` is removed (commit cleanup).
            BeforeCleanup(usize),
            /// After the backup of entry `n` is removed.
            AfterCleanup(usize),
            /// Before a rollback writes the journal mark that entry `n` was never published (a
            /// destination somebody else changed, found while its staged copy still exists).
            BeforeRollbackMark(usize),
            /// After that mark is durable and before the staged copy is removed.
            AfterRollbackMark(usize),
            /// Before entry `n` is restored (rollback).
            BeforeRestore(usize),
            /// After entry `n` is restored.
            AfterRestore(usize),
            /// Before the journal file is removed.
            BeforeJournalRemove,
            /// After the journal file is removed and the transaction directory synced.
            AfterJournalRemove,
            /// After the commit marker (and any record temp) is removed and the transaction
            /// directory synced, before the directory itself is removed.
            AfterMarkerRemove,
            /// Stands in for the flush of the directory a leftover was moved out of (set-aside of
            /// a committed transaction's changed backup). An error here is that flush failing.
            SetAsideSyncSource,
            /// Stands in for the flush of `.rktxn/retained/<id>` after a leftover was moved into it.
            SetAsideSyncTarget,
            /// Stands in for the look at a set-aside copy that an earlier attempt may have made;
            /// an error here is that look failing. Only a proven "not found" means absent.
            SetAsideProbe,
            /// The pause between two attempts to delete a file the protocol identified (after a
            /// sharing or access failure), before the identity is proven again. A hook that
            /// replaces the name here shows the next attempt refusing to delete the newcomer.
            DeleteBackoff,
        }
    };
}

define_step!(pub(crate));

#[inline(always)]
fn fire(_step: Step, _at: &Path) -> Result<()> {
    Ok(())
}

#[inline(always)]
fn forced_delete_failure() -> Option<io::Error> {
    None
}

#[inline(always)]
fn aside_forced() -> bool {
    false
}

// ---------------------------------------------------------------------------------------------
// Public vocabulary
// ---------------------------------------------------------------------------------------------

/// What a destination must look like when the transaction commits.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpectedPrior {
    /// Replace whatever regular file is there, or create the file. The identity found under the
    /// operation lock is what rollback restores.
    Any,
    /// The destination must not exist.
    Absent,
    /// The destination must be the file with this identity ([`FileKey::of_path`], read earlier by
    /// the caller). An atomic replace gives a file a new key, so this detects "someone replaced it
    /// since I read it", not an in-place edit; use `Sha256` for that.
    Identity(FileKey),
    /// The destination's content must hash to this lowercase hex SHA-256.
    #[cfg(feature = "digest")]
    Sha256(String),
}

/// How an entry's destination is published.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Publication {
    /// Replace an existing regular file, or create the file.
    #[default]
    Replace,
    /// Create only: an existing destination is [`FsError::AlreadyExists`].
    CreateNew,
}

/// One file of a transaction.
#[derive(Clone, Debug)]
pub struct TransactionEntry {
    destination: PathBuf,
    candidate: PathBuf,
    expect: ExpectedPrior,
    publication: Publication,
    private: bool,
    root: Option<PathBuf>,
}

impl TransactionEntry {
    /// Publish a copy of `candidate` at `destination` (relative to the root), replacing an
    /// existing file or creating it, with [`ExpectedPrior::Any`].
    pub fn new(destination: impl Into<PathBuf>, candidate: impl Into<PathBuf>) -> Self {
        Self {
            destination: destination.into(),
            candidate: candidate.into(),
            expect: ExpectedPrior::Any,
            publication: Publication::Replace,
            private: false,
            root: None,
        }
    }

    /// Publish a copy of `candidate` at `destination`, which must not exist.
    pub fn create_new(destination: impl Into<PathBuf>, candidate: impl Into<PathBuf>) -> Self {
        Self {
            publication: Publication::CreateNew,
            expect: ExpectedPrior::Absent,
            ..Self::new(destination, candidate)
        }
    }

    /// Require the destination to match `expect` when the transaction commits.
    pub fn expect(mut self, expect: ExpectedPrior) -> Self {
        self.expect = expect;
        self
    }

    /// Make the staged copy, and so the published file, owner-only (mode 0600 / owner DACL).
    /// Without it Unix keeps the prior file's mode bits (the candidate's for a new file) and
    /// Windows takes the destination directory's inherited ACL.
    pub fn private(mut self) -> Self {
        self.private = true;
        self
    }

    /// Publish this entry under `root` instead of the transaction's coordinating root (the one
    /// passed to [`FileTransaction::prepare`]). The destination stays a bounded relative path under
    /// `root`; `root` itself is resolved once and may contain links (a folder the user chose), but
    /// no component of the relative part may. See the module docs, "Several roots".
    pub fn in_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// The explicit root of this entry, if [`in_root`](Self::in_root) set one.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The relative destination.
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    /// The candidate file that will be copied.
    pub fn candidate(&self) -> &Path {
        &self.candidate
    }
}

/// Bounds and retry policy of a transaction.
#[derive(Clone, Debug)]
pub struct TransactionOptions {
    lock_timeout: Duration,
    retry: Option<RetryPolicy>,
    access_denied: Option<AccessDeniedRetry>,
}

impl Default for TransactionOptions {
    /// A 30 second wait for the operation lock, and five 50 ms retries of a Windows sharing or
    /// lock violation in each publish step.
    fn default() -> Self {
        Self {
            lock_timeout: Duration::from_secs(30),
            retry: Some(RetryPolicy::new(5, Duration::from_millis(50))),
            access_denied: None,
        }
    }
}

impl TransactionOptions {
    /// How long to wait for the root's operation lock (`Duration::ZERO` tries once).
    pub fn lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    /// Retry policy for Windows sharing and lock violations in every publish step (`None`: no
    /// retry). It also bounds the retries of the cleanup deletes of the protocol's own files
    /// (backups, staged names, journal, marker), which on Windows also retry access-denied
    /// (error 5) with doubling pauses; the bound applies to each file, not to the transaction.
    pub fn retry(mut self, retry: Option<RetryPolicy>) -> Self {
        self.retry = retry;
        self
    }

    /// Opt in to the bounded retry of Windows `ERROR_ACCESS_DENIED` in every publish step
    /// ([`AccessDeniedRetry`]); it replaces [`TransactionOptions::retry`] for those steps.
    pub fn access_denied_retry(mut self, retry: AccessDeniedRetry) -> Self {
        self.access_denied = Some(retry);
        self
    }
}

/// Something the protocol kept instead of deleting, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetainedItem {
    /// The retained path.
    pub path: PathBuf,
    /// Why it was kept (an identity that does not match, a failed removal, unknown material).
    pub reason: String,
}

impl RetainedItem {
    fn new(path: impl Into<PathBuf>, reason: impl std::fmt::Display) -> Self {
        Self {
            path: path.into(),
            reason: reason.to_string(),
        }
    }
}

/// Every destination is published and the commit marker is durable.
#[derive(Debug)]
#[non_exhaustive]
pub struct CommittedReport {
    /// The transaction id.
    pub id: String,
    /// The published destinations, in entry order.
    pub published: Vec<PathBuf>,
    /// Cleanup that did not finish (a failed removal) and leftovers that no longer matched their
    /// recorded identity and were moved to `.rktxn/retained/<id>/` (their `reason` says so; they
    /// block nothing). The commit is final regardless; `recover_transactions` retries the cleanup
    /// that did not finish and reports what it still cannot remove.
    pub retained: Vec<RetainedItem>,
}

/// A failure after publication started was undone and verified: every destination is as it was.
#[derive(Debug)]
#[non_exhaustive]
pub struct RolledBackReport {
    /// The transaction id.
    pub id: String,
    /// The failure that ended the commit.
    pub cause: FsError,
    /// Destinations that had been changed and were restored.
    pub restored: Vec<PathBuf>,
}

/// The state could not be decided or restored here; the journal and rollback material are kept.
#[derive(Debug)]
#[non_exhaustive]
pub struct RecoveryRequiredReport {
    /// The transaction id.
    pub id: String,
    /// The failure that ended the commit.
    pub cause: FsError,
    /// What was kept and why. Includes material that did not match its recorded identity.
    pub retained: Vec<RetainedItem>,
    /// The journal that [`recover_transactions`] will read.
    pub journal: PathBuf,
}

/// How [`PreparedTransaction::commit`] ended, once a journal existed.
#[derive(Debug)]
#[non_exhaustive]
pub enum TransactionReport {
    /// Committed.
    Committed(CommittedReport),
    /// Failed and rolled back.
    RolledBack(RolledBackReport),
    /// Failed; call [`recover_transactions`].
    RecoveryRequired(RecoveryRequiredReport),
}

impl TransactionReport {
    /// The transaction id.
    pub fn id(&self) -> &str {
        match self {
            Self::Committed(report) => &report.id,
            Self::RolledBack(report) => &report.id,
            Self::RecoveryRequired(report) => &report.id,
        }
    }

    /// True for [`TransactionReport::Committed`].
    pub fn is_committed(&self) -> bool {
        matches!(self, Self::Committed(_))
    }
}

/// What recovery did with one transaction directory.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryOutcome {
    /// It had not started changing live destinations; its staged copies and journal were removed.
    Discarded,
    /// It was not committed; every destination was restored (or was untouched) and the journal
    /// removed.
    RolledBack,
    /// It was committed; its cleanup finished.
    Completed,
    /// It was committed and its cleanup finished, but some leftovers no longer matched their
    /// recorded identity (a backup edited in place, say). They were moved, not deleted, into
    /// `.rktxn/retained/<id>/` and are listed here; the root takes new transactions.
    CompletedSetAside {
        /// The leftovers that were set aside, with the path they were moved to in `reason`.
        items: Vec<RetainedItem>,
    },
    /// Something did not match its recorded identity, could not be read, or could not be removed.
    /// Nothing listed was deleted or overwritten. The transaction stays in place and the next
    /// recovery re-evaluates it.
    Retained {
        /// Why recovery stopped.
        reason: String,
        /// The material kept.
        items: Vec<RetainedItem>,
    },
}

/// One recovered transaction directory.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecoveredTransaction {
    /// The transaction id (the directory name under `.rktxn`).
    pub id: String,
    /// What recovery did.
    pub outcome: RecoveryOutcome,
}

/// What [`recover_transactions`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecoveryReport {
    /// Every transaction directory found, newest first.
    pub transactions: Vec<RecoveredTransaction>,
}

impl RecoveryReport {
    /// True when nothing was retained: every transaction found is resolved. A
    /// [`RecoveryOutcome::CompletedSetAside`] counts as resolved but holds a file somebody edited
    /// under `.rktxn/retained/<id>/`: a caller that surfaces user content should look at its
    /// `items` as well.
    pub fn is_clean(&self) -> bool {
        !self
            .transactions
            .iter()
            .any(|t| matches!(t.outcome, RecoveryOutcome::Retained { .. }))
    }
}

// ---------------------------------------------------------------------------------------------
// Identity and probing
// ---------------------------------------------------------------------------------------------

/// What the protocol knows about one file: the filesystem's key (device number and file index)
/// plus size and modification time, always compared in full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ident {
    key: FileKey,
    len: u64,
    modified_ns: u128,
}

impl Ident {
    fn of_file(file: &File, shown: &Path) -> Result<Self> {
        let key = FileKey::of_file(file, shown)?;
        let meta = file.metadata().map_err(|e| FsError::io(shown, e))?;
        let modified_ns = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_nanos());
        Ok(Self {
            key,
            len: meta.len(),
            modified_ns,
        })
    }

    fn render(&self) -> String {
        format!(
            "{:x}.{:x}.{}.{}",
            self.key.volume, self.key.index, self.len, self.modified_ns
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let volume = u64::from_str_radix(parts.next()?, 16).ok()?;
        let index = u64::from_str_radix(parts.next()?, 16).ok()?;
        let len = parts.next()?.parse().ok()?;
        let modified_ns = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            key: FileKey { volume, index },
            len,
            modified_ns,
        })
    }
}

/// The state of one path, as the protocol compares it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Probe {
    Missing,
    File(Ident),
    /// Present but not a regular file (a directory, a link, a device).
    Other(String),
}

fn is_link(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

fn probe(path: &Path) -> Result<Probe> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Probe::Missing),
        Err(e) => return Err(FsError::io(path, e)),
    };
    if is_link(&meta) {
        return Ok(Probe::Other("a symbolic link or reparse point".into()));
    }
    if !meta.is_file() {
        return Ok(Probe::Other("not a regular file".into()));
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Probe::Missing),
        Err(e) => return Err(FsError::io(path, e)),
    };
    Ok(Probe::File(Ident::of_file(&file, path)?))
}

fn conflict(path: &Path, reason: &str) -> FsError {
    FsError::Conflict {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A delete that fails because another process still holds the file open without delete sharing
/// (Windows `ERROR_ACCESS_DENIED` while a delete is pending, `ERROR_SHARING_VIOLATION`,
/// `ERROR_LOCK_VIOLATION`). Never true off Windows. The classification is
/// [`crate::transient::is_transient_refusal`], the crate's one owner of it.
fn transient_delete_error(error: &io::Error) -> bool {
    crate::transient::is_transient_refusal(error)
}

/// A failure worth another attempt when deleting: the Windows classes above.
fn transient_removal_error(error: &io::Error) -> bool {
    transient_delete_error(error)
}

/// Delete the file at `path` only while it is still the file `identical` accepts. Identity is
/// proven again immediately before **every** attempt, including the ones after a pause, so a name
/// that changed hands during a retry's backoff is never deleted: the newcomer is retained and
/// reported (`mismatch` says why). A file that is gone is success. Where identity cannot be read,
/// nothing is deleted.
///
/// The check and the unlink are still two calls (no platform unlinks through a verified handle
/// from `std`), so the window is one syscall wide, as for every identity-checked removal in this
/// module; the retry no longer widens it to a whole backoff.
fn remove_while_identical(
    path: &Path,
    identical: impl Fn(&Ident) -> bool,
    mismatch: &str,
    retry: Option<RetryPolicy>,
) -> std::result::Result<(), RetainedItem> {
    let stop: std::cell::RefCell<Option<RetainedItem>> = std::cell::RefCell::new(None);
    let attempts = std::cell::Cell::new(0u32);
    let result = retry_while(retry, transient_removal_error, || {
        if attempts.get() > 0 {
            // The pause between attempts is a window in which the name can change hands.
            if let Err(error) = fire(Step::DeleteBackoff, path) {
                *stop.borrow_mut() = Some(RetainedItem::new(path, error));
                return Err(io::Error::other("stopped before the next attempt"));
            }
        }
        attempts.set(attempts.get() + 1);
        match probe(path) {
            Ok(Probe::Missing) => Ok(()),
            Ok(Probe::File(now)) if identical(&now) => match forced_delete_failure() {
                Some(error) => Err(error),
                None => remove_if_exists(path),
            },
            Ok(_) => {
                *stop.borrow_mut() = Some(RetainedItem::new(path, mismatch));
                Err(io::Error::other("identity no longer proven"))
            }
            Err(error) => {
                *stop.borrow_mut() = Some(RetainedItem::new(path, error));
                Err(io::Error::other("identity cannot be read"))
            }
        }
    });
    if let Some(item) = stop.into_inner() {
        return Err(item);
    }
    result.map_err(|e| RetainedItem::new(path, e))?;
    sync_parent_strict(path).map_err(|e| RetainedItem::new(path, e))
}

/// `fs::remove_dir`, retried by [`retry_while`] while the failure is transient (what a Windows
/// scanner or indexer that still holds the directory looks like), under the transaction's own
/// policy; `None` makes one attempt.
fn remove_dir_with_retry(path: &Path, retry: Option<RetryPolicy>) -> io::Result<()> {
    retry_while(
        retry,
        transient_removal_error,
        || match forced_delete_failure() {
            Some(error) => Err(error),
            None => fs::remove_dir(path),
        },
    )
}

/// Run `operation`; while it fails with an error `transient` accepts, pause (doubling, capped at
/// one second or the policy's own delay, whichever is longer) and run it again, at most
/// `policy.attempts` more times. `None` makes one attempt.
fn retry_while(
    retry: Option<RetryPolicy>,
    transient: impl Fn(&io::Error) -> bool,
    mut operation: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let attempts = retry.map_or(0, |policy| policy.attempts);
    let first = retry.map_or(Duration::ZERO, |policy| policy.delay);
    let cap = first.max(Duration::from_secs(1));
    let mut delay = first;
    let mut tried = 0;
    loop {
        match operation() {
            Err(error) if tried < attempts && transient(&error) => {
                tried += 1;
                std::thread::sleep(delay);
                delay = delay.saturating_mul(2).min(cap);
            }
            other => return other,
        }
    }
}

/// Flush directory entries using Unix fsync or Windows' existing writable-directory flush.
/// A directory that is already gone has nothing left to flush.
fn sync_dir_strict(dir: &Path) -> Result<()> {
    #[cfg(windows)]
    let result = crate::atomic::flush_dir_windows(dir);
    #[cfg(not(windows))]
    let result = try_sync_dir(dir);
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other.map_err(|e| FsError::io(dir, e)),
    }
}

fn sync_parent_strict(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_dir_strict(parent)?;
    }
    Ok(())
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = try_sync_dir(parent);
    }
}

// ---------------------------------------------------------------------------------------------
// Names and paths
// ---------------------------------------------------------------------------------------------

fn fold(text: &str) -> String {
    if cfg!(any(windows, target_vendor = "apple")) {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

fn bad_name(rel: &Path) -> FsError {
    FsError::BadName(rel.display().to_string())
}

fn valid_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\0'])
        && !(cfg!(windows) && crate::names::is_unsafe_windows_component(name))
}

/// The plain components of a relative destination, or `BadName`.
fn split_relative(rel: &Path) -> Result<Vec<String>> {
    let text = rel.to_str().ok_or_else(|| bad_name(rel))?;
    if text.len() > MAX_RELATIVE_BYTES || text.contains('\0') {
        return Err(bad_name(rel));
    }
    let mut components = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| bad_name(rel))?;
                if !valid_component(name) {
                    return Err(bad_name(rel));
                }
                components.push(name.to_string());
            }
            Component::CurDir => {}
            _ => return Err(bad_name(rel)),
        }
    }
    if components.is_empty() || components.len() > MAX_COMPONENTS {
        return Err(bad_name(rel));
    }
    if fold(&components[0]) == AREA {
        return Err(bad_name(rel));
    }
    let leaf = &components[components.len() - 1];
    crate::atomic::reject_reserved_name(leaf)?;
    if leaf.contains(SIDE_MARKER) {
        return Err(bad_name(rel));
    }
    Ok(components)
}

/// The components of a journal's `/`-separated relative path, or `None`.
fn split_journal_rel(text: &str) -> Option<Vec<String>> {
    if text.is_empty() || text.len() > MAX_RELATIVE_BYTES + 256 {
        return None;
    }
    let components: Vec<String> = text.split('/').map(str::to_string).collect();
    if components.len() > MAX_COMPONENTS + 1 || !components.iter().all(|c| valid_component(c)) {
        return None;
    }
    Some(components)
}

fn join_rel(dir: &[String], name: &str) -> String {
    let mut out = String::new();
    for component in dir {
        out.push_str(component);
        out.push('/');
    }
    out.push_str(name);
    out
}

/// `root` joined with `components`, refusing a symlink or reparse point at any existing component
/// and a leaf that exists but is not a regular file. Components that do not exist yet are
/// appended unchecked (they cannot be links).
fn resolve_entry_path(root: &Path, components: &[String]) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    let mut missing = false;
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        if missing {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if is_link(&meta) {
                    return Err(FsError::Symlink(current));
                }
                if index + 1 == components.len() {
                    if !meta.is_file() {
                        return Err(FsError::NotAFile(current));
                    }
                } else if !meta.is_dir() {
                    return Err(FsError::NotADirectory(current));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => missing = true,
            Err(e) => return Err(FsError::io(&current, e)),
        }
    }
    Ok(current)
}

/// The three paths of one journal entry.
struct EntryPaths {
    dest: PathBuf,
    stage: PathBuf,
    backup: PathBuf,
}

fn entry_paths(ctx: &Ctx<'_>, entry: &JournalEntry) -> Result<EntryPaths> {
    let corrupt = |reason: &str| FsError::Corrupt {
        path: ctx.root.join(AREA).join(JOURNAL),
        reason: reason.to_string(),
    };
    let root = ctx
        .root_of(entry.root)
        .ok_or_else(|| corrupt("an entry names a root the journal does not list"))?;
    let root = root.as_path();
    let dest = split_journal_rel(&entry.dest)
        .ok_or_else(|| corrupt("an entry destination is not a plain relative path"))?;
    let stage = split_journal_rel(&entry.stage)
        .ok_or_else(|| corrupt("an entry staged name is not a plain relative path"))?;
    let backup = split_journal_rel(&entry.backup)
        .ok_or_else(|| corrupt("an entry backup name is not a plain relative path"))?;
    let dir = &dest[..dest.len() - 1];
    if stage.len() != dest.len()
        || backup.len() != dest.len()
        || stage[..stage.len() - 1] != *dir
        || backup[..backup.len() - 1] != *dir
    {
        return Err(corrupt(
            "an entry's staged and backup names are not beside its destination",
        ));
    }
    if fold(&dest[0]) == AREA {
        return Err(corrupt(
            "an entry destination is inside the transaction directory",
        ));
    }
    Ok(EntryPaths {
        dest: resolve_entry_path(root, &dest)?,
        stage: resolve_entry_path(root, &stage)?,
        backup: resolve_entry_path(root, &backup)?,
    })
}

fn bounded_prefix(name: &str) -> &str {
    if name.len() <= SIDE_PREFIX_MAX {
        return name;
    }
    let mut end = SIDE_PREFIX_MAX;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

fn side_name(leaf: &str, id: &str, index: usize, kind: char) -> String {
    format!(".{}{SIDE_MARKER}{id}-{index}{kind}", bounded_prefix(leaf))
}

fn new_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let count = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{millis:012x}-{:x}-{count:x}", std::process::id())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-'))
}

// ---------------------------------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalPhase {
    Staging,
    Staged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct JournalEntry {
    dest: String,
    stage: String,
    backup: String,
    create_new: bool,
    private: bool,
    prior: Option<Ident>,
    /// Phase `staging`: only the file key of the staged file, as a claim (length and time zero),
    /// written before the file gets its name. Phase `staged`: the full identity.
    staged: Option<Ident>,
    /// A rollback found the destination to be somebody else's file while the staged copy still
    /// existed, so this entry never published; written before the staged copy is removed so a
    /// replay after the removal reaches the same decision.
    unpublished: bool,
    /// Which root the destination is under: 0 is the coordinating root (the one that holds this
    /// journal), `n` is `Journal::others[n - 1]`.
    root: usize,
}

/// A root other than the coordinating one: where to find it and which directory it was.
#[derive(Clone, Debug, PartialEq, Eq)]
struct JournalRoot {
    path: String,
    key: FileKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Journal {
    id: String,
    root: FileKey,
    /// The other roots of a multi-root transaction; empty for the single-root format.
    others: Vec<JournalRoot>,
    phase: JournalPhase,
    entries: Vec<JournalEntry>,
    /// The CRC-32 of the rendered body; set by `render` and `parse`.
    crc: u32,
}

fn hex_encode(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(text: &str) -> Option<String> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

fn render_ident(ident: &Option<Ident>) -> String {
    ident
        .as_ref()
        .map_or_else(|| "-".to_string(), Ident::render)
}

fn parse_ident(text: &str) -> Option<Option<Ident>> {
    if text == "-" {
        Some(None)
    } else {
        Ident::parse(text).map(Some)
    }
}

impl Journal {
    /// The journal text and its CRC-32 (also stored in `self.crc` by the caller).
    fn render(&self) -> (String, u32) {
        let mut out = String::new();
        let multi = !self.others.is_empty();
        out.push_str(if multi {
            JOURNAL_MAGIC_MULTI
        } else {
            JOURNAL_MAGIC
        });
        out.push('\n');
        out.push_str(&format!("id {}\n", self.id));
        out.push_str(&format!(
            "root {:x}.{:x}\n",
            self.root.volume, self.root.index
        ));
        if multi {
            out.push_str(&format!("roots {}\n", self.others.len()));
            for other in &self.others {
                out.push_str(&format!(
                    "rootn {} {:x}.{:x}\n",
                    hex_encode(&other.path),
                    other.key.volume,
                    other.key.index
                ));
            }
        }
        out.push_str(match self.phase {
            JournalPhase::Staging => "phase staging\n",
            JournalPhase::Staged => "phase staged\n",
        });
        out.push_str(&format!("entries {}\n", self.entries.len()));
        for (index, entry) in self.entries.iter().enumerate() {
            out.push_str(&format!(
                "entry {index} {} {} {} {} {} {} {} {}{}\n",
                if entry.create_new { 'N' } else { 'R' },
                if entry.private { 1 } else { 0 },
                hex_encode(&entry.dest),
                hex_encode(&entry.stage),
                hex_encode(&entry.backup),
                render_ident(&entry.prior),
                render_ident(&entry.staged),
                if entry.unpublished { 1 } else { 0 },
                if multi {
                    format!(" {}", entry.root)
                } else {
                    String::new()
                },
            ));
        }
        let crc = crate::zip::crc32(out.as_bytes());
        out.push_str(&format!("crc {crc:08x}\n"));
        (out, crc)
    }

    fn parse(text: &str, shown: &Path) -> Result<Self> {
        let corrupt = |reason: &str| FsError::Corrupt {
            path: shown.to_path_buf(),
            reason: reason.to_string(),
        };
        let split = text
            .rfind("crc ")
            .ok_or_else(|| corrupt("the checksum line is missing"))?;
        if split != 0 && !text[..split].ends_with('\n') {
            return Err(corrupt("the checksum line is misplaced"));
        }
        let (body, tail) = text.split_at(split);
        let crc_text = tail
            .strip_prefix("crc ")
            .and_then(|rest| rest.strip_suffix('\n'))
            .ok_or_else(|| corrupt("the checksum line is malformed"))?;
        if crc_text.len() != 8 {
            return Err(corrupt("the checksum is not eight hex digits"));
        }
        let crc =
            u32::from_str_radix(crc_text, 16).map_err(|_| corrupt("the checksum is not hex"))?;
        if crate::zip::crc32(body.as_bytes()) != crc {
            return Err(corrupt("the checksum does not match the journal"));
        }
        let mut lines = body.lines();
        let multi = match lines.next() {
            Some(JOURNAL_MAGIC) => false,
            Some(JOURNAL_MAGIC_MULTI) => true,
            _ => return Err(corrupt("not a version 1 or 2 journal")),
        };
        let mut field = |name: &str| -> Result<String> {
            let line = lines
                .next()
                .ok_or_else(|| corrupt("the journal is truncated"))?;
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(' '))
                .map(str::to_string)
                .ok_or_else(|| corrupt("a journal header line is out of place"))
        };
        let id = field("id")?;
        if !valid_id(&id) {
            return Err(corrupt("the transaction id is malformed"));
        }
        let root_text = field("root")?;
        let root = {
            let mut parts = root_text.split('.');
            let volume = parts.next().and_then(|p| u64::from_str_radix(p, 16).ok());
            let index = parts.next().and_then(|p| u64::from_str_radix(p, 16).ok());
            match (volume, index, parts.next()) {
                (Some(volume), Some(index), None) => FileKey { volume, index },
                _ => return Err(corrupt("the root identity is malformed")),
            }
        };
        let mut others: Vec<JournalRoot> = Vec::new();
        if multi {
            let count: usize = field("roots")?
                .parse()
                .map_err(|_| corrupt("the root count is malformed"))?;
            if count == 0 || count > MAX_ENTRIES {
                return Err(corrupt("the root count is out of range"));
            }
            for _ in 0..count {
                let line = field("rootn")?;
                let mut parts = line.split(' ');
                let (Some(path_hex), Some(key_text), None) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    return Err(corrupt("a root line is malformed"));
                };
                let path = hex_decode(path_hex)
                    .filter(|path| !path.is_empty() && !path.contains('\0'))
                    .ok_or_else(|| corrupt("a root path is not hex of UTF-8"))?;
                let mut key_parts = key_text.split('.');
                let volume = key_parts
                    .next()
                    .and_then(|p| u64::from_str_radix(p, 16).ok());
                let index = key_parts
                    .next()
                    .and_then(|p| u64::from_str_radix(p, 16).ok());
                let key = match (volume, index, key_parts.next()) {
                    (Some(volume), Some(index), None) => FileKey { volume, index },
                    _ => return Err(corrupt("a root identity is malformed")),
                };
                others.push(JournalRoot { path, key });
            }
        }
        let phase = match field("phase")?.as_str() {
            "staging" => JournalPhase::Staging,
            "staged" => JournalPhase::Staged,
            _ => return Err(corrupt("the journal phase is unknown")),
        };
        let count: usize = field("entries")?
            .parse()
            .map_err(|_| corrupt("the entry count is malformed"))?;
        if count == 0 || count > MAX_ENTRIES {
            return Err(corrupt("the entry count is out of range"));
        }
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let line = field("entry")?;
            let tokens: Vec<&str> = line.split(' ').collect();
            if tokens.len() != (if multi { 10 } else { 9 }) || tokens[0] != index.to_string() {
                return Err(corrupt("an entry line is malformed or out of order"));
            }
            let create_new = match tokens[1] {
                "N" => true,
                "R" => false,
                _ => return Err(corrupt("an entry mode is unknown")),
            };
            let private = match tokens[2] {
                "1" => true,
                "0" => false,
                _ => return Err(corrupt("an entry privacy flag is unknown")),
            };
            let unpublished = match tokens[8] {
                "1" => true,
                "0" => false,
                _ => return Err(corrupt("an entry rollback mark is unknown")),
            };
            let entry_root = if multi {
                tokens[9]
                    .parse::<usize>()
                    .ok()
                    .filter(|root| *root <= others.len())
                    .ok_or_else(|| corrupt("an entry names a root the journal does not list"))?
            } else {
                0
            };
            let decode = |text: &str| {
                hex_decode(text).ok_or_else(|| corrupt("an entry path is not hex of UTF-8"))
            };
            let prior = parse_ident(tokens[6])
                .ok_or_else(|| corrupt("an entry prior identity is malformed"))?;
            let staged = parse_ident(tokens[7])
                .ok_or_else(|| corrupt("an entry staged identity is malformed"))?;
            if phase == JournalPhase::Staged && staged.is_none() {
                return Err(corrupt(
                    "a staged journal has an entry without a staged identity",
                ));
            }
            let dest = decode(tokens[3])?;
            let stage = decode(tokens[4])?;
            let backup = decode(tokens[5])?;
            let components = split_journal_rel(&dest)
                .ok_or_else(|| corrupt("an entry destination is not a plain relative path"))?;
            let leaf = &components[components.len() - 1];
            let dir = &components[..components.len() - 1];
            if stage != join_rel(dir, &side_name(leaf, &id, index, 's'))
                || backup != join_rel(dir, &side_name(leaf, &id, index, 'b'))
            {
                return Err(corrupt(
                    "an entry side name does not match its transaction and index",
                ));
            }
            entries.push(JournalEntry {
                dest,
                stage,
                backup,
                create_new,
                private,
                prior,
                staged,
                unpublished,
                root: entry_root,
            });
        }
        if lines.next().is_some() {
            return Err(corrupt("the journal has trailing lines"));
        }
        Ok(Self {
            id,
            root,
            others,
            phase,
            entries,
            crc,
        })
    }
}

fn load_journal(path: &Path) -> Result<Journal> {
    let file = File::open(path).map_err(|e| FsError::io(path, e))?;
    let mut text = String::new();
    file.take(MAX_JOURNAL_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| FsError::io(path, e))?;
    if text.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(FsError::Corrupt {
            path: path.to_path_buf(),
            reason: "the journal is larger than any journal this crate writes".into(),
        });
    }
    Journal::parse(&text, path)
}

fn marker_text(id: &str, journal_crc: u32) -> String {
    format!("{COMMIT_MAGIC} {id} {journal_crc:08x}\n")
}

/// Writes one small record (journal, commit marker) durably and owner-only through
/// [`AtomicWriter`], so it goes through the write and flush seams like any atomic write.
fn write_record(ctx: &Ctx<'_>, path: &Path, text: &str, replace: ReplaceMode) -> Result<()> {
    let options = AtomicWriteOptions {
        private: true,
        create_parents: false,
        durable: true,
        replace,
        retry_sharing_violation: ctx.options.retry,
        privacy: Privacy::OwnerOnly,
    };
    let mut writer = AtomicWriter::create_with(path, options)?;
    writer.write_all(text.as_bytes())?;
    // Bind ownership before publication: a replacement after commit cannot become our claim.
    let claim = read_record_claim(writer.temp_path(), text)?;
    let result = writer.commit();
    if result.is_ok() || result.as_ref().is_err_and(|error| error.target_replaced()) {
        ctx.claim_record(path, claim);
    }
    result
}

fn write_journal(
    ctx: &Ctx<'_>,
    journal: &mut Journal,
    before: Step,
    after: Step,
    replace: ReplaceMode,
) -> Result<()> {
    let path = ctx.txn_dir.join(JOURNAL);
    fire(before, &path)?;
    let (text, crc) = journal.render();
    journal.crc = crc;
    write_record(ctx, &path, &text, replace)?;
    fire(after, &path)
}

// ---------------------------------------------------------------------------------------------
// The decision table
// ---------------------------------------------------------------------------------------------

/// Where one entry stands, read from the filesystem and the recorded identities.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EntryState {
    /// The destination is as it was recorded (or, for one that did not exist, any file there is
    /// someone else's and was never published by this transaction); nothing to restore, though a
    /// staged copy may remain.
    Untouched,
    /// The destination is as recorded and the backup is a second name for it (hard link).
    LinkedBackup,
    /// The destination is gone and the backup holds the prior file (renamed aside).
    AsideBackup,
    /// The destination holds the staged content; `backup` says whether a backup of the prior file
    /// exists.
    Published { backup: bool },
    /// Something does not match what was recorded; nothing may be deleted or replaced.
    Conflict(String),
}

/// [`classify`] with the journal's mark that a rollback already found this entry unpublished
/// (`unpublished`): the staged copy it was found by may have been removed since.
fn classify_entry(
    prior: Option<&Ident>,
    staged: &Ident,
    dest: &Probe,
    stage: &Probe,
    backup: &Probe,
    unpublished: bool,
) -> EntryState {
    for (name, state) in [
        ("destination", dest),
        ("staged copy", stage),
        ("backup", backup),
    ] {
        if let Probe::Other(why) = state {
            return EntryState::Conflict(format!("the {name} is {why}"));
        }
    }
    if let Probe::File(now) = stage {
        if now != staged {
            return EntryState::Conflict(
                "the staged copy no longer matches its recorded identity".into(),
            );
        }
    }
    match (backup, prior) {
        (Probe::File(now), Some(prior)) if now != prior => {
            return EntryState::Conflict(
                "the backup no longer matches the prior file's recorded identity".into(),
            );
        }
        (Probe::File(_), None) => {
            return EntryState::Conflict(
                "a backup exists for a destination that did not exist".into(),
            );
        }
        _ => {}
    }
    let backup_present = matches!(backup, Probe::File(_));
    let dest_is_staged = matches!(dest, Probe::File(now) if now == staged);
    let dest_is_prior = match (dest, prior) {
        (Probe::File(now), Some(prior)) => now == prior,
        _ => false,
    };
    if dest_is_staged && dest_is_prior {
        return EntryState::Conflict(
            "the staged and prior identities are indistinguishable".into(),
        );
    }
    if dest_is_staged {
        return match (prior, backup_present) {
            (Some(_), false) => EntryState::Conflict(
                "the destination holds the staged content but the prior file's backup is missing"
                    .into(),
            ),
            (Some(_), true) => EntryState::Published { backup: true },
            (None, _) => EntryState::Published { backup: false },
        };
    }
    match (prior, dest) {
        (Some(_), _) if dest_is_prior => {
            if backup_present {
                EntryState::LinkedBackup
            } else {
                EntryState::Untouched
            }
        }
        (Some(_), Probe::Missing) => {
            if backup_present {
                EntryState::AsideBackup
            } else {
                EntryState::Conflict(
                    "the destination and the prior file's backup are both missing".into(),
                )
            }
        }
        // The destination is someone else's file (an editor wrote it before we backed it up),
        // the staged copy is still here with its recorded identity and no backup exists: nothing
        // of ours was published (a replace consumes the staged name, and the exclusive publish
        // after an aside backup leaves a backup), so there is nothing to restore. The only action
        // taken is removing the staged name by identity; the destination is never touched.
        (Some(_), Probe::File(_))
            if !backup_present && (matches!(stage, Probe::File(_)) || unpublished) =>
        {
            EntryState::Untouched
        }
        (Some(_), _) => EntryState::Conflict("the destination was changed by someone else".into()),
        (None, Probe::Missing) => EntryState::Untouched,
        // Someone else created the name while our staged copy was still unpublished: that file is
        // theirs, nothing of ours is in it, and rolling back must leave it alone.
        (None, Probe::File(_)) if matches!(stage, Probe::File(_)) || unpublished => {
            EntryState::Untouched
        }
        (None, _) => EntryState::Conflict(
            "a destination that did not exist was created by someone else".into(),
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// Prepare
// ---------------------------------------------------------------------------------------------

/// Namespace for [`FileTransaction::prepare`].
#[derive(Clone, Copy, Debug)]
pub struct FileTransaction;

#[derive(Debug)]
struct PlannedEntry {
    /// The `/`-joined relative destination.
    rel: String,
    components: Vec<String>,
    candidate: PathBuf,
    candidate_ident: Ident,
    /// Which root the destination is under (0: the coordinating root).
    root: usize,
    parents: Vec<(PathBuf, FileKey)>,
    expect: ExpectedPrior,
    create_new: bool,
    private: bool,
}

/// A validated set of entries, ready to commit. Preparation changed nothing on disk.
#[derive(Debug)]
pub struct PreparedTransaction {
    root: PathBuf,
    requested_root: PathBuf,
    root_key: FileKey,
    /// The other roots of a multi-root transaction, in first-use order (entry root `n` is
    /// `others[n - 1]`).
    others: Vec<PreparedRoot>,
    entries: Vec<PlannedEntry>,
    options: TransactionOptions,
}

/// A root other than the coordinating one, resolved once at preparation.
#[derive(Debug)]
struct PreparedRoot {
    requested: PathBuf,
    root: PathBuf,
    key: FileKey,
}

/// True when one path is the other or lies below it.
fn nested(a: &Path, b: &Path) -> bool {
    let (a, b): (Vec<String>, Vec<String>) = (
        a.components()
            .map(|c| fold(&c.as_os_str().to_string_lossy()))
            .collect(),
        b.components()
            .map(|c| fold(&c.as_os_str().to_string_lossy()))
            .collect(),
    );
    let shared = a.len().min(b.len());
    a[..shared] == b[..shared]
}

impl FileTransaction {
    /// Validate `entries` against `root` and return a transaction that can be committed.
    ///
    /// `root` must be an existing directory. Each destination is a bounded relative path under it
    /// (see the module docs); each candidate must be a regular file. Nothing is created, written
    /// or locked: preparation reads the destinations and candidates only. Refusals: `BadName`
    /// (escape, absolute path, `..`, reserved or non-UTF-8 name, duplicate or overlapping
    /// destinations, a destination inside `.rktxn`), `Symlink`, `NotADirectory`, `NotAFile`,
    /// `AlreadyExists` (a create-new destination exists), [`FsError::Conflict`] (the destination
    /// does not match its [`ExpectedPrior`]), `TooManyEntries`, and the usual `Io`.
    pub fn prepare(
        root: &Path,
        entries: Vec<TransactionEntry>,
        options: TransactionOptions,
    ) -> Result<PreparedTransaction> {
        let requested_root = std::path::absolute(root).map_err(|e| FsError::io(root, e))?;
        let root = canonicalize_plain(root).map_err(|e| FsError::io(root, e))?;
        let root_key = FileKey::of_path(&root)?;
        let meta = fs::metadata(&root).map_err(|e| FsError::io(&root, e))?;
        if !meta.is_dir() {
            return Err(FsError::NotADirectory(root));
        }
        if entries.is_empty() {
            return Err(FsError::io(
                &root,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a transaction needs at least one entry",
                ),
            ));
        }
        if entries.len() > MAX_ENTRIES {
            return Err(FsError::TooManyEntries(MAX_ENTRIES));
        }
        // Each explicit root is resolved once, here; an entry without one is under `root`.
        let mut others: Vec<PreparedRoot> = Vec::new();
        let mut entry_roots: Vec<usize> = Vec::with_capacity(entries.len());
        for entry in &entries {
            let Some(requested) = entry.root.as_deref() else {
                entry_roots.push(0);
                continue;
            };
            let resolved = canonicalize_plain(requested).map_err(|e| FsError::io(requested, e))?;
            let key = FileKey::of_path(&resolved)?;
            let meta = fs::metadata(&resolved).map_err(|e| FsError::io(&resolved, e))?;
            if !meta.is_dir() {
                return Err(FsError::NotADirectory(resolved));
            }
            if key == root_key {
                entry_roots.push(0);
            } else if let Some(at) = others.iter().position(|other| other.key == key) {
                entry_roots.push(at + 1);
            } else {
                others.push(PreparedRoot {
                    requested: std::path::absolute(requested)
                        .map_err(|e| FsError::io(requested, e))?,
                    root: resolved,
                    key,
                });
                entry_roots.push(others.len());
            }
        }
        // A root inside another would let two names reach one file; refuse it up front.
        let all_roots: Vec<&Path> = std::iter::once(root.as_path())
            .chain(others.iter().map(|other| other.root.as_path()))
            .collect();
        for (at, first) in all_roots.iter().enumerate() {
            for second in &all_roots[at + 1..] {
                if nested(first, second) {
                    return Err(FsError::io(
                        second,
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "the roots of one transaction may not be inside one another",
                        ),
                    ));
                }
            }
        }
        let mut planned: Vec<PlannedEntry> = Vec::with_capacity(entries.len());
        let mut folded: Vec<(usize, Vec<String>)> = Vec::with_capacity(entries.len());
        for (entry, entry_root) in entries.into_iter().zip(entry_roots) {
            let (this_root, this_key): (&Path, FileKey) = if entry_root == 0 {
                (root.as_path(), root_key)
            } else {
                (
                    others[entry_root - 1].root.as_path(),
                    others[entry_root - 1].key,
                )
            };
            let components = split_relative(&entry.destination)?;
            let key: Vec<String> = components.iter().map(|c| fold(c)).collect();
            for (other_root, other) in &folded {
                let shared = key.len().min(other.len());
                if *other_root == entry_root && key[..shared] == other[..shared] {
                    return Err(bad_name(&entry.destination));
                }
            }
            folded.push((entry_root, key));
            if entry.publication == Publication::CreateNew && entry.expect != ExpectedPrior::Absent
            {
                return Err(FsError::io(
                    &entry.destination,
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a create-new entry can only expect an absent destination",
                    ),
                ));
            }
            let candidate_ident = match probe(&entry.candidate)? {
                Probe::File(ident) => ident,
                Probe::Missing => {
                    return Err(FsError::io(
                        &entry.candidate,
                        io::Error::from(io::ErrorKind::NotFound),
                    ))
                }
                Probe::Other(_) => return Err(FsError::NotAFile(entry.candidate)),
            };
            let mut parents = vec![(this_root.to_path_buf(), this_key)];
            let mut parent = this_root.to_path_buf();
            for component in components.iter().take(components.len() - 1) {
                parent.push(component);
                match fs::symlink_metadata(&parent) {
                    Ok(meta) if is_link(&meta) => return Err(FsError::Symlink(parent)),
                    Ok(meta) if meta.is_dir() => {
                        parents.push((parent.clone(), FileKey::of_path(&parent)?));
                    }
                    Ok(_) => return Err(FsError::NotADirectory(parent)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                    Err(error) => return Err(FsError::io(&parent, error)),
                }
            }
            let planned_entry = PlannedEntry {
                parents,
                rel: components.join("/"),
                components,
                candidate: entry.candidate,
                candidate_ident,
                root: entry_root,
                expect: entry.expect,
                create_new: entry.publication == Publication::CreateNew,
                private: entry.private,
            };
            let dest = resolve_entry_path(this_root, &planned_entry.components)?;
            check_expectation(&dest, &planned_entry)?;
            planned.push(planned_entry);
        }
        Ok(PreparedTransaction {
            root,
            requested_root,
            root_key,
            others,
            entries: planned,
            options,
        })
    }
}

/// Verify `dest` against the entry's expectation now; the identity found (`None`: absent) is what
/// the transaction records as the prior state.
fn check_expectation(dest: &Path, entry: &PlannedEntry) -> Result<Option<Ident>> {
    let now = probe(dest)?;
    let ident = match now {
        Probe::Other(_) => return Err(FsError::NotAFile(dest.to_path_buf())),
        Probe::Missing => None,
        Probe::File(ident) => Some(ident),
    };
    if entry.create_new && ident.is_some() {
        return Err(FsError::AlreadyExists(dest.to_path_buf()));
    }
    match (&entry.expect, ident) {
        (ExpectedPrior::Any, found) => Ok(found),
        (ExpectedPrior::Absent, None) => Ok(None),
        (ExpectedPrior::Absent, Some(_)) => Err(conflict(
            dest,
            "the destination exists but was expected to be absent",
        )),
        (ExpectedPrior::Identity(_), None) => Err(conflict(
            dest,
            "the destination is gone but was expected to exist",
        )),
        (ExpectedPrior::Identity(key), Some(found)) => {
            if found.key == *key {
                Ok(Some(found))
            } else {
                Err(conflict(
                    dest,
                    "the destination is not the file that was expected (its identity differs)",
                ))
            }
        }
        #[cfg(feature = "digest")]
        (ExpectedPrior::Sha256(_), None) => Err(conflict(
            dest,
            "the destination is gone but was expected to exist",
        )),
        #[cfg(feature = "digest")]
        (ExpectedPrior::Sha256(expected), Some(found)) => {
            let actual = crate::digest::sha256_file(dest).map_err(|e| FsError::io(dest, e))?;
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(conflict(
                    dest,
                    "the destination's content hash is not the expected one",
                ));
            }
            // The hash is only evidence for the file that was hashed.
            match probe(dest)? {
                Probe::File(again) if again == found => Ok(Some(found)),
                _ => Err(conflict(
                    dest,
                    "the destination changed while it was being hashed",
                )),
            }
        }
    }
}

fn verify_parent_identities(parents: &[(PathBuf, FileKey)]) -> Result<()> {
    for (path, expected) in parents {
        let meta = fs::symlink_metadata(path).map_err(|e| FsError::io(path, e))?;
        if is_link(&meta) || !meta.is_dir() || FileKey::of_path(path)? != *expected {
            return Err(conflict(
                path,
                "a directory recorded during preparation was substituted",
            ));
        }
    }
    Ok(())
}

impl PreparedTransaction {
    fn verify_prepared_directories(&self) -> Result<()> {
        let resolved = canonicalize_plain(&self.requested_root)
            .map_err(|e| FsError::io(&self.requested_root, e))?;
        if resolved != self.root || FileKey::of_path(&resolved)? != self.root_key {
            return Err(conflict(
                &self.requested_root,
                "the prepared root was substituted",
            ));
        }
        for other in &self.others {
            let resolved = canonicalize_plain(&other.requested)
                .map_err(|e| FsError::io(&other.requested, e))?;
            if resolved != other.root || FileKey::of_path(&resolved)? != other.key {
                return Err(conflict(
                    &other.requested,
                    "a prepared root was substituted",
                ));
            }
        }
        for entry in &self.entries {
            verify_parent_identities(&entry.parents)?;
        }
        Ok(())
    }

    fn root_path(&self, index: usize) -> &Path {
        if index == 0 {
            &self.root
        } else {
            &self.others[index - 1].root
        }
    }

    /// The canonical root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The absolute destination paths, in entry order.
    pub fn destinations(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .map(|entry| {
                let mut path = self.root_path(entry.root).to_path_buf();
                path.extend(&entry.components);
                path
            })
            .collect()
    }

    /// Commit the transaction. See the module docs for the protocol and [`TransactionReport`] for
    /// the outcomes. `Err` means nothing was changed.
    pub fn commit(self) -> Result<TransactionReport> {
        self.commit_inner(None)
    }

    /// [`commit`](Self::commit), running `after` once every file is published and **before** the
    /// commit marker is written, while the outcome can still be undone. If `after` returns an error
    /// the whole transaction is rolled back (every destination restored and verified) and the
    /// report is [`TransactionReport::RolledBack`] with `after`'s error as its cause (or
    /// [`TransactionReport::RecoveryRequired`] if the rollback itself could not finish). If `after`
    /// succeeds the marker is written as in `commit`. A crash inside `after`, or between it and the
    /// marker, leaves no marker, so [`recover_transactions`] rolls the files back: the caller's own
    /// record (the row `after` writes) and the files are then either both new or both old, provided
    /// `after`'s effect is itself one atomic step that recovery of the caller undoes or never
    /// started. `after` runs with the operation lock held: it must not call this crate's
    /// transaction functions for the same root.
    pub fn commit_then(self, after: impl FnOnce() -> Result<()>) -> Result<TransactionReport> {
        let mut after = Some(after);
        let mut call = move || match after.take() {
            Some(f) => f(),
            None => Ok(()),
        };
        self.commit_inner(Some(&mut call))
    }

    fn commit_inner(
        self,
        after: Option<&mut dyn FnMut() -> Result<()>>,
    ) -> Result<TransactionReport> {
        self.verify_prepared_directories()?;
        let area = self.root.join(AREA);
        create_dir_all_durable(&area, true)?;
        let _lock = FileLock::acquire(&area.join(LOCK_NAME), &lock_options(&self.options))?;
        if let Some(pending) = first_pending(&area)? {
            return Err(FsError::Locked {
                path: area.join(&pending),
                owner: format!(
                    "an earlier transaction ({pending}) awaits recovery; call recover_transactions"
                ),
            });
        }
        self.verify_prepared_directories()?;
        let root_key = self.root_key;
        let id = new_id();
        let mut entries = Vec::with_capacity(self.entries.len());
        for (index, planned) in self.entries.iter().enumerate() {
            entries.push(plan_entry(
                self.root_path(planned.root),
                &id,
                index,
                planned,
            )?);
        }
        let mut others = Vec::with_capacity(self.others.len());
        for other in &self.others {
            // The journal holds the path as UTF-8 text so recovery can find the root again.
            let path = other
                .root
                .to_str()
                .ok_or_else(|| FsError::BadName(other.root.display().to_string()))?;
            others.push(JournalRoot {
                path: path.to_string(),
                key: other.key,
            });
        }
        let mut journal = Journal {
            id: id.clone(),
            root: root_key,
            others,
            phase: JournalPhase::Staging,
            entries,
            crc: 0,
        };
        let txn_dir = area.join(&id);
        if let Err(error) = create_dir_all_durable(&txn_dir, true) {
            let _ = fs::remove_dir(&txn_dir);
            return Err(error);
        }
        let ctx = Ctx {
            records: std::cell::RefCell::new(Vec::new()),
            root: &self.root,
            others: std::cell::RefCell::new(
                self.others.iter().map(|other| other.root.clone()).collect(),
            ),
            options: &self.options,
            txn_dir: &txn_dir,
        };
        run_commit(&ctx, &self.entries, &mut journal, after)
    }
}

fn lock_options(options: &TransactionOptions) -> LockOptions {
    LockOptions::wait(options.lock_timeout).labeled("rightkit-fs transaction")
}

/// The first transaction directory under `.rktxn` (ids sort oldest first), if any.
fn first_pending(area: &Path) -> Result<Option<String>> {
    let mut pending: Vec<String> = Vec::new();
    for entry in fs::read_dir(area).map_err(|e| FsError::io(area, e))? {
        let entry = entry.map_err(|e| FsError::io(area, e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| FsError::io(entry.path(), e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() && name != RETAINED_DIR {
            pending.push(name);
        }
    }
    pending.sort();
    Ok(pending.into_iter().next())
}

/// Re-verify one entry under the lock and build its journal record.
fn plan_entry(root: &Path, id: &str, index: usize, planned: &PlannedEntry) -> Result<JournalEntry> {
    let dest = resolve_entry_path(root, &planned.components)?;
    let prior = check_expectation(&dest, planned)?;
    match probe(&planned.candidate)? {
        Probe::File(now) if now == planned.candidate_ident => {}
        Probe::File(_) => {
            return Err(conflict(
                &planned.candidate,
                "the candidate file changed after the transaction was prepared",
            ))
        }
        Probe::Missing => return Err(conflict(&planned.candidate, "the candidate file is gone")),
        Probe::Other(_) => return Err(FsError::NotAFile(planned.candidate.clone())),
    }
    let (dir, leaf) = planned.components.split_at(planned.components.len() - 1);
    let leaf = &leaf[0];
    Ok(JournalEntry {
        dest: planned.rel.clone(),
        stage: join_rel(dir, &side_name(leaf, id, index, 's')),
        backup: join_rel(dir, &side_name(leaf, id, index, 'b')),
        create_new: planned.create_new,
        private: planned.private,
        prior,
        staged: None,
        unpublished: false,
        root: planned.root,
    })
}

// ---------------------------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct RecordClaim {
    ident: Ident,
    text: String,
}

fn read_record_claim(path: &Path, expected: &str) -> Result<RecordClaim> {
    if !matches!(probe(path)?, Probe::File(_)) {
        return Err(conflict(path, "the protocol record is not a regular file"));
    }
    let mut file = File::open(path).map_err(|e| FsError::io(path, e))?;
    let before = Ident::of_file(&file, path)?;
    let mut text = String::new();
    (&mut file)
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| FsError::io(path, e))?;
    if text != expected || Ident::of_file(&file, path)? != before {
        return Err(conflict(
            path,
            "the protocol record's content changed or is unrecognized",
        ));
    }
    Ok(RecordClaim {
        ident: before,
        text,
    })
}

struct Ctx<'a> {
    records: std::cell::RefCell<Vec<(String, RecordClaim)>>,
    /// The coordinating root: it holds the journal, the commit marker and the operation lock.
    root: &'a Path,
    /// The other roots, resolved (at commit from the prepared roots, at recovery from the journal
    /// after each was verified against its recorded identity). Entry root `n` is `others[n - 1]`.
    others: std::cell::RefCell<Vec<PathBuf>>,
    options: &'a TransactionOptions,
    txn_dir: &'a Path,
}

impl Ctx<'_> {
    /// The root directory entry root `index` stands for, if the journal lists it.
    fn root_of(&self, index: usize) -> Option<PathBuf> {
        if index == 0 {
            Some(self.root.to_path_buf())
        } else {
            self.others.borrow().get(index - 1).cloned()
        }
    }

    /// `entry`'s destination as a path to show in a report.
    fn shown(&self, entry: &JournalEntry) -> PathBuf {
        self.root_of(entry.root)
            .unwrap_or_else(|| self.root.to_path_buf())
            .join(&entry.dest)
    }

    fn claim_record(&self, path: &Path, claim: RecordClaim) {
        let name = path
            .file_name()
            .expect("a protocol record has a name")
            .to_string_lossy()
            .into_owned();
        let mut records = self.records.borrow_mut();
        if let Some(record) = records.iter_mut().find(|record| record.0 == name) {
            record.1 = claim;
        } else {
            records.push((name, claim));
        }
    }

    fn record_claim(&self, name: &str) -> Option<RecordClaim> {
        self.records
            .borrow()
            .iter()
            .find(|record| record.0 == name)
            .map(|record| record.1.clone())
    }
}

fn run_commit(
    ctx: &Ctx<'_>,
    planned: &[PlannedEntry],
    journal: &mut Journal,
    after: Option<&mut dyn FnMut() -> Result<()>>,
) -> Result<TransactionReport> {
    // Phases 3 to 5: journal, staged copies, staged journal. No live destination is touched.
    if let Err(cause) = begin_staging(ctx, planned, journal) {
        return abandon(ctx, journal, cause);
    }
    // Phase 6: publish, entry by entry.
    for (index, planned_entry) in planned.iter().enumerate() {
        if let Err(cause) = verify_parent_identities(&planned_entry.parents)
            .and_then(|()| apply_entry(ctx, index, &journal.entries[index]))
        {
            return Ok(roll_back_here(ctx, journal, cause));
        }
    }
    // Phase 7: the single decision.
    let marker_path = ctx.txn_dir.join(COMMIT);
    if let Err(cause) = fire(Step::BeforeCommitMarker, &marker_path) {
        // The marker was never attempted, so rollback is the only consistent outcome.
        return Ok(roll_back_here(ctx, journal, cause));
    }
    // The caller's step that must succeed together with the files: still undoable here.
    if let Some(after) = after {
        if let Err(cause) = after() {
            return Ok(roll_back_here(ctx, journal, cause));
        }
    }
    let marker = marker_text(&journal.id, journal.crc);
    if let Err(cause) = write_record(ctx, &marker_path, &marker, ReplaceMode::NoReplace) {
        // The marker may or may not exist (the rename can succeed and the directory sync fail).
        // Deciding here could contradict what a restart reads, so leave the decision to
        // `recover_transactions`: the marker is present (committed) or absent (rolled back), and
        // every file state is consistent with either.
        return Ok(TransactionReport::RecoveryRequired(
            RecoveryRequiredReport {
                id: journal.id.clone(),
                cause,
                retained: vec![RetainedItem::new(
                    &marker_path,
                    "the commit marker could not be confirmed; recover_transactions decides",
                )],
                journal: ctx.txn_dir.join(JOURNAL),
            },
        ));
    }
    // Phase 8: committed from here on; failures are leftovers, never a different outcome.
    let published: Vec<PathBuf> = journal
        .entries
        .iter()
        .filter_map(|entry| entry_paths(ctx, entry).ok().map(|paths| paths.dest))
        .collect();
    let retained = match fire(Step::AfterCommitMarker, &marker_path) {
        Ok(()) => complete_committed(ctx, journal).into_items(),
        Err(error) => vec![RetainedItem::new(ctx.txn_dir, error)],
    };
    Ok(TransactionReport::Committed(CommittedReport {
        id: journal.id.clone(),
        published,
        retained,
    }))
}

fn begin_staging(ctx: &Ctx<'_>, planned: &[PlannedEntry], journal: &mut Journal) -> Result<()> {
    write_journal(
        ctx,
        journal,
        Step::BeforeJournalStaging,
        Step::AfterJournalStaging,
        ReplaceMode::NoReplace,
    )?;
    for (index, planned_entry) in planned.iter().enumerate() {
        let staged = stage_entry(ctx, index, planned_entry, journal)?;
        journal.entries[index].staged = Some(staged);
    }
    journal.phase = JournalPhase::Staged;
    write_journal(
        ctx,
        journal,
        Step::BeforeJournalStaged,
        Step::AfterJournalStaged,
        ReplaceMode::MoveWriteThrough,
    )
}

/// Nothing live was touched: remove what the transaction created. `Err(cause)` when that left
/// nothing behind, a recovery report when something could not be removed.
fn abandon(ctx: &Ctx<'_>, journal: &Journal, cause: FsError) -> Result<TransactionReport> {
    let retained = discard(ctx, journal);
    if retained.is_empty() {
        Err(cause)
    } else {
        Ok(TransactionReport::RecoveryRequired(
            RecoveryRequiredReport {
                id: journal.id.clone(),
                cause,
                retained,
                journal: ctx.txn_dir.join(JOURNAL),
            },
        ))
    }
}

fn roll_back_here(ctx: &Ctx<'_>, journal: &mut Journal, cause: FsError) -> TransactionReport {
    let outcome = rollback(ctx, journal);
    if outcome.retained.is_empty() {
        TransactionReport::RolledBack(RolledBackReport {
            id: journal.id.clone(),
            cause,
            restored: outcome.restored,
        })
    } else {
        TransactionReport::RecoveryRequired(RecoveryRequiredReport {
            id: journal.id.clone(),
            cause,
            retained: outcome.retained,
            journal: ctx.txn_dir.join(JOURNAL),
        })
    }
}

fn stage_entry(
    ctx: &Ctx<'_>,
    index: usize,
    planned: &PlannedEntry,
    journal: &mut Journal,
) -> Result<Ident> {
    let entry = journal.entries[index].clone();
    let entry = &entry;
    let paths = entry_paths(ctx, entry)?;
    fire(Step::BeforeStage(index), &paths.stage)?;
    verify_parent_identities(&planned.parents)?;
    if let Some(parent) = paths.dest.parent() {
        match fs::symlink_metadata(parent) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                create_dir_all_durable(parent, entry.private)?;
            }
            Err(e) => return Err(FsError::io(parent, e)),
        }
    }
    // The parents may have just been created: resolve again, so a link planted in the gap is seen.
    let paths = entry_paths(ctx, entry)?;
    let staged = {
        // The staged file exists (under its writer's temporary name) before it has its staged
        // name: journal its file key now, so nothing can ever hold the staged name without a
        // record that lets discard and recovery tell it from somebody else's file.
        let mut claim = |key: FileKey| -> Result<()> {
            journal.entries[index].staged = Some(Ident {
                key,
                len: 0,
                modified_ns: 0,
            });
            write_journal(
                ctx,
                journal,
                Step::BeforeStageClaim(index),
                Step::AfterStageClaim(index),
                ReplaceMode::MoveWriteThrough,
            )
        };
        stage_copy(ctx, planned, entry, &paths, &mut claim)?
    };
    fire(Step::AfterStage(index), &paths.stage)?;
    Ok(staged)
}

fn stage_copy(
    ctx: &Ctx<'_>,
    planned: &PlannedEntry,
    entry: &JournalEntry,
    paths: &EntryPaths,
    claim: &mut dyn FnMut(FileKey) -> Result<()>,
) -> Result<Ident> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let mut source =
        File::open(&planned.candidate).map_err(|e| FsError::io(&planned.candidate, e))?;
    let before = Ident::of_file(&source, &planned.candidate)?;
    if before != planned.candidate_ident {
        return Err(conflict(
            &planned.candidate,
            "the candidate file changed after the transaction was prepared",
        ));
    }
    let options = AtomicWriteOptions {
        private: entry.private,
        create_parents: false,
        durable: true,
        replace: ReplaceMode::NoReplace,
        retry_sharing_violation: ctx.options.retry,
        privacy: if entry.private {
            Privacy::OwnerOnly
        } else {
            Privacy::Inherit
        },
    };
    let mut writer = AtomicWriter::create_with(&paths.stage, options)?;
    let claimed = FileKey::of_file(writer.file_mut(), &paths.stage)?;
    claim(claimed)?;
    let mut buffer = vec![0u8; COPY_BUFFER];
    let mut copied = 0u64;
    loop {
        let read = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(FsError::io(&planned.candidate, e)),
        };
        writer.write_all(&buffer[..read])?;
        copied += read as u64;
    }
    #[cfg(unix)]
    if !entry.private {
        // Never wider than the file it replaces; a new file keeps the candidate's bits.
        let candidate_mode = source
            .metadata()
            .map(|meta| meta.permissions().mode())
            .unwrap_or(0o644);
        let prior_or_candidate = if entry.prior.is_some() {
            fs::symlink_metadata(&paths.dest)
                .map(|meta| meta.permissions().mode())
                .unwrap_or(candidate_mode)
        } else {
            candidate_mode
        };
        let mode = prior_or_candidate & 0o7777;
        let temp = writer.temp_path().to_path_buf();
        writer
            .file_mut()
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|e| FsError::io(&temp, e))?;
    }
    let after = Ident::of_file(&source, &planned.candidate)?;
    if after != before || copied != before.len {
        return Err(conflict(
            &planned.candidate,
            "the candidate file changed while it was being staged",
        ));
    }
    writer.commit()?;
    match probe(&paths.stage)? {
        Probe::File(ident) if ident.len == copied && ident.key == claimed => Ok(ident),
        _ => Err(conflict(
            &paths.stage,
            "the staged copy is not what was written",
        )),
    }
}

enum BackupKind {
    Link,
    Aside,
}

fn link_unsupported(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        return matches!(
            code,
            libc::EPERM | libc::ENOTSUP | libc::ENOSYS | libc::EINVAL | libc::EMLINK
        );
    }
    #[cfg(windows)]
    if let Some(code) = error.raw_os_error() {
        // ERROR_INVALID_FUNCTION and ERROR_NOT_SUPPORTED: a volume without hard links.
        return matches!(code, 1 | 50);
    }
    false
}

/// Publish exclusively with write-through on Windows and strict source/destination parent flushes.
fn rename_durable_exclusive(from: &Path, to: &Path) -> Result<()> {
    publish_file_with(
        from,
        to,
        AtomicWriteOptions {
            durable: true,
            create_parents: false,
            replace: ReplaceMode::NoReplace,
            ..AtomicWriteOptions::default()
        },
    )?;
    sync_parent_strict(from)?;
    sync_parent_strict(to)
}

/// Keep the prior file under a hard link, or a durable exclusive rename aside.
fn make_backup(dest: &Path, backup: &Path) -> Result<BackupKind> {
    if aside_forced() {
        rename_durable_exclusive(dest, backup)?;
        return Ok(BackupKind::Aside);
    }
    match fs::hard_link(dest, backup) {
        Ok(()) => Ok(BackupKind::Link),
        Err(e) if link_unsupported(&e) => {
            rename_durable_exclusive(dest, backup)?;
            Ok(BackupKind::Aside)
        }
        Err(e) => Err(FsError::io(backup, e)),
    }
}

/// The backup just made must be the file that was recorded. When it is not, a hard link shares the
/// changed inode with the destination: the link (our own new name, and only that) is removed so no
/// second name of somebody's edited file is left behind, and the commit stops before it replaces
/// anything. A backup made by rename is renamed back to the destination name by an exclusive
/// rename (it cannot replace a file that has appeared there meanwhile); when that fails the file
/// stays under the backup name and rollback retains it for a person.
fn verify_backup(paths: &EntryPaths, prior: &Ident, kind: &BackupKind) -> Result<()> {
    let backup = probe(&paths.backup)?;
    if matches!(&backup, Probe::File(now) if now == prior) {
        return Ok(());
    }
    if let (BackupKind::Link, Probe::File(linked)) = (kind, &backup) {
        if matches!(probe(&paths.dest), Ok(Probe::File(now)) if now == *linked) {
            let _ = remove_if_exists(&paths.backup);
            sync_parent(&paths.backup);
        }
    }
    if let (BackupKind::Aside, Probe::File(_)) = (kind, &backup) {
        // Our own rename moved the edited file; put its name back. Exclusive: if anything has
        // been created at the destination meanwhile the edited file stays where it is.
        if rename_durable_exclusive(&paths.backup, &paths.dest).is_ok() {
            sync_parent(&paths.dest);
        }
    }
    Err(conflict(
        &paths.dest,
        "the destination changed while its backup was being made",
    ))
}

fn publish_step(ctx: &Ctx<'_>, from: &Path, to: &Path, replace: ReplaceMode) -> Result<()> {
    let options = AtomicWriteOptions {
        private: false,
        create_parents: false,
        durable: true,
        replace,
        retry_sharing_violation: ctx.options.retry,
        privacy: Privacy::Inherit,
    };
    match ctx.options.access_denied {
        Some(retry) => publish_file_with_access_denied_retry(from, to, options, retry),
        None => publish_file_with(from, to, options),
    }
}

fn apply_entry(ctx: &Ctx<'_>, index: usize, entry: &JournalEntry) -> Result<()> {
    let paths = entry_paths(ctx, entry)?;
    let staged = entry.staged.ok_or_else(|| FsError::Corrupt {
        path: ctx.txn_dir.join(JOURNAL),
        reason: "an entry has no staged identity".into(),
    })?;
    let mut replace = ReplaceMode::NoReplace;
    match &entry.prior {
        Some(prior) => {
            if entry.create_new {
                return Err(FsError::AlreadyExists(paths.dest));
            }
            fire(Step::BeforeBackup(index), &paths.dest)?;
            match probe(&paths.dest)? {
                Probe::File(now) if now == *prior => {}
                _ => {
                    return Err(conflict(
                        &paths.dest,
                        "the destination changed after the transaction recorded it",
                    ))
                }
            }
            fire(Step::BeforeLink(index), &paths.dest)?;
            let kind = make_backup(&paths.dest, &paths.backup)?;
            // An in-place writer can change the file between the check above and the link. The
            // backup must still be the prior file, or publishing would replace content nobody
            // recorded.
            verify_backup(&paths, prior, &kind)?;
            if let BackupKind::Link = kind {
                // The link must preserve both the prior bytes and its directory entry before
                // publication can destroy the destination's original name. Windows needs write
                // access for FlushFileBuffers, matching atomic publication's source flush.
                let mut open = fs::OpenOptions::new();
                open.read(true);
                #[cfg(windows)]
                open.write(true);
                let backup = open
                    .open(&paths.backup)
                    .map_err(|e| FsError::io(&paths.backup, e))?;
                if Ident::of_file(&backup, &paths.backup)? != *prior {
                    return Err(conflict(
                        &paths.backup,
                        "the opened backup changed before its flush",
                    ));
                }
                crate::atomic::durable_sync_with(&backup, crate::atomic::SyncPolicy::Fallback)
                    .map_err(|e| FsError::io(&paths.backup, e))?;
                sync_parent_strict(&paths.backup)?;
                // Recheck the name and identity after the barriers, before authorizing overwrite.
                verify_backup(&paths, prior, &kind)?;
                // The destination still has its name: replace it atomically, readers never see it
                // absent.
                replace = ReplaceMode::MoveWriteThrough;
            }
            fire(Step::AfterBackup(index), &paths.backup)?;
        }
        None => {
            if probe(&paths.dest)? != Probe::Missing {
                return Err(FsError::AlreadyExists(paths.dest));
            }
        }
    }
    fire(Step::BeforePublish(index), &paths.dest)?;
    match probe(&paths.stage)? {
        Probe::File(now) if now == staged => {}
        _ => {
            return Err(conflict(
                &paths.stage,
                "the staged copy changed before it was published",
            ))
        }
    }
    publish_step(ctx, &paths.stage, &paths.dest, replace)?;
    fire(Step::AfterPublish(index), &paths.dest)
}

// ---------------------------------------------------------------------------------------------
// Rollback, cleanup, discard
// ---------------------------------------------------------------------------------------------

struct RollbackOutcome {
    restored: Vec<PathBuf>,
    retained: Vec<RetainedItem>,
}

/// Remove the file at `path` if it still has the identity `expect`; a different file is retained.
fn remove_matching(
    ctx: &Ctx<'_>,
    path: &Path,
    expect: &Ident,
) -> std::result::Result<(), RetainedItem> {
    remove_while_identical(
        path,
        |now| now == expect,
        "it no longer matches the identity the transaction recorded, so it was not removed",
        ctx.options.retry,
    )
}

/// Undo a transaction that has no commit marker: restore every entry in reverse order, then remove
/// the journal and the directory when nothing was retained. Idempotent: it reads progress from the
/// filesystem, so a crash at any point of a rollback is resumed by running it again.
fn rollback(ctx: &Ctx<'_>, journal: &mut Journal) -> RollbackOutcome {
    let mut outcome = RollbackOutcome {
        restored: Vec::new(),
        retained: Vec::new(),
    };
    for index in (0..journal.entries.len()).rev() {
        let entry = journal.entries[index].clone();
        match rollback_entry(ctx, index, &entry, journal) {
            Ok(Some(restored)) => outcome.restored.push(restored),
            Ok(None) => {}
            Err(item) => outcome.retained.push(item),
        }
    }
    outcome.restored.reverse();
    if outcome.retained.is_empty() {
        // Retry the durability barrier even when a previous recovery already removed/restored
        // every file. A missing name is not evidence that its unlink reached stable storage.
        for entry in &journal.entries {
            if let Err(error) = sync_parent_strict(&ctx.shown(entry)) {
                outcome
                    .retained
                    .push(RetainedItem::new(ctx.shown(entry), error));
            }
        }
    }
    if outcome.retained.is_empty() {
        outcome.retained.extend(finish_dir(ctx));
    }
    outcome
}

/// Write the journal mark that entry `index` never published, before its staged copy (the other
/// evidence of that) is removed.
fn mark_unpublished(ctx: &Ctx<'_>, index: usize, journal: &mut Journal) -> Result<()> {
    journal.entries[index].unpublished = true;
    write_journal(
        ctx,
        journal,
        Step::BeforeRollbackMark(index),
        Step::AfterRollbackMark(index),
        ReplaceMode::MoveWriteThrough,
    )
}

fn rollback_entry(
    ctx: &Ctx<'_>,
    index: usize,
    entry: &JournalEntry,
    journal: &mut Journal,
) -> std::result::Result<Option<PathBuf>, RetainedItem> {
    let shown = ctx.shown(entry);
    let paths = entry_paths(ctx, entry).map_err(|e| RetainedItem::new(&shown, e))?;
    let staged = entry
        .staged
        .ok_or_else(|| RetainedItem::new(&paths.dest, "the entry has no staged identity"))?;
    let dest = probe(&paths.dest).map_err(|e| RetainedItem::new(&paths.dest, e))?;
    let stage = probe(&paths.stage).map_err(|e| RetainedItem::new(&paths.stage, e))?;
    let backup = probe(&paths.backup).map_err(|e| RetainedItem::new(&paths.backup, e))?;
    let state = classify_entry(
        entry.prior.as_ref(),
        &staged,
        &dest,
        &stage,
        &backup,
        entry.unpublished,
    );
    let step_error = |path: &Path, error: FsError| RetainedItem::new(path, error);
    // The destination is somebody else's file and this entry never published: the staged copy is
    // what proves that, and it is about to be removed. Make the decision durable first, or a kill
    // before the journal goes would replay as a conflict that locks the root.
    let external = match &dest {
        Probe::File(now) => entry.prior.as_ref() != Some(now),
        _ => false,
    };
    if matches!(state, EntryState::Untouched) && external && !entry.unpublished {
        mark_unpublished(ctx, index, journal).map_err(|e| step_error(&paths.dest, e))?;
    }
    let restored = match state {
        EntryState::Conflict(reason) => return Err(RetainedItem::new(&paths.dest, reason)),
        EntryState::Untouched => None,
        EntryState::LinkedBackup => {
            let prior = entry.prior.as_ref().ok_or_else(|| {
                RetainedItem::new(&paths.backup, "a linked backup without a prior identity")
            })?;
            fire(Step::BeforeRestore(index), &paths.backup)
                .map_err(|e| step_error(&paths.backup, e))?;
            remove_matching(ctx, &paths.backup, prior)?;
            fire(Step::AfterRestore(index), &paths.backup)
                .map_err(|e| step_error(&paths.backup, e))?;
            None
        }
        EntryState::AsideBackup => {
            fire(Step::BeforeRestore(index), &paths.dest)
                .map_err(|e| step_error(&paths.dest, e))?;
            publish_step(ctx, &paths.backup, &paths.dest, ReplaceMode::NoReplace)
                .map_err(|e| step_error(&paths.dest, e))?;
            sync_parent_strict(&paths.dest).map_err(|e| step_error(&paths.dest, e))?;
            fire(Step::AfterRestore(index), &paths.dest).map_err(|e| step_error(&paths.dest, e))?;
            Some(paths.dest.clone())
        }
        EntryState::Published { backup: true } => {
            fire(Step::BeforeRestore(index), &paths.dest)
                .map_err(|e| step_error(&paths.dest, e))?;
            // Our own published copy is replaced by the prior file in one rename.
            publish_step(
                ctx,
                &paths.backup,
                &paths.dest,
                ReplaceMode::MoveWriteThrough,
            )
            .map_err(|e| step_error(&paths.dest, e))?;
            fire(Step::AfterRestore(index), &paths.dest).map_err(|e| step_error(&paths.dest, e))?;
            Some(paths.dest.clone())
        }
        EntryState::Published { backup: false } => {
            fire(Step::BeforeRestore(index), &paths.dest)
                .map_err(|e| step_error(&paths.dest, e))?;
            // The destination did not exist before: remove our published copy, by identity.
            remove_matching(ctx, &paths.dest, &staged)?;
            fire(Step::AfterRestore(index), &paths.dest).map_err(|e| step_error(&paths.dest, e))?;
            Some(paths.dest.clone())
        }
    };
    // A staged name that remains (never published, or the second name of a link fallback).
    remove_matching(ctx, &paths.stage, &staged)?;
    Ok(restored)
}

/// What finishing a committed transaction left: `blocking` items keep the journal so a later
/// recovery retries; `set_aside` items were moved to `.rktxn/retained/<id>/` and block nothing.
struct Cleanup {
    blocking: Vec<RetainedItem>,
    set_aside: Vec<RetainedItem>,
}

impl Cleanup {
    /// File one step's outcome: a moved item, nothing to report, or a blocking failure.
    fn note(&mut self, outcome: std::result::Result<Option<RetainedItem>, RetainedItem>) {
        match outcome {
            Ok(Some(moved)) => self.set_aside.push(moved),
            Ok(None) => {}
            Err(item) => self.blocking.push(item),
        }
    }

    /// Everything to tell the caller, blocking items first.
    fn into_items(self) -> Vec<RetainedItem> {
        let mut items = self.blocking;
        items.extend(self.set_aside);
        items
    }
}

/// Finish a committed transaction: remove each backup (and any leftover staged name) that still
/// matches its recorded identity; move one that does not into `.rktxn/retained/<id>/` (the commit
/// is decided, so a leftover must not wedge the root, and it is never deleted); then remove the
/// journal, the marker and the directory. A removal that fails, or a move that fails, is retained
/// and reported, and the journal stays so a later recovery retries.
fn complete_committed(ctx: &Ctx<'_>, journal: &Journal) -> Cleanup {
    let mut cleanup = Cleanup {
        blocking: Vec::new(),
        set_aside: Vec::new(),
    };
    for (index, entry) in journal.entries.iter().enumerate() {
        let shown = ctx.shown(entry);
        let paths = match entry_paths(ctx, entry) {
            Ok(paths) => paths,
            Err(error) => {
                cleanup.note(Err(RetainedItem::new(&shown, error)));
                continue;
            }
        };
        if let Some(prior) = &entry.prior {
            let attempt = fire(Step::BeforeCleanup(index), &paths.backup)
                .map_err(|e| RetainedItem::new(&paths.backup, e))
                .and_then(|()| finish_leftover(ctx, &paths.backup, prior, entry.root))
                .and_then(|moved| {
                    fire(Step::AfterCleanup(index), &paths.backup)
                        .map_err(|e| RetainedItem::new(&paths.backup, e))
                        .map(|()| moved)
                });
            cleanup.note(attempt);
        }
        if let Some(staged) = &entry.staged {
            cleanup.note(finish_leftover(ctx, &paths.stage, staged, entry.root));
        }
    }
    if cleanup.blocking.is_empty() {
        cleanup.blocking.extend(finish_dir(ctx));
    }
    cleanup
}

/// A committed transaction's leftover at `path`: removed when it still has the recorded identity
/// `expect`, moved aside (`Some(item)` says where) when it is a different file, left alone and
/// reported when it is neither (a directory, a link) or cannot be removed. A name that is already
/// gone but whose set-aside copy exists (an earlier attempt moved it and then could not flush the
/// directories) has its flushes repeated and is reported again, so the journal is only released
/// once the move is durable.
fn finish_leftover(
    ctx: &Ctx<'_>,
    path: &Path,
    expect: &Ident,
    root: usize,
) -> std::result::Result<Option<RetainedItem>, RetainedItem> {
    match probe(path) {
        Ok(Probe::File(now)) if now != *expect => set_aside(ctx, path, root).map(Some),
        Ok(Probe::Missing) => resume_set_aside(ctx, path, root),
        _ => remove_matching(ctx, path, expect).map(|()| None),
    }
}

/// `.rktxn/retained/<id>` and the name `path` takes in it.
///
/// A leftover of another root than the coordinating one is not moved to the coordinating root's
/// directory (that may be another volume, and names from different roots could collide): it keeps
/// its directory and takes the name `<name>.retained`.
fn set_aside_target(ctx: &Ctx<'_>, path: &Path, root: usize) -> Option<(PathBuf, PathBuf)> {
    if root != 0 {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let target = path.with_file_name(format!("{name}.retained"));
        return Some((path.parent()?.to_path_buf(), target));
    }
    let (id, name) = (ctx.txn_dir.file_name()?, path.file_name()?);
    let dir = ctx.root.join(AREA).join(RETAINED_DIR).join(id);
    let target = dir.join(name);
    Some((dir, target))
}

fn set_aside_item(path: &Path, target: &Path, how: &str) -> RetainedItem {
    RetainedItem::new(
        target,
        format!(
            "{how} from {}: it no longer matched what the transaction recorded",
            path.display()
        ),
    )
}

/// Move `path` (a file of this transaction that no longer matches its recorded identity) to
/// `.rktxn/retained/<id>/<its name>` by exclusive rename: nothing is deleted or overwritten, and
/// the name it had stops being in the way. The move only counts once the directory it left and the
/// directory it entered have both been flushed; until then it is a blocking failure and the
/// journal stays, so recovery repeats the flushes. Fails (and the item is retained where it was)
/// when the rename cannot be made, for instance across volumes.
fn set_aside(
    ctx: &Ctx<'_>,
    path: &Path,
    root: usize,
) -> std::result::Result<RetainedItem, RetainedItem> {
    let Some((dir, target)) = set_aside_target(ctx, path, root) else {
        return Err(RetainedItem::new(path, "it has no name to set aside under"));
    };
    create_dir_all_durable(&dir, true).map_err(|e| RetainedItem::new(path, e))?;
    rename_durable_exclusive(path, &target).map_err(|e| RetainedItem::new(path, e))?;
    sync_set_aside(path, &target)?;
    Ok(set_aside_item(path, &target, "moved here"))
}

/// A set-aside copy left by an earlier attempt whose flushes failed: flush again and report it.
fn resume_set_aside(
    ctx: &Ctx<'_>,
    path: &Path,
    root: usize,
) -> std::result::Result<Option<RetainedItem>, RetainedItem> {
    let Some((_, target)) = set_aside_target(ctx, path, root) else {
        return Ok(None);
    };
    let looked = fire(Step::SetAsideProbe, &target)
        .and_then(|()| fs::symlink_metadata(&target).map_err(|e| FsError::io(&target, e)));
    match looked {
        Ok(_) => {}
        // Only a proven "not found" means no earlier attempt moved anything. Any other error
        // (EIO, access denied) leaves it unknown, so the journal must stay.
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(RetainedItem::new(&target, error)),
    }
    sync_set_aside(path, &target)?;
    Ok(Some(set_aside_item(path, &target, "moved here earlier")))
}

/// Flush the directory `path` was moved out of and the directory `target` is in. Both must
/// succeed: a rename alone does not prove the new name survives a power loss.
fn sync_set_aside(path: &Path, target: &Path) -> std::result::Result<(), RetainedItem> {
    let failed = |dir: &Path, error: FsError| {
        RetainedItem::new(
            target,
            format!(
                "moved from {} but {} could not be flushed ({error}); the journal stays so recovery repeats it",
                path.display(),
                dir.display()
            ),
        )
    };
    if let Some(source_dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fire(Step::SetAsideSyncSource, source_dir)
            .and_then(|()| sync_dir_strict(source_dir))
            .map_err(|e| failed(source_dir, e))?;
    }
    if let Some(target_dir) = target.parent() {
        fire(Step::SetAsideSyncTarget, target_dir)
            .and_then(|()| sync_dir_strict(target_dir))
            .map_err(|e| failed(target_dir, e))?;
    }
    Ok(())
}

/// Remove a staged name that is the file this transaction created: `claim` is its journaled
/// identity. In phase `staging` the claim is the file key only (`full` false); in phase `staged` the
/// whole identity must still match. A file with no claim, another identity, or that is not a
/// regular file is retained and reported, never removed on its name.
fn remove_claimed(
    path: &Path,
    claim: Option<&Ident>,
    full: bool,
    retry: Option<RetryPolicy>,
) -> std::result::Result<(), RetainedItem> {
    let Some(claim) = claim else {
        // Nothing to prove identity with: look, never delete.
        return match probe(path) {
            Ok(Probe::Missing) => Ok(()),
            Ok(Probe::File(_)) => Err(RetainedItem::new(
                path,
                "the journal records no staged file for this name, so it was not removed",
            )),
            Ok(Probe::Other(why)) => Err(RetainedItem::new(path, format!("it is {why}"))),
            Err(error) => Err(RetainedItem::new(path, error)),
        };
    };
    remove_while_identical(
        path,
        |now| {
            if full {
                now == claim
            } else {
                now.key == claim.key
            }
        },
        "it is not the file the transaction journaled for this staged name, so it was not removed",
        retry,
    )
}

/// Remove the staged copies of a transaction that never reached its first live change, then its
/// journal and directory. A staged name is removed only if the journal's record of the file this
/// transaction created there matches (see [`remove_claimed`]); anything else at a staged name is
/// retained. The candidates themselves are never touched.
fn discard(ctx: &Ctx<'_>, journal: &Journal) -> Vec<RetainedItem> {
    let mut retained = Vec::new();
    let full = journal.phase == JournalPhase::Staged;
    for entry in &journal.entries {
        match entry_paths(ctx, entry) {
            Ok(paths) => {
                if let Err(item) =
                    remove_claimed(&paths.stage, entry.staged.as_ref(), full, ctx.options.retry)
                {
                    retained.push(item);
                }
            }
            Err(error) => retained.push(RetainedItem::new(ctx.shown(entry), error)),
        }
    }
    if retained.is_empty() {
        retained.extend(finish_dir(ctx));
    }
    retained
}

/// Remove one protocol record only while its publication/recovery claim still holds.
fn remove_record(ctx: &Ctx<'_>, name: &str) -> std::result::Result<(), RetainedItem> {
    let path = ctx.txn_dir.join(name);
    let claim = ctx.record_claim(name);
    remove_while_identical(
        &path,
        |now| {
            claim.as_ref().is_some_and(|claim| {
                *now == claim.ident
                    && read_record_claim(&path, &claim.text)
                        .is_ok_and(|found| found.ident == claim.ident)
            })
        },
        "the protocol record no longer has its recorded identity and exact content",
        ctx.options.retry,
    )
}

/// Remove only identity/content-claimed journal and marker records, then the empty directory.
/// Record temps have no durable ownership claim and are retained.
/// The order matters: the journal goes before the marker, so a crash between them leaves a marker
/// without a journal (finished by recovery), never a journal without its marker (which would read
/// as an uncommitted transaction). The order alone is not enough on a filesystem that may persist
/// two unlinks in either order, so the transaction directory is synced after the journal unlink
/// and before the marker is touched (a committed transaction can then never read as uncommitted
/// after a power loss), and once more after the marker unlink. Windows uses a writable directory
/// handle flush; a failure retains recovery material. Unknown files and record temps are retained.
fn finish_dir(ctx: &Ctx<'_>) -> Vec<RetainedItem> {
    let txn_dir = ctx.txn_dir;
    let mut retained = Vec::new();
    // Generated syntax alone never proves ownership of a record temp. Retain all unknown files,
    // and preserve the journal when any record is unrecognized before cleanup starts.
    match fs::read_dir(txn_dir) {
        Ok(listing) => {
            for entry in listing {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        retained.push(RetainedItem::new(txn_dir, error));
                        continue;
                    }
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                let valid = ctx.record_claim(&name).is_some_and(|claim| {
                    read_record_claim(&entry.path(), &claim.text)
                        .is_ok_and(|found| found.ident == claim.ident)
                });
                if !matches!(name.as_str(), JOURNAL | COMMIT) || !valid {
                    retained.push(RetainedItem::new(
                        entry.path(),
                        "unclaimed or changed protocol material",
                    ));
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return retained,
        Err(error) => return vec![RetainedItem::new(txn_dir, error)],
    }
    if !retained.is_empty() {
        return retained;
    }
    let journal = txn_dir.join(JOURNAL);
    if let Err(error) = fire(Step::BeforeJournalRemove, &journal) {
        return vec![RetainedItem::new(&journal, error)];
    }
    if let Err(item) = remove_record(ctx, JOURNAL) {
        return vec![item];
    }
    let removed = sync_dir_strict(txn_dir).and_then(|()| fire(Step::AfterJournalRemove, &journal));
    if let Err(error) = removed {
        return vec![RetainedItem::new(&journal, error)];
    }
    if let Err(item) = remove_record(ctx, COMMIT) {
        return vec![item];
    }
    let marker = txn_dir.join(COMMIT);
    let synced = sync_dir_strict(txn_dir).and_then(|()| fire(Step::AfterMarkerRemove, &marker));
    if let Err(error) = synced {
        return vec![RetainedItem::new(txn_dir, error)];
    }
    // The directory is empty now, but whatever listed it a moment ago (an indexer, a scanner) can
    // still hold it, and Windows then refuses the removal with a code that clears by itself. A
    // refusal that outlives the retry bound is retained and reported, not hidden.
    match remove_dir_with_retry(txn_dir, ctx.options.retry) {
        Ok(()) => {
            if let Err(error) = sync_parent_strict(txn_dir) {
                retained.push(RetainedItem::new(txn_dir, error));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => retained.push(RetainedItem::new(txn_dir, error)),
    }
    retained
}

// ---------------------------------------------------------------------------------------------
// Recovery
// ---------------------------------------------------------------------------------------------

/// Resolve every unfinished transaction under `root` with the default [`TransactionOptions`].
/// See [`recover_transactions_with`].
pub fn recover_transactions(root: &Path) -> Result<RecoveryReport> {
    recover_transactions_with(root, &TransactionOptions::default())
}

/// Resolve every unfinished transaction under `root`.
///
/// Takes the root's operation lock (so a live commit in another process is waited for, up to the
/// lock timeout, never interrupted), then for each transaction directory, newest first: one with a
/// valid commit marker is completed (cleanup only); one without is rolled back (every entry
/// restored by identity, a `staging` journal simply discarded); one whose journal cannot be read,
/// whose identities do not match, or whose cleanup fails is **retained** and reported, never
/// deleted. The journal is bound to the root directory's full file identity (device number and
/// file index): a root that was copied, or whose device number differs for any reason, is not
/// recovered from a journal written for the original and is retained for a person to resolve. The
/// directory `.rktxn/retained` holds set-aside material and is not a transaction.
///
/// An absent `.rktxn` directory is an empty clean report; nothing is created. Running recovery
/// again after it returned a clean report finds nothing.
pub fn recover_transactions_with(
    root: &Path,
    options: &TransactionOptions,
) -> Result<RecoveryReport> {
    let root = canonicalize_plain(root).map_err(|e| FsError::io(root, e))?;
    let area = root.join(AREA);
    match fs::symlink_metadata(&area) {
        Ok(meta) => {
            if is_link(&meta) {
                return Err(FsError::Symlink(area));
            }
            if !meta.is_dir() {
                return Err(FsError::NotADirectory(area));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(RecoveryReport::default()),
        Err(e) => return Err(FsError::io(&area, e)),
    }
    let _lock = FileLock::acquire(&area.join(LOCK_NAME), &lock_options(options))?;
    let root_key = FileKey::of_path(&root)?;
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(&area).map_err(|e| FsError::io(&area, e))? {
        let entry = entry.map_err(|e| FsError::io(&area, e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| FsError::io(entry.path(), e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() && name != RETAINED_DIR {
            names.push(name);
        }
    }
    names.sort();
    names.reverse();
    let mut report = RecoveryReport::default();
    for name in names {
        let txn_dir = area.join(&name);
        let ctx = Ctx {
            records: std::cell::RefCell::new(Vec::new()),
            root: &root,
            others: std::cell::RefCell::new(Vec::new()),
            options,
            txn_dir: &txn_dir,
        };
        let outcome = recover_one(&ctx, &name, root_key);
        report
            .transactions
            .push(RecoveredTransaction { id: name, outcome });
    }
    Ok(report)
}

fn retained_outcome(reason: &str, items: Vec<RetainedItem>) -> RecoveryOutcome {
    RecoveryOutcome::Retained {
        reason: reason.to_string(),
        items,
    }
}

fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Find each non-coordinating root of a multi-root journal again and prove it is the directory
/// that was recorded (same file index and device). The journal that lives in the coordinating
/// root is the only decision for every root: a root that cannot be reached or whose identity
/// differs keeps the whole transaction retained, and nothing in any root is touched. A reboot
/// that renumbers a device is therefore resolved by a person, as for the coordinating root.
fn resolve_other_roots(ctx: &Ctx<'_>, journal: &Journal) -> std::result::Result<(), RetainedItem> {
    let mut resolved = Vec::with_capacity(journal.others.len());
    for other in &journal.others {
        let shown = Path::new(&other.path);
        let root = canonicalize_plain(shown).map_err(|e| {
            RetainedItem::new(
                shown,
                format!("the root cannot be reached ({e}); recover when it is available"),
            )
        })?;
        let key = FileKey::of_path(&root).map_err(|e| RetainedItem::new(&root, e))?;
        if key != other.key {
            return Err(RetainedItem::new(
                &root,
                "this directory is not the one the journal recorded for the root (its file identity differs)",
            ));
        }
        resolved.push(root);
    }
    *ctx.others.borrow_mut() = resolved;
    Ok(())
}

fn recover_one(ctx: &Ctx<'_>, name: &str, root_key: FileKey) -> RecoveryOutcome {
    let journal_path = ctx.txn_dir.join(JOURNAL);
    let marker_path = ctx.txn_dir.join(COMMIT);
    if !valid_id(name) {
        return retained_outcome(
            "an unrecognized directory under .rktxn",
            vec![RetainedItem::new(ctx.txn_dir, "not a transaction id")],
        );
    }
    let has_journal = present(&journal_path);
    let has_marker = present(&marker_path);
    if !has_journal {
        // Either nothing was journaled, or a committed transaction finished everything but the
        // marker and the directory.
        if has_marker {
            let text = match fs::read_to_string(&marker_path) {
                Ok(text) => text,
                Err(error) => {
                    return retained_outcome(
                        "the remaining marker cannot be read",
                        vec![RetainedItem::new(&marker_path, error)],
                    )
                }
            };
            let prefix = format!("{COMMIT_MAGIC} {name} ");
            let valid = text
                .strip_prefix(&prefix)
                .and_then(|tail| tail.strip_suffix('\n'))
                .is_some_and(|crc| crc.len() == 8 && u32::from_str_radix(crc, 16).is_ok());
            if !valid {
                return retained_outcome(
                    "unrecognized marker without a journal",
                    vec![RetainedItem::new(
                        &marker_path,
                        "marker does not identify this transaction",
                    )],
                );
            }
            match read_record_claim(&marker_path, &text) {
                Ok(claim) => ctx.claim_record(&marker_path, claim),
                Err(error) => {
                    return retained_outcome(
                        "the remaining marker changed",
                        vec![RetainedItem::new(&marker_path, error)],
                    )
                }
            }
        }
        let left = finish_dir(ctx);
        return match (left.is_empty(), has_marker) {
            (true, true) => RecoveryOutcome::Completed,
            (true, false) => RecoveryOutcome::Discarded,
            (false, _) => retained_outcome(
                "the transaction directory holds material recovery will not delete",
                left,
            ),
        };
    }
    let mut journal = match load_journal(&journal_path) {
        Ok(journal) => journal,
        Err(error) => {
            return retained_outcome(
                "the journal cannot be read",
                vec![RetainedItem::new(&journal_path, error)],
            )
        }
    };
    match read_record_claim(&journal_path, &journal.render().0) {
        Ok(claim) => ctx.claim_record(&journal_path, claim),
        Err(error) => {
            return retained_outcome(
                "the journal changed while loading",
                vec![RetainedItem::new(&journal_path, error)],
            )
        }
    }
    if journal.id != name {
        return retained_outcome(
            "the journal belongs to another transaction",
            vec![RetainedItem::new(
                &journal_path,
                "its id differs from its directory name",
            )],
        );
    }
    if journal.root.index != root_key.index {
        return retained_outcome(
            "the journal was written for a different root directory",
            vec![RetainedItem::new(
                &journal_path,
                "the root's file identity differs (the root was copied or replaced)",
            )],
        );
    }
    if journal.root.volume != root_key.volume {
        // Same file index, other device: a restart may have renumbered the device, or this may be
        // another filesystem with the same index. Nothing here can tell which, so nothing is
        // restored, completed or deleted; a person decides.
        return retained_outcome(
            "the journal's root directory is on a different device than recorded",
            vec![RetainedItem::new(
                &journal_path,
                "the root has the recorded file index but another device number; a renumbering after a restart is not proven, so the transaction is kept for a person to resolve",
            )],
        );
    }
    if let Err(item) = resolve_other_roots(ctx, &journal) {
        return retained_outcome(
            "a root of the transaction cannot be resolved to the directory the journal recorded",
            vec![item],
        );
    }
    if has_marker {
        let expected = marker_text(&journal.id, journal.crc);
        let matches = fs::read_to_string(&marker_path)
            .map(|text| text == expected)
            .unwrap_or(false);
        if !matches || journal.phase != JournalPhase::Staged {
            return retained_outcome(
                "the commit marker does not match the journal",
                vec![RetainedItem::new(
                    &marker_path,
                    "unreadable, or bound to a different journal",
                )],
            );
        }
        match read_record_claim(&marker_path, &expected) {
            Ok(claim) => ctx.claim_record(&marker_path, claim),
            Err(error) => {
                return retained_outcome(
                    "the marker changed while loading",
                    vec![RetainedItem::new(&marker_path, error)],
                )
            }
        }
        let cleanup = complete_committed(ctx, &journal);
        return if !cleanup.blocking.is_empty() {
            retained_outcome(
                "committed, but cleanup could not finish",
                cleanup.into_items(),
            )
        } else if cleanup.set_aside.is_empty() {
            RecoveryOutcome::Completed
        } else {
            RecoveryOutcome::CompletedSetAside {
                items: cleanup.set_aside,
            }
        };
    }
    match journal.phase {
        JournalPhase::Staging => {
            let left = discard(ctx, &journal);
            if left.is_empty() {
                RecoveryOutcome::Discarded
            } else {
                retained_outcome("staged copies could not be removed", left)
            }
        }
        JournalPhase::Staged => {
            let outcome = rollback(ctx, &mut journal);
            if outcome.retained.is_empty() {
                RecoveryOutcome::RolledBack
            } else {
                retained_outcome(
                    "rollback stopped at material that does not match what the journal recorded",
                    outcome.retained,
                )
            }
        }
    }
}
