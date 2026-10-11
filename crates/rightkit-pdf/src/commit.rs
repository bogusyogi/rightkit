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
//! them. [`FileIdentity`] is the volume and file id read from the file itself (`dev`/`ino` on Unix,
//! `FILE_ID_INFO` from an opened handle on Windows) together with the canonical path; a destination
//! that does not exist yet is its canonical parent directory plus its file name. Where the platform
//! cannot give an existing file an id, the commit is refused (fail closed) rather than compared by
//! path alone.
//!
//! A symlink destination is resolved **once**: the lock, the source recheck and the replacement all
//! use the resolved path, so the rename replaces the file the link points at and never the link
//! itself. The destination is resolved again after the lock is obtained; if it now resolves
//! elsewhere (the link was retargeted while this commit waited) the commit fails with
//! [`PdfEditError::WriteConflict`].
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

/// A file's `(volume, file id)`. Unix uses `(dev, ino)`; Windows uses `FILE_ID_INFO` (a 64-bit
/// volume serial and a 128-bit file id).
type FileKey = (u64, u128);

/// Which file a path names, independent of how it is spelled.
#[derive(Clone, Debug)]
struct FileIdentity {
    /// The volume and file id of the file when it exists.
    file: Option<FileKey>,
    /// The canonical path with every symlink resolved; for a file that does not exist, the
    /// canonical parent directory plus the file name. This is the path a commit writes to.
    path: PathBuf,
}

#[cfg(unix)]
fn file_key(path: &Path) -> std::io::Result<Option<FileKey>> {
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(path) {
        Ok(meta) => Ok(Some((meta.dev(), u128::from(meta.ino())))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Volume serial and file id from an opened handle (attributes-only access, so the open does not
/// contend with a reader). `FILE_ID_INFO` (Windows 8 and later) carries the full 128-bit id that
/// ReFS needs; the older 64-bit file index is the fallback. A failure of both is an error: the
/// caller refuses the commit rather than guess.
#[cfg(windows)]
fn file_key(path: &Path) -> std::io::Result<Option<FileKey>> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
    };

    let file = match fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let handle = file.as_raw_handle() as HANDLE;

    // SAFETY: `handle` is open for the duration of the calls and the buffers are plain structs of
    // the sizes passed.
    unsafe {
        let mut info: FILE_ID_INFO = std::mem::zeroed();
        let ok = GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        );
        if ok != 0 {
            return Ok(Some((
                info.VolumeSerialNumber,
                u128::from_le_bytes(info.FileId.Identifier),
            )));
        }
        let mut basic: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(handle, &mut basic) != 0 {
            let index = (u64::from(basic.nFileIndexHigh) << 32) | u64::from(basic.nFileIndexLow);
            return Ok(Some((
                u64::from(basic.dwVolumeSerialNumber),
                u128::from(index),
            )));
        }
    }
    Err(std::io::Error::last_os_error())
}

/// No file id is available on this platform: an existing destination cannot be identified, so it
/// is refused; a destination that does not exist yet has no id to compare.
#[cfg(not(any(unix, windows)))]
fn file_key(path: &Path) -> std::io::Result<Option<FileKey>> {
    match fs::metadata(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "file identity is unavailable on this platform",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Resolves `path` once: every symlink followed for an existing file; for a file that does not
/// exist, its canonical parent plus its name. A dangling symlink is an error - replacing it would
/// replace the link and leave its target unprotected.
fn resolve_destination(path: &Path) -> std::io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok() {
                return Err(error);
            }
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let parent = fs::canonicalize(parent)
                .or_else(|_| std::path::absolute(parent))
                .unwrap_or_else(|_| parent.to_path_buf());
            Ok(match path.file_name() {
                Some(name) => parent.join(name),
                None => parent,
            })
        }
        Err(error) => Err(error),
    }
}

impl FileIdentity {
    /// The identity of what `path` names right now. An error means the identity cannot be
    /// established; the commit then fails closed.
    fn try_of(path: &Path) -> std::io::Result<FileIdentity> {
        let resolved = resolve_destination(path)?;
        let file = file_key(&resolved)?;
        Ok(FileIdentity {
            file,
            path: resolved,
        })
    }

    /// True when both name the same file: the same volume and file id when both have them (a hard
    /// link or a case variant matches here), else the same canonical path.
    fn same_file(&self, other: &FileIdentity) -> bool {
        match (self.file, other.file) {
            (Some(a), Some(b)) => a == b,
            _ => self.path == other.path,
        }
    }
}

/// `true` when `a` and `b` name the same file, by [`FileIdentity`]. When either identity cannot be
/// established the answer is `true`: the source recheck then runs, which is the safe direction.
fn same_file(a: &str, b: &str) -> bool {
    match (
        FileIdentity::try_of(Path::new(a)),
        FileIdentity::try_of(Path::new(b)),
    ) {
        (Ok(a), Ok(b)) => a.same_file(&b),
        _ => true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DestinationKey {
    /// An existing file, by device and file id: reached by any path, link or case spelling.
    File(u64, u128),
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
    // Resolve once (symlinks followed), lock that file, then resolve again: a link retargeted
    // while this commit waited is a different destination than the one that was locked.
    let before = FileIdentity::try_of(&destination)?;
    let _destination = DestinationGuard::acquire(&before);
    let identity = FileIdentity::try_of(&destination)?;
    if identity.path != before.path {
        return Err(PdfEditError::WriteConflict.into());
    }

    let (_source, bytes, result) = resolve_commit_with(
        store,
        token,
        target.map(|p| p.to_string_lossy().into_owned()),
        same_file,
        read_source,
    )?;
    // Replace the resolved file, never a symlink that points at it.
    write_atomic(&identity.path, &bytes)?;

    // The destination now holds the candidate. Any other token staged against that file recorded
    // bytes that are gone; it must not be able to commit over this write. A staged path whose
    // identity cannot be read is invalidated too.
    let written = FileIdentity::try_of(&identity.path)?;
    for staged in store.source_paths() {
        let same = match FileIdentity::try_of(Path::new(&staged)) {
            Ok(staged_identity) => staged_identity.same_file(&written),
            Err(_) => true,
        };
        if same {
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

    fn identity_of(path: &Path) -> FileIdentity {
        FileIdentity::try_of(path).expect("identity")
    }

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

        // Both commits read the source before either writes - the audit's interleaving. Without a
        // lock across recheck and replacement, both compare the original bytes and both succeed.
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
                        // Read first, meet second, return what was read: both contenders hold
                        // the same original snapshot, so without the destination lock both pass
                        // the source check and both succeed, whatever the interleaving after.
                        let snapshot = fs::read(path).map_err(|_| PdfEditError::WriteConflict);
                        meeting.meet(2, Duration::from_millis(1500));
                        snapshot
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

        let first = DestinationGuard::acquire(&identity_of(&file));
        let second_got_it = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&second_got_it);
        let waiting = std::thread::spawn(move || {
            let _held = DestinationGuard::acquire(&identity_of(&alias));
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
        let _a = DestinationGuard::acquire(&identity_of(&file));
        let _b = DestinationGuard::acquire(&identity_of(&other));
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

        let canonical = identity_of(&file);
        for alias in [
            dir.join(".").join("memo.pdf"),
            dir.join("sub").join("..").join("memo.pdf"),
        ] {
            assert!(
                canonical.same_file(&identity_of(&alias)),
                "{}",
                alias.display()
            );
        }
        assert!(same_file(
            file.to_str().expect("utf-8"),
            dir.join(".").join("memo.pdf").to_str().expect("utf-8")
        ));
        assert!(!canonical.same_file(&identity_of(&other)));

        // A destination that does not exist yet is its canonical directory plus its name.
        let fresh = dir.join("sub").join("..").join("new.pdf");
        assert!(identity_of(&fresh).same_file(&identity_of(&dir.join("new.pdf"))));
        assert!(!identity_of(&fresh).same_file(&identity_of(&dir.join("old.pdf"))));

        #[cfg(unix)]
        {
            let linked = dir.join("linked.pdf");
            fs::hard_link(&file, &linked).expect("hard link");
            assert!(canonical.same_file(&identity_of(&linked)));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// Stages one candidate per entry of `candidates` against `source_path`, whose bytes are `source`.
    fn stage_all(
        store: &PdfSaveTokenStore,
        source_path: &Path,
        source: &[u8],
        candidates: &[Vec<u8>],
    ) -> Vec<String> {
        let sha = document_sha256(source);
        candidates
            .iter()
            .map(|candidate| {
                stage_prepared_save(
                    store,
                    source_path.to_str().expect("utf-8 path"),
                    &sha,
                    &sha,
                    candidate.clone(),
                    vec![1],
                    vec![],
                )
                .expect("stages")
                .token
            })
            .collect()
    }

    #[test]
    #[cfg(unix)]
    fn finding2_a_symlink_destination_is_resolved_once_and_the_sibling_token_dies() {
        let dir = scratch("symlink");
        let real = dir.join("A.pdf");
        let link = dir.join("B.pdf");
        let source = build_pdf(b"BT /F1 12 Tf 72 700 Td (Source) Tj ET");
        fs::write(&real, &source).expect("write source");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let candidates = [
            build_pdf(b"BT /F1 12 Tf 72 700 Td (First) Tj ET"),
            build_pdf(b"BT /F1 12 Tf 72 700 Td (Second) Tj ET"),
        ];
        let store = PdfSaveTokenStore::default();
        let tokens = stage_all(&store, &real, &source, &candidates);

        commit_pdf_save(&store, &tokens[0], Some(&link)).expect("first commit");
        assert!(
            fs::symlink_metadata(&link)
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "the commit must replace the file the link points at, not the link"
        );
        assert_eq!(fs::read(&real).expect("read"), candidates[0]);

        let second = commit_pdf_save(&store, &tokens[1], Some(&link));
        assert!(
            matches!(second, Err(CommitError::Edit(PdfEditError::WriteConflict))),
            "{second:?}"
        );
        assert_eq!(fs::read(&real).expect("read"), candidates[0]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn finding2_symlink_and_target_share_one_resolved_identity_and_a_dangling_link_is_refused() {
        let dir = scratch("symlink-identity");
        let real = dir.join("A.pdf");
        fs::write(&real, b"x").expect("file");
        let link = dir.join("B.pdf");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert_eq!(identity_of(&link).path, identity_of(&real).path);
        assert!(identity_of(&link).same_file(&identity_of(&real)));

        let dangling = dir.join("gone.pdf");
        std::os::unix::fs::symlink(dir.join("missing.pdf"), &dangling).expect("dangling");
        assert!(FileIdentity::try_of(&dangling).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finding3_a_hard_link_destination_still_rechecks_the_source() {
        let dir = scratch("hardlink");
        let real = dir.join("A.pdf");
        let linked = dir.join("B.pdf");
        let source = build_pdf(b"BT /F1 12 Tf 72 700 Td (Source) Tj ET");
        fs::write(&real, &source).expect("write source");
        fs::hard_link(&real, &linked).expect("hard link");
        let candidates = [build_pdf(b"BT /F1 12 Tf 72 700 Td (Mine) Tj ET")];
        let store = PdfSaveTokenStore::default();
        let tokens = stage_all(&store, &real, &source, &candidates);

        // Someone rewrites A (and so B) after staging.
        let newer = build_pdf(b"BT /F1 12 Tf 72 700 Td (Newer) Tj ET");
        fs::write(&real, &newer).expect("external rewrite");

        let outcome = commit_pdf_save(&store, &tokens[0], Some(&linked));
        assert!(
            matches!(outcome, Err(CommitError::Edit(PdfEditError::WriteConflict))),
            "{outcome:?}"
        );
        assert_eq!(
            fs::read(&linked).expect("read"),
            newer,
            "newer bytes survive"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finding3_a_file_has_a_volume_and_file_id_that_hard_links_share() {
        let dir = scratch("file-key");
        let real = dir.join("A.pdf");
        let linked = dir.join("B.pdf");
        let other = dir.join("C.pdf");
        fs::write(&real, b"x").expect("file");
        fs::write(&other, b"x").expect("other");
        fs::hard_link(&real, &linked).expect("hard link");
        let a = file_key(&real).expect("key").expect("exists");
        assert_eq!(a, file_key(&linked).expect("key").expect("exists"));
        assert_ne!(a, file_key(&other).expect("key").expect("exists"));
        assert_eq!(file_key(&dir.join("absent.pdf")).expect("key"), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
