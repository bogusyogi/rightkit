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
