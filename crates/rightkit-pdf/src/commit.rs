//! Atomic on-disk commit for a staged save token.
//!
//! [`crate::save::resolve_commit_with`] is filesystem-free; this module supplies the real source
//! re-read, the file-identity comparison that decides whether that re-read applies, a
//! per-destination lock, and a crash-safe replace (write temp sibling, fsync, rename). Path
//! authorization is the caller's job.
//!
//! ## Concurrency and aliases
//!
//! A commit is "check the source, replace the destination, invalidate the siblings". Two commits
//! staged against the same source must not interleave those steps - each would read the original
//! bytes, both would pass the check, and the later rename would silently discard the earlier edit.
//! So every commit takes a lock on its destination's *identity* first and holds it across all three
//! steps; the second commit then finds its token invalidated (or the source changed) and fails with
//! [`PdfEditError::WriteConflict`], the same error a source changed by someone else gives.
//!
//! Identity is not the path string. `/tmp/./memo.pdf`, a hard link, a symlink, and (on a
//! case-insensitive volume) `MEMO.pdf` are the same file, and the source recheck must run for all of
//! them. [`FileIdentity`] is the device and file id where the platform gives them (Unix) with the
//! canonical path as the fallback; a destination that does not exist yet is its canonical parent
//! directory plus its file name.
//!
//! The lock is in-process. Another program replacing the file between the recheck and the rename is
//! narrowed by the recheck, not excluded.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};

use crate::model::PdfEditError;
use crate::save::{resolve_commit_with, PdfCommitResult, PdfSaveTokenStore};

#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error(transparent)]
    Edit(#[from] PdfEditError),
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Writes `bytes` to `target` atomically: a temp file in the same directory is fsynced and then
/// renamed over the target, so readers observe either the old or the new file, never a mix.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut nonce = [0_u8; 6];
    getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let tmp: PathBuf = dir.join(format!(".{name}.{suffix}.tmp"));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Which file a path names, independent of how it is spelled.
#[derive(Clone, Debug)]
struct FileIdentity {
    /// `(device, file id)` of the file when it exists and the platform exposes them.
    file: Option<(u64, u64)>,
    /// The canonical path; for a file that does not exist, the canonical parent directory plus
    /// the file name.
    path: PathBuf,
}

#[cfg(unix)]
fn file_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

/// `std` exposes no stable file id off Unix; the canonical path (the on-disk spelling, case
/// included, on Windows) is the identity there.
#[cfg(not(unix))]
fn file_id(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

impl FileIdentity {
    fn of(path: &Path) -> FileIdentity {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let parent = fs::canonicalize(parent)
                .or_else(|_| std::path::absolute(parent))
                .unwrap_or_else(|_| parent.to_path_buf());
            match path.file_name() {
                Some(name) => parent.join(name),
                None => parent,
            }
        });
        FileIdentity {
            file: fs::metadata(path).ok().as_ref().and_then(file_id),
            path: canonical,
        }
    }

    /// True when both name the same file: the same device and file id when both have them (a hard
    /// link or a case variant matches here), else the same canonical path.
    fn same_file(&self, other: &FileIdentity) -> bool {
        match (self.file, other.file) {
            (Some(a), Some(b)) => a == b,
            _ => self.path == other.path,
        }
    }
}

/// `true` when `a` and `b` name the same file, by [`FileIdentity`].
fn same_file(a: &str, b: &str) -> bool {
    FileIdentity::of(Path::new(a)).same_file(&FileIdentity::of(Path::new(b)))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DestinationKey {
    /// An existing file, by device and file id: reached by any path, link or case spelling.
    File(u64, u64),
    /// A canonical location, existing or not.
    Path(PathBuf),
}

/// The destinations a commit currently holds. One mutex and one condition variable cover the whole
/// process; a commit waits until none of its keys is held, then takes all of them at once, so two
/// commits can never each hold half of what the other needs.
struct DestinationLocks {
    held: Mutex<BTreeSet<DestinationKey>>,
    released: Condvar,
}

static DESTINATION_LOCKS: DestinationLocks = DestinationLocks {
    held: Mutex::new(BTreeSet::new()),
    released: Condvar::new(),
};

/// Exclusive hold on one destination file until dropped.
struct DestinationGuard {
    keys: Vec<DestinationKey>,
}

impl DestinationGuard {
    fn acquire(identity: &FileIdentity) -> DestinationGuard {
        let mut keys = vec![DestinationKey::Path(identity.path.clone())];
        if let Some((device, file)) = identity.file {
            keys.push(DestinationKey::File(device, file));
        }
        let mut held = DESTINATION_LOCKS
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while keys.iter().any(|key| held.contains(key)) {
            held = DESTINATION_LOCKS
                .released
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
        held.extend(keys.iter().cloned());
        DestinationGuard { keys }
    }
}

impl Drop for DestinationGuard {
    fn drop(&mut self) {
        let mut held = DESTINATION_LOCKS
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for key in &self.keys {
            held.remove(key);
        }
        drop(held);
        DESTINATION_LOCKS.released.notify_all();
    }
}

/// Consumes `token`, re-checks the on-disk source for same-file saves, and atomically writes the
/// verified candidate to `target` (or back to the source path when `None`).
///
/// "Same file" is decided by filesystem identity, not by comparing path strings, and the whole
/// check-replace-invalidate sequence runs under a lock on the destination (see the module docs).
/// A commit that loses the race, or whose source changed, fails with
/// `CommitError::Edit(PdfEditError::WriteConflict)` and writes nothing.
pub fn commit_pdf_save(
    store: &PdfSaveTokenStore,
    token: &str,
    target: Option<&Path>,
) -> Result<PdfCommitResult, CommitError> {
    commit_with(store, token, target, |source| {
        fs::read(source).map_err(|_| PdfEditError::WriteConflict)
    })
}

/// [`commit_pdf_save`] with the source re-read supplied, so a test can interleave two commits at
/// the exact point the race lives.
fn commit_with<R>(
    store: &PdfSaveTokenStore,
    token: &str,
    target: Option<&Path>,
    read_source: R,
) -> Result<PdfCommitResult, CommitError>
where
    R: FnOnce(&str) -> Result<Vec<u8>, PdfEditError>,
{
    // The destination is known before the token is touched: the explicit target, else the source
    // path the token was staged against. An unknown token has nothing to serialise on.
    let destination: PathBuf = match target {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from(
            store
                .source_path_of(token)
                .ok_or(PdfEditError::WriteConflict)?,
        ),
    };
    let _destination = DestinationGuard::acquire(&FileIdentity::of(&destination));

    let (_source, bytes, result) = resolve_commit_with(
        store,
        token,
        target.map(|p| p.to_string_lossy().into_owned()),
        same_file,
        read_source,
    )?;
    write_atomic(Path::new(&result.target_path), &bytes)?;

    // The destination now holds the candidate. Any other token staged against that file recorded
    // bytes that are gone; it must not be able to commit over this write.
    let written = FileIdentity::of(Path::new(&result.target_path));
    for staged in store.source_paths() {
        if FileIdentity::of(Path::new(&staged)).same_file(&written) {
            store.invalidate_source(&staged);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::model::document_sha256;
    use crate::save::stage_prepared_save;
    use crate::test_fixtures::build_one_page_pdf_bytes as build_pdf;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rightkit-pdf-commit-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Lets `parties` threads reach a point together, but never waits longer than `patience`:
    /// with correct locking the second party can never arrive while the first is waiting.
    struct Rendezvous {
        arrived: Mutex<usize>,
        all: Condvar,
    }

    impl Rendezvous {
        fn meet(&self, parties: usize, patience: Duration) {
            let mut arrived = self.arrived.lock().expect("rendezvous");
            *arrived += 1;
            self.all.notify_all();
            let _ = self
                .all
                .wait_timeout_while(arrived, patience, |arrived| *arrived < parties)
                .expect("rendezvous wait");
        }
    }

    #[test]
    fn finding5_two_commits_staged_against_one_source_cannot_both_succeed() {
        let dir = scratch("race");
        let target = dir.join("memo.pdf");
        let source = build_pdf(b"BT /F1 12 Tf 72 700 Td (Source) Tj ET");
        fs::write(&target, &source).expect("write source");
        let sha = document_sha256(&source);
        let candidates = [
            build_pdf(b"BT /F1 12 Tf 72 700 Td (First) Tj ET"),
            build_pdf(b"BT /F1 12 Tf 72 700 Td (Second) Tj ET"),
        ];

        let store = PdfSaveTokenStore::default();
        let tokens: Vec<String> = candidates
            .iter()
            .map(|candidate| {
                stage_prepared_save(
                    &store,
                    target.to_str().expect("utf-8 path"),
                    &sha,
                    &sha,
                    candidate.clone(),
                    vec![1],
                    vec![],
                )
                .expect("stages")
                .token
            })
            .collect();

        // Both commits reach the source re-read together - the audit's interleaving. Without a
        // lock across recheck and replacement, both read the original bytes and both succeed.
        let meeting = Arc::new(Rendezvous {
            arrived: Mutex::new(0),
            all: Condvar::new(),
        });
        let start = Arc::new(std::sync::Barrier::new(2));
        let results: Vec<Result<PdfCommitResult, CommitError>> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for token in &tokens {
                let (store, meeting, start) = (&store, Arc::clone(&meeting), Arc::clone(&start));
                handles.push(scope.spawn(move || {
                    start.wait();
                    commit_with(store, token, None, |path| {
                        meeting.meet(2, Duration::from_millis(1500));
                        fs::read(path).map_err(|_| PdfEditError::WriteConflict)
                    })
                }));
            }
            let mut finished = Vec::new();
            for handle in handles {
                finished.push(handle.join().expect("commit thread"));
            }
            finished
        });
        let winners: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, result)| result.is_ok())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(winners.len(), 1, "exactly one commit may win: {results:?}");
        let loser = &results[1 - winners[0]];
        assert!(
            matches!(loser, Err(CommitError::Edit(PdfEditError::WriteConflict))),
            "{loser:?}"
        );
        assert_eq!(
            fs::read(&target).expect("read back"),
            candidates[winners[0]],
            "the file holds exactly the winner's candidate"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_destination_guard_excludes_every_alias_of_the_same_file() {
        let dir = scratch("guard");
        let file = dir.join("a.pdf");
        fs::write(&file, b"x").expect("file");
        let alias = dir.join(".").join("a.pdf");

        let first = DestinationGuard::acquire(&FileIdentity::of(&file));
        let second_got_it = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&second_got_it);
        let waiting = std::thread::spawn(move || {
            let _held = DestinationGuard::acquire(&FileIdentity::of(&alias));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !second_got_it.load(std::sync::atomic::Ordering::SeqCst),
            "an alias must wait for the first holder"
        );
        drop(first);
        waiting.join().expect("second holder");
        assert!(second_got_it.load(std::sync::atomic::Ordering::SeqCst));

        // A different file is never blocked.
        let other = dir.join("b.pdf");
        fs::write(&other, b"y").expect("other");
        let _a = DestinationGuard::acquire(&FileIdentity::of(&file));
        let _b = DestinationGuard::acquire(&FileIdentity::of(&other));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finding6_path_aliases_of_one_file_have_the_same_identity() {
        let dir = scratch("identity");
        fs::create_dir_all(dir.join("sub")).expect("sub");
        let file = dir.join("memo.pdf");
        fs::write(&file, b"x").expect("file");
        let other = dir.join("other.pdf");
        fs::write(&other, b"y").expect("other");

        let canonical = FileIdentity::of(&file);
        for alias in [
            dir.join(".").join("memo.pdf"),
            dir.join("sub").join("..").join("memo.pdf"),
        ] {
            assert!(
                canonical.same_file(&FileIdentity::of(&alias)),
                "{}",
                alias.display()
            );
        }
        assert!(same_file(
            file.to_str().expect("utf-8"),
            dir.join(".").join("memo.pdf").to_str().expect("utf-8")
        ));
        assert!(!canonical.same_file(&FileIdentity::of(&other)));

        // A destination that does not exist yet is its canonical directory plus its name.
        let fresh = dir.join("sub").join("..").join("new.pdf");
        assert!(FileIdentity::of(&fresh).same_file(&FileIdentity::of(&dir.join("new.pdf"))));
        assert!(!FileIdentity::of(&fresh).same_file(&FileIdentity::of(&dir.join("old.pdf"))));

        #[cfg(unix)]
        {
            let linked = dir.join("linked.pdf");
            fs::hard_link(&file, &linked).expect("hard link");
            assert!(canonical.same_file(&FileIdentity::of(&linked)));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
