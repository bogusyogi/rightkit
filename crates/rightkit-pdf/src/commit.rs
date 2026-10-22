//! Atomic on-disk commit for a staged save token.
//!
//! [`crate::save::resolve_commit_with`] is filesystem-free; this module supplies the real source
//! re-read, the file-identity comparison that decides whether that re-read applies, and a
//! per-destination lock. The replacement itself is not done here: it is
//! `rightkit_fs::AtomicWriter` (temp sibling in the destination's directory, flush, rename,
//! directory sync), which this crate does not reimplement. Path authorization is the caller's job.
//!
//! ## What this crate owns, and what it does not
//!
//! Owned here: the destination lock, the source re-validation, resolving a symlink destination
//! once, invalidating the other staged tokens, and the check described under "What a replacement
//! keeps" below. Owned by `rightkit-fs`: the temp file, the flush, the rename (on Windows the
//! POSIX-semantics `std::fs::rename`, never `ReplaceFileW`), the directory sync, and cleanup. A
//! write that fails before the rename leaves the original file as it was and removes the temp
//! file; there is no state in which neither version exists.
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
//! them. `FileIdentity` is the volume and file id read from the file itself (`dev`/`ino` on Unix,
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
//!
//! ## What a replacement keeps
//!
//! A replacement is a new file in the same directory, so it carries what a *new* file there would
//! carry, not what the old file had. Before any byte is written this module compares the new temp
//! file with the destination and **refuses** the save ([`CommitError::Refused`]) when a difference
//! it can see would weaken or change the file's protection; the original is untouched. It sees only
//! what `std` and `rightkit-fs` already expose, no platform call of its own:
//!
//! | Platform | Refused when | Not examined, and not kept |
//! |---|---|---|
//! | Unix | owner, group or permission bits (`0o7777`) of the new file differ from the destination's | POSIX ACLs, SELinux labels, extended attributes, file flags, hard links (siblings keep the old bytes), creation time |
//! | macOS | as Unix; and a destination whose mode is owner-only (`& 0o077 == 0`) when it or the new file is not provably owner-only, which is how `rightkit-fs` reports an extended ACL | the ACL of a destination whose mode is wider than owner-only; extended attributes; file flags |
//! | Windows | the read-only attribute or the EFS-encrypted attribute is set | the destination's DACL (a protected or explicit ACL, its owner), alternate data streams, the hidden/system/compressed attributes, creation time: the new file takes the parent directory's inherited ACL |
//!
//! "Not examined" means exactly that: such a save succeeds and the property is lost.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};

use rightkit_fs::{AtomicOptions, AtomicWriter, FsError};

use crate::model::PdfEditError;
use crate::save::{resolve_commit_with, PdfCommitResult, PdfSaveTokenStore};

#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error(transparent)]
    Edit(#[from] PdfEditError),
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
    /// The destination has protection the replacement file would not carry over. Nothing was
    /// written and the original file is unchanged. The token is spent, as for every failed commit.
    #[error("not saved, the original is unchanged: {0}")]
    Refused(#[from] Unpreserved),
    /// The destination now holds the new bytes, but a step after the replacement failed (the
    /// directory sync). The token is spent and the other tokens staged against the file are
    /// invalidated: treat the file as saved, durability across a power loss as not established.
    #[error("saved, but a step after the replacement failed: {0}")]
    Replaced(std::io::Error),
}

/// What a replacement file would not keep of the file it replaces, found before anything was
/// written. See "What a replacement keeps" in the module docs for what is, and is not, examined on
/// each platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Unpreserved {
    /// The file is read-only (Windows attribute).
    #[error("the file is read-only")]
    ReadOnly,
    /// The file is EFS-encrypted (Windows attribute); the replacement would be plaintext.
    #[error("the file is encrypted and a replacement would not be")]
    Encrypted,
    /// The replacement would have a different owner (Unix).
    #[error("the replacement would have a different owner")]
    Owner,
    /// The replacement would have a different group (Unix).
    #[error("the replacement would have a different group")]
    Group,
    /// The replacement would have different permission bits (Unix).
    #[error("the replacement would have different permission bits")]
    Mode,
    /// The file is owner-only and has, or may have, an extended ACL the replacement would not
    /// carry, or the replacement itself got one (macOS).
    #[error("the file has, or may have, an access control list the replacement would not keep")]
    Acl,
}

/// Writes `bytes` to `target` atomically through `rightkit_fs::AtomicWriter`: a temp file in the
/// same directory is flushed and then renamed over the target, and the directory is synced, so
/// readers observe either the old or the new file, never a mix, and a failure before the rename
/// leaves the old file in place.
///
/// A symlink target is resolved first, so the file it points at is replaced and the link survives.
/// Replacing an existing file is checked against what the replacement would not keep (module docs,
/// "What a replacement keeps"); a refusal or a failure after the rename is an `io::Error` whose
/// inner error is the [`CommitError`] (`get_ref()` and `downcast_ref`). This function takes no
/// destination lock and cannot invalidate staged tokens: it is the bare writer. A staged save goes
/// through [`commit_pdf_save`].
pub fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = resolve_destination(target)?;
    publish(&target, bytes).map_err(into_io)
}

fn into_io(error: CommitError) -> std::io::Error {
    match error {
        CommitError::Io(source) => source,
        other => {
            let kind = match &other {
                CommitError::Refused(_) => std::io::ErrorKind::PermissionDenied,
                CommitError::Replaced(source) => source.kind(),
                _ => std::io::ErrorKind::Other,
            };
            std::io::Error::new(kind, other)
        }
    }
}

/// An `rightkit-fs` failure as a commit failure: a failure after the rename means the new bytes
/// are in place.
fn fs_error(error: FsError) -> CommitError {
    if error.target_replaced() {
        CommitError::Replaced(error.into())
    } else {
        CommitError::Io(error.into())
    }
}

/// Replaces the already-resolved file `target` with `bytes`. The temp file is created first and
/// compared with the destination before a byte is written; if the check refuses, the writer is
/// dropped and removes the temp file.
fn publish(target: &Path, bytes: &[u8]) -> Result<(), CommitError> {
    let mut writer = AtomicWriter::create(target, AtomicOptions::default()).map_err(fs_error)?;
    match fs::metadata(target) {
        Ok(existing) => {
            if !existing.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the destination exists and is not a regular file",
                )
                .into());
            }
            keeps_protection(target, &existing, writer.temp_path())?;
        }
        // A new file: there is nothing to keep.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    writer.write_all(bytes).map_err(fs_error)?;
    writer.commit().map_err(fs_error)
}

/// The owner, group and permission bits of the new file must equal the destination's. The writer
/// restores the mode itself but ignores a failure to do so, and it never sets an owner or a group,
/// so this is what makes "kept" a fact instead of an intention.
#[cfg(unix)]
fn same_protection(existing: &fs::Metadata, made: &fs::Metadata) -> Result<(), Unpreserved> {
    use std::os::unix::fs::MetadataExt;
    if existing.uid() != made.uid() {
        return Err(Unpreserved::Owner);
    }
    if existing.gid() != made.gid() {
        return Err(Unpreserved::Group);
    }
    if existing.mode() & 0o7777 != made.mode() & 0o7777 {
        return Err(Unpreserved::Mode);
    }
    Ok(())
}

/// Compares the new temp file (still empty) with the destination. On macOS an owner-only
/// destination must stay provably owner-only: `rightkit_fs::private::is_owner_only` also looks at
/// the extended ACL there, which the mode bits do not show, and an unreadable ACL counts as "not
/// provably". Elsewhere on Unix it is the mode check again.
#[cfg(unix)]
fn keeps_protection(
    target: &Path,
    existing: &fs::Metadata,
    temp: &Path,
) -> Result<(), CommitError> {
    use std::os::unix::fs::MetadataExt;
    let made = fs::metadata(temp)?;
    same_protection(existing, &made)?;
    if existing.mode() & 0o077 == 0 {
        let owner_only = |path: &Path| rightkit_fs::private::is_owner_only(path).unwrap_or(false);
        if !owner_only(target) || !owner_only(temp) {
            return Err(Unpreserved::Acl.into());
        }
    }
    Ok(())
}

/// Windows: the new file takes the parent directory's inherited ACL. Nothing in `std` reads a
/// DACL, so a protected or explicit ACL on the destination is neither compared nor kept. The two
/// attributes `std` does expose and a replacement would silently drop are refused.
#[cfg(windows)]
fn keeps_protection(
    _target: &Path,
    existing: &fs::Metadata,
    _temp: &Path,
) -> Result<(), CommitError> {
    use std::os::windows::fs::MetadataExt;
    // winnt.h: FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_ENCRYPTED.
    const READONLY: u32 = 0x0000_0001;
    const ENCRYPTED: u32 = 0x0000_4000;
    let attributes = existing.file_attributes();
    if attributes & READONLY != 0 {
        return Err(Unpreserved::ReadOnly.into());
    }
    if attributes & ENCRYPTED != 0 {
        return Err(Unpreserved::Encrypted.into());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn keeps_protection(
    _target: &Path,
    _existing: &fs::Metadata,
    _temp: &Path,
) -> Result<(), CommitError> {
    Ok(())
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
    let published = publish(&identity.path, &bytes);

    // The destination now holds the candidate - also when only a step after the replacement failed
    // (`Replaced`). Any other token staged against bytes that are gone must not be able to commit
    // over this write. That is every staged source naming the file that was replaced (`identity`,
    // read under the lock before the rename: a hard link to it keeps the old bytes under another
    // name, so a sibling staged against that name no longer matches the destination afterwards)
    // and every staged source naming the file the destination is now. A staged path whose identity
    // cannot be read is invalidated too, and so is every staged path when the new destination
    // cannot be read back. A write that failed before the replacement changed nothing, so nothing
    // is invalidated.
    if matches!(published, Ok(()) | Err(CommitError::Replaced(_))) {
        let written = FileIdentity::try_of(&identity.path).ok();
        invalidate_aliases(store, &identity, written.as_ref());
    }
    published.map(|()| result)
}

/// Invalidates every token staged against `replaced` (the file a commit just overwrote, as it was
/// before the rename) or against `written` (what the destination path names now). `written` is
/// `None` when it could not be read back; every token then dies, the safe direction.
fn invalidate_aliases(
    store: &PdfSaveTokenStore,
    replaced: &FileIdentity,
    written: Option<&FileIdentity>,
) {
    for staged in store.source_paths() {
        let same = match FileIdentity::try_of(Path::new(&staged)) {
            Ok(staged_identity) => {
                staged_identity.same_file(replaced)
                    || written.is_none_or(|written| staged_identity.same_file(written))
            }
            Err(_) => true,
        };
        if same {
            store.invalidate_source(&staged);
        }
    }
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

    /// The temp files `rightkit-fs` left in `dir` (its generated names all contain `.rktmp-`).
    fn temps_in(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".rktmp-"))
            .collect()
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

    #[test]
    fn finding8_hard_link_sibling_tokens_die_when_the_first_commit_replaces_the_link() {
        let dir = scratch("sibling");
        let a = dir.join("A.pdf");
        let b = dir.join("B.pdf");
        let source = build_pdf(b"BT /F1 12 Tf 72 700 Td (Source) Tj ET");
        fs::write(&a, &source).expect("write source");
        fs::hard_link(&a, &b).expect("hard link");
        let candidates = [
            build_pdf(b"BT /F1 12 Tf 72 700 Td (First) Tj ET"),
            build_pdf(b"BT /F1 12 Tf 72 700 Td (Second) Tj ET"),
        ];
        let store = PdfSaveTokenStore::default();
        // Both candidates are staged against A; B is the same file when they are staged.
        let tokens = stage_all(&store, &a, &source, &candidates);

        commit_pdf_save(&store, &tokens[0], Some(&b)).expect("first commit to B");
        assert_eq!(fs::read(&b).expect("read B"), candidates[0]);
        assert_eq!(
            fs::read(&a).expect("read A"),
            source,
            "A keeps the old bytes"
        );

        // B is no longer A, so a naive identity comparison now calls this a Save As and skips the
        // source check. The sibling token must be dead instead.
        let second = commit_pdf_save(&store, &tokens[1], Some(&b));
        assert!(
            matches!(second, Err(CommitError::Edit(PdfEditError::WriteConflict))),
            "{second:?}"
        );
        assert_eq!(fs::read(&b).expect("read B"), candidates[0]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finding8_a_failed_read_back_still_invalidates_every_staged_token() {
        let dir = scratch("readback");
        let a = dir.join("A.pdf");
        let source = build_pdf(b"BT /F1 12 Tf 72 700 Td (Source) Tj ET");
        fs::write(&a, &source).expect("write source");
        let store = PdfSaveTokenStore::default();
        let tokens = stage_all(
            &store,
            &a,
            &source,
            &[build_pdf(b"BT /F1 12 Tf 72 700 Td (One) Tj ET")],
        );
        let replaced = identity_of(&a);
        invalidate_aliases(&store, &replaced, None);
        assert_eq!(store.source_path_of(&tokens[0]), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn finding7_saving_over_an_existing_file_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        for mode in [0o600_u32, 0o640, 0o644, 0o444] {
            let target = dir.join(format!("m{mode:o}.pdf"));
            fs::write(&target, b"old").expect("old");
            fs::set_permissions(&target, fs::Permissions::from_mode(mode)).expect("chmod");
            write_atomic(&target, b"new").expect("write");
            assert_eq!(fs::read(&target).expect("read"), b"new");
            let now = fs::metadata(&target).expect("meta").permissions().mode() & 0o7777;
            assert_eq!(now, mode, "mode {mode:o} survives the replacement");
        }
        assert_eq!(
            temps_in(&dir),
            Vec::<String>::new(),
            "no temp sibling is left"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn finding7_a_private_pdf_stays_private_through_a_staged_commit_and_through_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode-commit");
        let real = dir.join("A.pdf");
        let link = dir.join("L.pdf");
        let source = build_pdf(b"BT /F1 12 Tf 72 700 Td (Source) Tj ET");
        fs::write(&real, &source).expect("write source");
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).expect("chmod");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let candidate = build_pdf(b"BT /F1 12 Tf 72 700 Td (Edited) Tj ET");
        let store = PdfSaveTokenStore::default();
        let tokens = stage_all(&store, &real, &source, std::slice::from_ref(&candidate));
        commit_pdf_save(&store, &tokens[0], Some(&link)).expect("commit through the link");
        assert_eq!(fs::read(&real).expect("read"), candidate);
        assert_eq!(
            fs::metadata(&real).expect("meta").permissions().mode() & 0o7777,
            0o600
        );
        assert!(fs::symlink_metadata(&link)
            .expect("link")
            .file_type()
            .is_symlink());

        // A direct write_atomic through the link is resolved too.
        write_atomic(&link, b"direct").expect("direct");
        assert_eq!(fs::read(&real).expect("read"), b"direct");
        assert_eq!(
            fs::metadata(&real).expect("meta").permissions().mode() & 0o7777,
            0o600
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finding7_a_new_destination_is_created_and_leaves_no_temp() {
        let dir = scratch("fresh");
        let target = dir.join("new.pdf");
        write_atomic(&target, b"fresh").expect("create");
        assert_eq!(fs::read(&target).expect("read"), b"fresh");
        assert_eq!(temps_in(&dir), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_destination_that_is_a_directory_is_refused_and_left_alone() {
        let dir = scratch("dir-destination");
        let target = dir.join("folder.pdf");
        fs::create_dir(&target).expect("directory");
        let error = write_atomic(&target, b"x").expect_err("a directory is not replaced");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
        assert!(target.is_dir());
        assert_eq!(temps_in(&dir), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finding13_a_publish_that_fails_leaves_the_original_and_no_temp() {
        let dir = scratch("failed-publish");
        let target = dir.join("memo.pdf");
        let source = build_pdf(b"BT /F1 12 Tf 72 700 Td (Source) Tj ET");
        fs::write(&target, &source).expect("write source");
        let candidates = [build_pdf(b"BT /F1 12 Tf 72 700 Td (Edited) Tj ET")];
        let store = PdfSaveTokenStore::default();
        let tokens = stage_all(&store, &target, &source, &candidates);

        // The seam stands in for the replace call of the one writer; the failure arrives exactly
        // as a failed rename would.
        let outcome = {
            let _fail = rightkit_fs::atomic::testing::on_replace(|_| {
                Some(std::io::Error::other("injected replace failure"))
            });
            commit_pdf_save(&store, &tokens[0], None)
        };
        assert!(matches!(outcome, Err(CommitError::Io(_))), "{outcome:?}");
        assert_eq!(
            fs::read(&target).expect("read"),
            source,
            "the original survives a failed publish"
        );
        assert_eq!(temps_in(&dir), Vec::<String>::new(), "and no temp is left");

        // Nothing changed, so a fresh token commits.
        let again = stage_all(&store, &target, &source, &candidates);
        commit_pdf_save(&store, &again[0], None).expect("the retry publishes");
        assert_eq!(fs::read(&target).expect("read"), candidates[0]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn finding13_a_failure_after_the_replacement_is_reported_as_replaced_and_still_invalidates() {
        let dir = scratch("replaced-then-failed");
        let target = dir.join("memo.pdf");
        let source = build_pdf(b"BT /F1 12 Tf 72 700 Td (Source) Tj ET");
        fs::write(&target, &source).expect("write source");
        let candidates = [
            build_pdf(b"BT /F1 12 Tf 72 700 Td (First) Tj ET"),
            build_pdf(b"BT /F1 12 Tf 72 700 Td (Second) Tj ET"),
        ];
        let store = PdfSaveTokenStore::default();
        let tokens = stage_all(&store, &target, &source, &candidates);

        let outcome = {
            let _fail = rightkit_fs::atomic::testing::fail_dir_sync(std::io::ErrorKind::Other);
            commit_pdf_save(&store, &tokens[0], None)
        };
        assert!(
            matches!(outcome, Err(CommitError::Replaced(_))),
            "{outcome:?}"
        );
        assert_eq!(
            fs::read(&target).expect("read"),
            candidates[0],
            "the new bytes are in place"
        );
        let second = commit_pdf_save(&store, &tokens[1], None);
        assert!(
            matches!(second, Err(CommitError::Edit(PdfEditError::WriteConflict))),
            "{second:?}"
        );
        assert_eq!(fs::read(&target).expect("read"), candidates[0]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn finding7_a_replacement_with_other_permission_bits_is_not_the_same_protection() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("protection");
        let (old, new) = (dir.join("old.pdf"), dir.join("new.pdf"));
        fs::write(&old, b"x").expect("old");
        fs::write(&new, b"y").expect("new");
        fs::set_permissions(&old, fs::Permissions::from_mode(0o640)).expect("chmod");
        fs::set_permissions(&new, fs::Permissions::from_mode(0o600)).expect("chmod");
        let (old_meta, new_meta) = (
            fs::metadata(&old).expect("meta"),
            fs::metadata(&new).expect("meta"),
        );
        assert_eq!(
            same_protection(&old_meta, &new_meta),
            Err(Unpreserved::Mode)
        );
        fs::set_permissions(&new, fs::Permissions::from_mode(0o640)).expect("chmod");
        let new_meta = fs::metadata(&new).expect("meta");
        assert_eq!(same_protection(&old_meta, &new_meta), Ok(()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn finding7_an_owner_only_file_with_an_acl_is_refused_and_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("acl");
        let target = dir.join("private.pdf");
        fs::write(&target, b"old").expect("old");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("chmod");
        let status = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow read")
            .arg(&target)
            .status()
            .expect("chmod +a runs");
        assert!(status.success(), "chmod +a failed");
        assert!(
            !rightkit_fs::private::is_owner_only(&target).expect("reads the ACL"),
            "precondition: the volume holds ACLs and rightkit-fs sees this one"
        );

        let error = write_atomic(&target, b"new").expect_err("refused");
        let refusal = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<CommitError>());
        assert!(
            matches!(refusal, Some(CommitError::Refused(Unpreserved::Acl))),
            "{error:?}"
        );
        assert_eq!(fs::read(&target).expect("read"), b"old");
        assert_eq!(temps_in(&dir), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    #[allow(clippy::permissions_set_readonly_false)]
    fn finding7_a_read_only_file_is_refused_and_left_alone() {
        let dir = scratch("readonly");
        let target = dir.join("locked.pdf");
        fs::write(&target, b"old").expect("old");
        let mut permissions = fs::metadata(&target).expect("meta").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&target, permissions).expect("read-only");

        let error = write_atomic(&target, b"new").expect_err("refused");
        let refusal = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<CommitError>());
        assert!(
            matches!(refusal, Some(CommitError::Refused(Unpreserved::ReadOnly))),
            "{error:?}"
        );
        assert_eq!(fs::read(&target).expect("read"), b"old");
        assert_eq!(temps_in(&dir), Vec::<String>::new());

        let mut permissions = fs::metadata(&target).expect("meta").permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&target, permissions).expect("writable again");
        let _ = fs::remove_dir_all(&dir);
    }
}
