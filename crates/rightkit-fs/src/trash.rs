//! Move to the trash, find it again by identity, put it back (macOS, Windows and Linux).
//!
//! - [`move_to_trash`]: send a file, directory or symlink to the Trash (macOS), the Recycle Bin
//!   (Windows) or the freedesktop.org trash (Linux) and return a [`TrashIdentity`], a small value an
//!   app can persist.
//! - [`lookup`] and [`list`]: find what is in the trash, by identity or all of it.
//! - [`restore`]: move an item back to the path it came from, never over something that is there.
//!
//! The identity is the item's [`FileKey`] (device and inode, or volume serial and file index, which a
//! move within one volume preserves), its original absolute path, and the time it was trashed. A
//! lookup requires all three to agree (the trashed time within a window), because a bare inode number
//! can be reused by an unrelated file after the original is gone, and a bare path can be reused by a
//! new file.
//!
//! # macOS
//!
//! `FSMoveObjectToTrashSync` (CoreServices; the call Finder's "Move to Trash" is built on, without
//! Finder or a UI), hand-declared like the crate's other system calls. It picks the right trash for the
//! item's volume (`~/.Trash`, or `/Volumes/<name>/.Trashes/<uid>`) and renames on collision. The trashed
//! path comes back from the same API. A symlink is trashed as the link, not its target. Finder's
//! "Put Back" records live in `.DS_Store`, which this module neither reads nor writes: an item this
//! module trashes is restored by [`restore`], and Finder may or may not offer "Put Back" for it.
//! [`list`] covers `~/.Trash` and every mounted volume's `.Trashes/<uid>`; it cannot know an entry's
//! original path (`original` is `None`).
//!
//! # Windows
//!
//! [`move_to_trash`] uses `IFileOperation` (hand-declared COM, `ole32` and `shell32`) with
//! `FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE` and no UI. `FOFX_RECYCLEONDELETE` makes the operation
//! fail instead of deleting permanently when the item cannot be recycled (too large for the bin,
//! the bin disabled for the volume), which is why the older `SHFileOperationW` route was not used.
//! After the operation the Recycle Bin records are searched for the item's [`FileKey`]; if none
//! holds it the call returns an error that says the item left its place without a record (it was
//! not found in the bin), rather than reporting a trash that cannot be restored. Only a path on a
//! fixed drive with a letter is moved (no UNC, network or removable drive): anything else is
//! [`FsError::Unavailable`] before the item is touched. **This path has not been run on a Windows
//! host from this tree**; the device run listed in the changelog must pass before relying on it.
//!
//! [`lookup`], [`list`] and [`restore`] still work for items already in the bin: the shell keeps each
//! item as a pair in `<drive>:\$Recycle.Bin\<user SID>\`: `$I<id>` (a small record: original path,
//! size, deletion time) and `$R<id>` (the item itself). They read those records (format versions 1
//! and 2) and move `$R<id>` back; they never write a record. A UNC path, a network or removable
//! drive and a path that is not drive-lettered are refused with [`FsError::Unavailable`].
//!
//! # Linux and other freedesktop.org systems
//!
//! The trash specification 1.0: the home trash `$XDG_DATA_HOME/Trash` (default
//! `~/.local/share/Trash`) with `files/` and `info/`, for an item on the same filesystem as it;
//! otherwise the per-volume trash `<topdir>/.Trash/<uid>` (when `<topdir>/.Trash` is a real
//! sticky directory) or `<topdir>/.Trash-<uid>`, with `Path` recorded relative to the top
//! directory. The name is claimed by creating its `.trashinfo` file exclusively, then the item is
//! moved with an exclusive rename, so nothing in the trash is ever overwritten. An item is never
//! copied across filesystems: when no usable trash exists on its filesystem the move is refused
//! and the item stays. [`list`] covers the home trash and the volume trashes of the mounts named in
//! `/proc/self/mounts`; unlike macOS it knows each entry's original path. `directorysizes` is not
//! maintained. Android, iOS and other targets return [`FsError::Unavailable`].
//!
//! The Swift counterpart ("RightKitSwift") is a separate repository and is not part of this crate.

use crate::error::{FsError, Result};
use crate::file_key::FileKey;
use std::path::{Path, PathBuf};

/// How long before the recorded trashing time an entry's own timestamp may still count as the same
/// trashing (clock steps, coarse timestamps). 60 s; an unrelated entry that reused an inode is older
/// than that unless it was trashed in the same minute.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const WINDOW_MS: u64 = 60_000;

/// The durable handle of a trashed item.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(serde::Serialize, serde::Deserialize))]
pub struct TrashIdentity {
    /// The absolute path the item had before it was trashed (and where [`restore`] puts it back).
    pub original: PathBuf,
    /// The item's filesystem identity (the symlink's own for a symlink).
    pub key: FileKey,
    /// When it was trashed, milliseconds since the Unix epoch.
    pub trashed_unix_ms: u64,
    /// Where the trash put it, as a hint for a fast lookup; it can be stale (the trash was emptied or
    /// the entry renamed), so [`lookup`] checks it and falls back to scanning.
    pub trashed_as: Option<PathBuf>,
}

/// One item found in the trash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrashEntry {
    /// Where the item is now (inside the trash).
    pub path: PathBuf,
    /// Its filesystem identity.
    pub key: FileKey,
    /// The path it came from, when the platform records it (Windows). `None` on macOS.
    pub original: Option<PathBuf>,
    /// When it was trashed: the Recycle Bin record's time (Windows) or the entry's change time
    /// (macOS), milliseconds since the Unix epoch.
    pub trashed_unix_ms: u64,
}

#[cfg_attr(
    any(
        target_os = "macos",
        windows,
        all(unix, not(any(target_os = "ios", target_os = "android")))
    ),
    allow(dead_code)
)]
fn unavailable() -> FsError {
    FsError::Unavailable("trash is available on macOS, Windows and Linux only")
}

/// Send `path` to the trash and return its identity. `path` must exist (a symlink is trashed itself,
/// never its target), be absolute or is made absolute against the current directory, and must not
/// hold a NUL byte. The item leaves `path`; on error it is where it was. On Windows this always
/// returns [`FsError::Unavailable`] without touching the item (see the module docs).
pub fn move_to_trash(path: &Path) -> Result<TrashIdentity> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(FsError::BadName(path.display().to_string()));
    }
    let original = std::path::absolute(path).map_err(|e| FsError::io(path, e))?;
    let key = sys::key_nofollow(&original)?;
    let before_ms = now_ms();
    let moved = sys::move_item(&original, key)?;
    Ok(TrashIdentity {
        original,
        key,
        trashed_unix_ms: moved.trashed_unix_ms.unwrap_or(before_ms),
        trashed_as: moved.trashed_as,
    })
}

/// Every item in the trash this module can see (see the platform sections of the module docs). Order
/// is unspecified. A trash that does not exist is an empty list, not an error.
pub fn list() -> Result<Vec<TrashEntry>> {
    sys::list()
}

/// The trashed item `identity` names, or `None` when it is no longer there (restored, emptied, or
/// never matched). Requires the key and the trashing time to agree (and on Windows the original
/// path), see the module docs.
pub fn lookup(identity: &TrashIdentity) -> Result<Option<TrashEntry>> {
    sys::lookup(identity)
}

/// Put the item `identity` names back at `identity.original` and return that path.
///
/// The item is found with [`lookup`] ([`FsError::Io`] with `NotFound` when it is not in the trash) and
/// moved with an exclusive rename: if something exists at the original path the result is
/// [`FsError::AlreadyExists`] and the item stays in the trash, so nothing is ever overwritten. The
/// original's parent directory must exist (it is not recreated). Windows then removes the Recycle Bin
/// record of the item on a best-effort basis (a leftover record without its data is ignored by
/// Explorer).
pub fn restore(identity: &TrashIdentity) -> Result<PathBuf> {
    let entry = lookup(identity)?.ok_or_else(|| {
        FsError::io(
            &identity.original,
            std::io::Error::new(std::io::ErrorKind::NotFound, "the item is not in the trash"),
        )
    })?;
    sys::restore(&entry, identity)?;
    Ok(identity.original.clone())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What a platform's move reports: where the item went and, when the platform records it, when.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Moved {
    trashed_as: Option<PathBuf>,
    trashed_unix_ms: Option<u64>,
}

/// Whether an entry's own timestamp is consistent with the trashing `identity_ms` it should belong
/// to: not older than the window before it. (An entry can be touched after trashing, so there is no
/// upper bound.)
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn entry_time_matches(entry_ms: u64, identity_ms: u64) -> bool {
    entry_ms.saturating_add(WINDOW_MS) >= identity_ms
}

/// FILETIME (100 ns ticks since 1601-01-01) to milliseconds since the Unix epoch, saturating at 0.
#[cfg_attr(not(windows), allow(dead_code))]
fn filetime_to_unix_ms(filetime: u64) -> u64 {
    const EPOCH_DIFFERENCE_MS: u64 = 11_644_473_600_000;
    (filetime / 10_000).saturating_sub(EPOCH_DIFFERENCE_MS)
}

/// One Recycle Bin `$I` record.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
struct RecycleInfo {
    #[allow(dead_code)]
    size: u64,
    deleted_unix_ms: u64,
    original: String,
}

/// Parse a Recycle Bin `$I` record. Version 1 (Vista to 8.1) is 544 bytes with a fixed 520-byte path
/// field; version 2 (Windows 10 and later) stores the path length in characters (with its NUL) and
/// then the path. Every length is bounds-checked; anything malformed is `None`.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_recycle_info(bytes: &[u8]) -> Option<RecycleInfo> {
    let u64_at = |at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
    };
    let version = u64_at(0)?;
    let size = u64_at(8)?;
    let deleted = u64_at(16)?;
    let path_bytes: &[u8] = match version {
        1 => {
            let rest = bytes.get(24..)?;
            &rest[..rest.len().min(520)]
        }
        2 => {
            let chars = u32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?) as usize;
            if chars == 0 || chars > 32_768 + 1 {
                return None;
            }
            bytes.get(28..28usize.checked_add(chars.checked_mul(2)?)?)?
        }
        _ => return None,
    };
    let units: Vec<u16> = path_bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    if units.is_empty() {
        return None;
    }
    Some(RecycleInfo {
        size,
        deleted_unix_ms: filetime_to_unix_ms(deleted),
        original: String::from_utf16(&units).ok()?,
    })
}

#[cfg(target_os = "macos")]
mod sys {
    use super::{entry_time_matches, FileKey, FsError, Moved, Result, TrashEntry, TrashIdentity};
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    /// `FSRef`, an opaque 80-byte record.
    #[repr(C)]
    struct FsRef {
        #[allow(dead_code)]
        hidden: [u8; 80],
    }

    /// `kFSPathMakeRefDoNotFollowLeafSymlink`: a symlink is trashed as the link.
    const DO_NOT_FOLLOW_LEAF_SYMLINK: u32 = 0x1;
    /// `kFSFileOperationDefaultOptions`.
    const DEFAULT_OPTIONS: u32 = 0;
    const NO_ERR: i32 = 0;

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn FSPathMakeRefWithOptions(
            path: *const u8,
            options: u32,
            fs_ref: *mut FsRef,
            is_directory: *mut u8,
        ) -> i32;
        fn FSMoveObjectToTrashSync(source: *const FsRef, target: *mut FsRef, options: u32) -> i32;
        fn FSRefMakePath(fs_ref: *const FsRef, path: *mut u8, max_size: u32) -> i32;
    }

    pub fn key_nofollow(path: &Path) -> Result<FileKey> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| FsError::io(path, e))?;
        Ok(FileKey {
            volume: meta.dev(),
            index: meta.ino(),
        })
    }

    fn os_status(path: &Path, call: &str, status: i32) -> FsError {
        FsError::io(
            path,
            std::io::Error::other(format!("{call} failed with OSStatus {status}")),
        )
    }

    pub fn move_item(original: &Path, _key: FileKey) -> Result<Moved> {
        let mut c_path = original.as_os_str().as_bytes().to_vec();
        c_path.push(0);
        let mut source = FsRef { hidden: [0; 80] };
        let mut target = FsRef { hidden: [0; 80] };
        // SAFETY: `c_path` is NUL-terminated and outlives the call; `source` is an out-parameter of
        // the 80-byte FSRef size; the directory flag out-parameter is optional (null).
        let status = unsafe {
            FSPathMakeRefWithOptions(
                c_path.as_ptr(),
                DO_NOT_FOLLOW_LEAF_SYMLINK,
                &mut source,
                std::ptr::null_mut(),
            )
        };
        if status != NO_ERR {
            return Err(os_status(original, "FSPathMakeRefWithOptions", status));
        }
        // SAFETY: `source` was filled by the call above; `target` is a valid out-parameter.
        let status = unsafe { FSMoveObjectToTrashSync(&source, &mut target, DEFAULT_OPTIONS) };
        if status != NO_ERR {
            return Err(os_status(original, "FSMoveObjectToTrashSync", status));
        }
        let mut buffer = vec![0u8; 4096];
        // SAFETY: `target` was filled by the move; `buffer` is writable for its length.
        let status = unsafe { FSRefMakePath(&target, buffer.as_mut_ptr(), buffer.len() as u32) };
        let trashed_as = if status == NO_ERR {
            let len = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
            buffer.truncate(len);
            Some(PathBuf::from(OsString::from_vec(buffer)))
        } else {
            None
        };
        Ok(Moved {
            trashed_as,
            trashed_unix_ms: None,
        })
    }

    fn change_time_ms(meta: &std::fs::Metadata) -> u64 {
        (meta.ctime().max(0) as u64) * 1000 + (meta.ctime_nsec().max(0) as u64) / 1_000_000
    }

    /// The directories that hold the current user's trash: `~/.Trash` and each mounted volume's
    /// `.Trashes/<uid>`. With `volume_of`, only the trash of that path's volume and the home trash.
    fn trash_dirs(volume_of: Option<&Path>) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            dirs.push(PathBuf::from(home).join(".Trash"));
        }
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let volumes = Path::new("/Volumes");
        match volume_of {
            Some(path) => {
                if let Ok(rest) = path.strip_prefix(volumes) {
                    if let Some(name) = rest.components().next() {
                        dirs.push(
                            volumes
                                .join(name.as_os_str())
                                .join(".Trashes")
                                .join(uid.to_string()),
                        );
                    }
                }
            }
            None => {
                if let Ok(entries) = std::fs::read_dir(volumes) {
                    for entry in entries.flatten() {
                        dirs.push(entry.path().join(".Trashes").join(uid.to_string()));
                    }
                }
            }
        }
        dirs
    }

    fn entries_in(dir: &Path) -> Vec<TrashEntry> {
        let Ok(read) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in read.flatten() {
            if entry.file_name() == ".DS_Store" {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            out.push(TrashEntry {
                path,
                key: FileKey {
                    volume: meta.dev(),
                    index: meta.ino(),
                },
                original: None,
                trashed_unix_ms: change_time_ms(&meta),
            });
        }
        out
    }

    pub fn list() -> Result<Vec<TrashEntry>> {
        let mut all = Vec::new();
        for dir in trash_dirs(None) {
            all.extend(entries_in(&dir));
        }
        Ok(all)
    }

    pub fn lookup(identity: &TrashIdentity) -> Result<Option<TrashEntry>> {
        let matches = |entry: &TrashEntry| {
            entry.key == identity.key
                && entry_time_matches(entry.trashed_unix_ms, identity.trashed_unix_ms)
        };
        if let Some(hint) = &identity.trashed_as {
            if let Some(parent) = hint.parent() {
                if let Some(found) = entries_in(parent)
                    .into_iter()
                    .find(|e| e.path == *hint && matches(e))
                {
                    return Ok(Some(found));
                }
            }
        }
        for dir in trash_dirs(Some(&identity.original)) {
            if let Some(found) = entries_in(&dir).into_iter().find(|e| matches(e)) {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    pub fn restore(entry: &TrashEntry, identity: &TrashIdentity) -> Result<()> {
        crate::atomic::rename_exclusive(&entry.path, &identity.original)
    }
}

#[cfg(windows)]
mod sys {
    use super::{parse_recycle_info, FileKey, FsError, Moved, Result, TrashEntry, TrashIdentity};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Component, Path, PathBuf, Prefix};

    const DRIVE_FIXED: u32 = 3;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x7;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    /// Largest `$I` record read: a version 2 record with a 32 768-character path is under 66 KiB.
    const MAX_RECORD_BYTES: u64 = 128 * 1024;
    /// How far apart two readings of one deletion time may be.
    const TIME_TOLERANCE_MS: u64 = 2_000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDriveTypeW(root: *const u16) -> u32;
        fn GetLogicalDrives() -> u32;
    }

    pub fn key_nofollow(path: &Path) -> Result<FileKey> {
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ_WRITE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|e| FsError::io(path, e))?;
        FileKey::of_file(&file, path)
    }

    /// The drive letter of a plain `X:\...` path, or why the trash cannot be used for the path.
    fn drive_of(path: &Path) -> Result<char> {
        let plain = crate::plain::plain_path(path.to_path_buf());
        match plain.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) => Ok(char::from(letter).to_ascii_uppercase()),
                _ => Err(FsError::Unavailable(
                    "the Recycle Bin needs a path on a drive with a letter (no UNC or device paths)",
                )),
            },
            _ => Err(FsError::Unavailable("the Recycle Bin needs an absolute drive path")),
        }
    }

    fn root_wide(drive: char) -> Vec<u16> {
        format!("{drive}:\\")
            .encode_utf16()
            .chain(Some(0))
            .collect()
    }

    fn require_fixed_drive(drive: char) -> Result<()> {
        // SAFETY: NUL-terminated root path.
        let kind = unsafe { GetDriveTypeW(root_wide(drive).as_ptr()) };
        if kind == DRIVE_FIXED {
            Ok(())
        } else {
            Err(FsError::Unavailable(
                "the Recycle Bin is only used for fixed drives (not network or removable ones)",
            ))
        }
    }

    fn bin_dir(drive: char) -> Result<PathBuf> {
        let probe = PathBuf::from(format!("{drive}:\\"));
        let sid = crate::private::current_process_sid_string(&probe)?;
        Ok(PathBuf::from(format!("{drive}:\\$Recycle.Bin\\{sid}")))
    }

    struct Record {
        data: PathBuf,
        key: FileKey,
        original: String,
        deleted_unix_ms: u64,
    }

    fn records(drive: char) -> Result<Vec<Record>> {
        let bin = bin_dir(drive)?;
        let read = match std::fs::read_dir(&bin) {
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(FsError::io(&bin, e)),
        };
        let mut out = Vec::new();
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_prefix("$I") else {
                continue;
            };
            let info = entry.path();
            match std::fs::metadata(&info) {
                Ok(meta) if meta.is_file() && meta.len() <= MAX_RECORD_BYTES => {}
                _ => continue,
            }
            let Ok(bytes) = std::fs::read(&info) else {
                continue;
            };
            let Some(parsed) = parse_recycle_info(&bytes) else {
                continue;
            };
            let data = bin.join(format!("$R{id}"));
            let Ok(key) = key_nofollow(&data) else {
                continue; // a record whose data is gone
            };
            out.push(Record {
                data,
                key,
                original: parsed.original,
                deleted_unix_ms: parsed.deleted_unix_ms,
            });
        }
        Ok(out)
    }

    fn entry_of(record: Record) -> TrashEntry {
        TrashEntry {
            path: record.data,
            key: record.key,
            original: Some(PathBuf::from(record.original)),
            trashed_unix_ms: record.deleted_unix_ms,
        }
    }

    fn same_path(a: &str, b: &Path) -> bool {
        a.to_lowercase() == b.to_string_lossy().to_lowercase()
    }

    /// Move to the Recycle Bin with `IFileOperation` and `FOFX_RECYCLEONDELETE` (fails instead of
    /// deleting permanently), then confirm a bin record holds the item.
    pub fn move_item(original: &Path, key: FileKey) -> Result<Moved> {
        let drive = drive_of(original)?;
        require_fixed_drive(drive)?;
        com::recycle_only(original)?;
        match records(drive)?.into_iter().find(|r| r.key == key) {
            Some(record) => Ok(Moved {
                trashed_as: Some(record.data),
                trashed_unix_ms: Some(record.deleted_unix_ms),
            }),
            None => Err(FsError::io(
                original,
                io::Error::other(
                    "the item left its place but no Recycle Bin record with its identity was found",
                ),
            )),
        }
    }

    /// Hand-declared `IFileOperation` (shobjidl_core.h). Vtable slots counted from `IUnknown`
    /// (0 QueryInterface, 1 AddRef, 2 Release): 3 Advise, 4 Unadvise, 5 SetOperationFlags,
    /// 6 SetProgressMessage, 7 SetProgressDialog, 8 SetProperties, 9 SetOwnerWindow,
    /// 10 ApplyPropertiesToItem, 11 ApplyPropertiesToItems, 12 RenameItem, 13 RenameItems,
    /// 14 MoveItem, 15 MoveItems, 16 CopyItem, 17 CopyItems, 18 DeleteItem, 19 DeleteItems,
    /// 20 NewItem, 21 PerformOperations, 22 GetAnyOperationsAborted.
    mod com {
        use super::super::FsError;
        use super::Result;
        use std::ffi::c_void;
        use std::io;
        use std::os::windows::ffi::OsStrExt;
        use std::path::Path;

        #[repr(C)]
        struct Guid {
            data1: u32,
            data2: u16,
            data3: u16,
            data4: [u8; 8],
        }

        /// `CLSID_FileOperation` {3AD05575-8857-4850-9277-11B85BDB8E09}.
        static CLSID_FILE_OPERATION: Guid = Guid {
            data1: 0x3AD0_5575,
            data2: 0x8857,
            data3: 0x4850,
            data4: [0x92, 0x77, 0x11, 0xB8, 0x5B, 0xDB, 0x8E, 0x09],
        };
        /// `IID_IFileOperation` {947AAB5F-0A5C-4C13-B4D6-4BF7836FC9F8}.
        static IID_FILE_OPERATION: Guid = Guid {
            data1: 0x947A_AB5F,
            data2: 0x0A5C,
            data3: 0x4C13,
            data4: [0xB4, 0xD6, 0x4B, 0xF7, 0x83, 0x6F, 0xC9, 0xF8],
        };
        /// `IID_IShellItem` {43826D1E-E718-42EE-BC55-A1E261C37BFE}.
        static IID_SHELL_ITEM: Guid = Guid {
            data1: 0x4382_6D1E,
            data2: 0xE718,
            data3: 0x42EE,
            data4: [0xBC, 0x55, 0xA1, 0xE2, 0x61, 0xC3, 0x7B, 0xFE],
        };

        const COINIT_APARTMENTTHREADED: u32 = 0x2;
        const CLSCTX_INPROC_SERVER: u32 = 0x1;
        const RPC_E_CHANGED_MODE: i32 = 0x8001_0106_u32 as i32;
        const FOF_SILENT: u32 = 0x0004;
        const FOF_NOCONFIRMATION: u32 = 0x0010;
        const FOF_ALLOWUNDO: u32 = 0x0040;
        const FOF_NOCONFIRMMKDIR: u32 = 0x0200;
        const FOF_NOERRORUI: u32 = 0x0400;
        const FOFX_RECYCLEONDELETE: u32 = 0x0008_0000;
        const FLAGS: u32 = FOF_SILENT
            | FOF_NOCONFIRMATION
            | FOF_NOCONFIRMMKDIR
            | FOF_NOERRORUI
            | FOF_ALLOWUNDO
            | FOFX_RECYCLEONDELETE;

        const SLOT_RELEASE: usize = 2;
        const SLOT_SET_OPERATION_FLAGS: usize = 5;
        const SLOT_DELETE_ITEM: usize = 18;
        const SLOT_PERFORM_OPERATIONS: usize = 21;
        const SLOT_GET_ANY_OPERATIONS_ABORTED: usize = 22;

        #[link(name = "ole32")]
        unsafe extern "system" {
            fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
            fn CoUninitialize();
            fn CoCreateInstance(
                clsid: *const Guid,
                outer: *mut c_void,
                context: u32,
                iid: *const Guid,
                out: *mut *mut c_void,
            ) -> i32;
        }

        #[link(name = "shell32")]
        unsafe extern "system" {
            fn SHCreateItemFromParsingName(
                path: *const u16,
                bind: *mut c_void,
                iid: *const Guid,
                out: *mut *mut c_void,
            ) -> i32;
        }

        /// The function pointer in vtable `slot` of COM object `object`.
        unsafe fn slot(object: *mut c_void, index: usize) -> *const c_void {
            // SAFETY: a COM object starts with a pointer to its vtable of function pointers.
            unsafe {
                let vtable = *(object as *const *const *const c_void);
                *vtable.add(index)
            }
        }

        struct Com(*mut c_void);
        impl Drop for Com {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    // SAFETY: an owned COM reference, released once.
                    unsafe {
                        let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                            std::mem::transmute(slot(self.0, SLOT_RELEASE));
                        release(self.0);
                    }
                }
            }
        }

        struct Apartment(bool);
        impl Drop for Apartment {
            fn drop(&mut self) {
                if self.0 {
                    // SAFETY: balances a successful CoInitializeEx on this thread.
                    unsafe { CoUninitialize() };
                }
            }
        }

        fn failed(path: &Path, call: &str, hr: i32) -> FsError {
            FsError::io(
                path,
                io::Error::other(format!("{call} failed with HRESULT {:#010x}", hr as u32)),
            )
        }

        pub fn recycle_only(path: &Path) -> Result<()> {
            let plain = crate::plain::plain_path(path.to_path_buf());
            let wide: Vec<u16> = plain
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: COM calls follow the documented signatures; every owned interface is
            // wrapped in a guard that releases it, the apartment guard only balances a success.
            unsafe {
                let hr = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
                if hr < 0 && hr != RPC_E_CHANGED_MODE {
                    return Err(failed(path, "CoInitializeEx", hr));
                }
                let _apartment = Apartment(hr >= 0);
                let mut raw: *mut c_void = std::ptr::null_mut();
                let hr = CoCreateInstance(
                    &CLSID_FILE_OPERATION,
                    std::ptr::null_mut(),
                    CLSCTX_INPROC_SERVER,
                    &IID_FILE_OPERATION,
                    &mut raw,
                );
                if hr < 0 || raw.is_null() {
                    return Err(failed(path, "CoCreateInstance(FileOperation)", hr));
                }
                let operation = Com(raw);
                let set_flags: unsafe extern "system" fn(*mut c_void, u32) -> i32 =
                    std::mem::transmute(slot(operation.0, SLOT_SET_OPERATION_FLAGS));
                let hr = set_flags(operation.0, FLAGS);
                if hr < 0 {
                    return Err(failed(path, "SetOperationFlags", hr));
                }
                let mut item_raw: *mut c_void = std::ptr::null_mut();
                let hr = SHCreateItemFromParsingName(
                    wide.as_ptr(),
                    std::ptr::null_mut(),
                    &IID_SHELL_ITEM,
                    &mut item_raw,
                );
                if hr < 0 || item_raw.is_null() {
                    return Err(failed(path, "SHCreateItemFromParsingName", hr));
                }
                let item = Com(item_raw);
                let delete_item: unsafe extern "system" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                ) -> i32 = std::mem::transmute(slot(operation.0, SLOT_DELETE_ITEM));
                let hr = delete_item(operation.0, item.0, std::ptr::null_mut());
                if hr < 0 {
                    return Err(failed(path, "DeleteItem", hr));
                }
                let perform: unsafe extern "system" fn(*mut c_void) -> i32 =
                    std::mem::transmute(slot(operation.0, SLOT_PERFORM_OPERATIONS));
                let hr = perform(operation.0);
                if hr < 0 {
                    return Err(failed(path, "PerformOperations", hr));
                }
                let aborted_fn: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32 =
                    std::mem::transmute(slot(operation.0, SLOT_GET_ANY_OPERATIONS_ABORTED));
                let mut aborted = 0i32;
                let hr = aborted_fn(operation.0, &mut aborted);
                if hr < 0 || aborted != 0 {
                    return Err(failed(path, "the recycle operation was aborted", hr));
                }
            }
            Ok(())
        }
    }

    pub fn list() -> Result<Vec<TrashEntry>> {
        let mut all = Vec::new();
        // SAFETY: no arguments.
        let mask = unsafe { GetLogicalDrives() };
        for index in 0..26u32 {
            if mask & (1 << index) == 0 {
                continue;
            }
            let drive = char::from(b'A' + index as u8);
            if require_fixed_drive(drive).is_err() {
                continue;
            }
            all.extend(records(drive)?.into_iter().map(entry_of));
        }
        Ok(all)
    }

    pub fn lookup(identity: &TrashIdentity) -> Result<Option<TrashEntry>> {
        let drive = drive_of(&identity.original)?;
        let found = records(drive)?.into_iter().find(|r| {
            r.key == identity.key
                && same_path(&r.original, &identity.original)
                && r.deleted_unix_ms.abs_diff(identity.trashed_unix_ms) <= TIME_TOLERANCE_MS
        });
        Ok(found.map(entry_of))
    }

    pub fn restore(entry: &TrashEntry, identity: &TrashIdentity) -> Result<()> {
        crate::atomic::rename_exclusive(&entry.path, &identity.original)?;
        // Best effort: drop the `$I` record that described the item. Its name is the data name with
        // `$R` swapped for `$I`.
        if let Some(name) = entry.path.file_name().and_then(|n| n.to_str()) {
            if let Some(id) = name.strip_prefix("$R") {
                if let Some(dir) = entry.path.parent() {
                    let _ = std::fs::remove_file(dir.join(format!("$I{id}")));
                }
            }
        }
        Ok(())
    }
}

#[cfg(not(any(
    target_os = "macos",
    windows,
    all(unix, not(any(target_os = "ios", target_os = "android")))
)))]
mod sys {
    use super::{unavailable, FileKey, Moved, Result, TrashEntry, TrashIdentity};
    use std::path::Path;

    pub fn key_nofollow(_path: &Path) -> Result<FileKey> {
        Err(unavailable())
    }
    pub fn move_item(_original: &Path, _key: FileKey) -> Result<Moved> {
        Err(unavailable())
    }
    pub fn list() -> Result<Vec<TrashEntry>> {
        Err(unavailable())
    }
    pub fn lookup(_identity: &TrashIdentity) -> Result<Option<TrashEntry>> {
        Err(unavailable())
    }
    pub fn restore(_entry: &TrashEntry, _identity: &TrashIdentity) -> Result<()> {
        Err(unavailable())
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod sys {
    use super::{now_ms, FileKey, FsError, Moved, Result, TrashEntry, TrashIdentity};
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::{self, Write};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};

    /// How far apart two readings of one deletion time may be (the record has second resolution).
    const TIME_TOLERANCE_MS: u64 = 2_000;
    const INFO_SUFFIX: &str = ".trashinfo";
    const MAX_INFO_BYTES: u64 = 64 * 1024;

    /// One trash directory: `files/` and `info/`, and for a volume trash its top directory.
    struct Root {
        files: PathBuf,
        info: PathBuf,
        topdir: Option<PathBuf>,
    }

    impl Root {
        fn at(dir: &Path, topdir: Option<PathBuf>) -> Root {
            Root {
                files: dir.join("files"),
                info: dir.join("info"),
                topdir,
            }
        }
    }

    pub fn key_nofollow(path: &Path) -> Result<FileKey> {
        let meta = fs::symlink_metadata(path).map_err(|e| FsError::io(path, e))?;
        Ok(FileKey {
            volume: meta.dev(),
            index: meta.ino(),
        })
    }

    fn uid() -> u32 {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }

    fn home_trash_dir() -> Option<PathBuf> {
        let absolute = |v: OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
        let data = std::env::var_os("XDG_DATA_HOME")
            .and_then(absolute)
            .or_else(|| {
                std::env::var_os("HOME")
                    .and_then(absolute)
                    .map(|h| h.join(".local/share"))
            })?;
        Some(data.join("Trash"))
    }

    fn make_private_dir(path: &Path) -> Result<()> {
        match fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
        {
            Ok(()) => Ok(()),
            Err(e) => Err(FsError::io(path, e)),
        }
    }

    fn ensure_root(dir: &Path, topdir: Option<PathBuf>) -> Result<Root> {
        let root = Root::at(dir, topdir);
        make_private_dir(&root.files)?;
        make_private_dir(&root.info)?;
        Ok(root)
    }

    /// The trash for a volume other than the home trash's: `<top>/.Trash/<uid>` when `.Trash` is a
    /// real sticky directory, else `<top>/.Trash-<uid>`, both owned by the user.
    fn volume_trash(original: &Path, dev: u64) -> Result<Root> {
        let mut top = original
            .parent()
            .ok_or_else(|| FsError::BadName(original.display().to_string()))?
            .to_path_buf();
        while let Some(up) = top.parent() {
            match fs::metadata(up) {
                Ok(m) if m.dev() == dev => top = up.to_path_buf(),
                _ => break,
            }
        }
        let shared = top.join(".Trash");
        let dir = match fs::symlink_metadata(&shared) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() && m.mode() & 0o1000 != 0 => {
                shared.join(uid().to_string())
            }
            _ => top.join(format!(".Trash-{}", uid())),
        };
        if let Ok(m) = fs::symlink_metadata(&dir) {
            if m.file_type().is_symlink() || !m.is_dir() || m.uid() != uid() {
                return Err(FsError::Unavailable(
                    "the volume's trash directory is not a directory owned by this user",
                ));
            }
        }
        ensure_root(&dir, Some(top))
    }

    fn encode_path(path: &Path) -> String {
        let mut out = String::new();
        for &b in path.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
                out.push(char::from(b));
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    fn decode_path(text: &str) -> PathBuf {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        PathBuf::from(OsString::from_vec(out))
    }

    fn format_local(ms: u64) -> String {
        let secs = (ms / 1000) as libc::time_t;
        // SAFETY: zeroed tm is a valid out-parameter; localtime_r fills it.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&secs, &mut tm) };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }

    fn parse_local(text: &str) -> Option<u64> {
        let (date, time) = text.trim().split_once('T')?;
        let mut d = date.split('-').map(|p| p.parse::<i32>().ok());
        let mut t = time.split(':').map(|p| p.parse::<i32>().ok());
        let (year, month, day) = (d.next()??, d.next()??, d.next()??);
        let (hour, minute, second) = (t.next()??, t.next()??, t.next()??);
        // SAFETY: zeroed tm is valid; the fields mktime reads are set below.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = year - 1900;
        tm.tm_mon = month - 1;
        tm.tm_mday = day;
        tm.tm_hour = hour;
        tm.tm_min = minute;
        tm.tm_sec = second;
        tm.tm_isdst = -1;
        let secs = unsafe { libc::mktime(&mut tm) };
        (secs >= 0).then(|| secs as u64 * 1000)
    }

    pub fn move_item(original: &Path, _key: FileKey) -> Result<Moved> {
        let meta = fs::symlink_metadata(original).map_err(|e| FsError::io(original, e))?;
        let home = home_trash_dir().ok_or(FsError::Unavailable(
            "no home trash: neither XDG_DATA_HOME nor HOME is an absolute path",
        ))?;
        let home_root = ensure_root(&home, None)?;
        let home_dev = fs::metadata(&home_root.files)
            .map_err(|e| FsError::io(&home_root.files, e))?
            .dev();
        let root = if meta.dev() == home_dev {
            home_root
        } else {
            volume_trash(original, meta.dev())?
        };
        let name = original
            .file_name()
            .ok_or_else(|| FsError::BadName(original.display().to_string()))?;
        let recorded = match &root.topdir {
            Some(top) => original.strip_prefix(top).unwrap_or(original),
            None => original,
        };
        let deleted_ms = now_ms();
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            encode_path(recorded),
            format_local(deleted_ms)
        );
        for attempt in 0..10_000u32 {
            let mut candidate = OsString::from(name);
            if attempt > 0 {
                candidate.push(format!(".{attempt}"));
            }
            let mut info_name = candidate.clone();
            info_name.push(INFO_SUFFIX);
            let info_path = root.info.join(&info_name);
            let mut info = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&info_path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(FsError::io(&info_path, e)),
            };
            if let Err(e) = info
                .write_all(body.as_bytes())
                .and_then(|()| info.sync_all())
            {
                let _ = fs::remove_file(&info_path);
                return Err(FsError::io(&info_path, e));
            }
            drop(info);
            let target = root.files.join(&candidate);
            match crate::atomic::rename_exclusive(original, &target) {
                Ok(()) => {
                    return Ok(Moved {
                        trashed_as: Some(target),
                        trashed_unix_ms: Some(deleted_ms / 1000 * 1000),
                    })
                }
                Err(FsError::AlreadyExists(_)) => {
                    let _ = fs::remove_file(&info_path);
                    continue;
                }
                Err(e) => {
                    let _ = fs::remove_file(&info_path);
                    return Err(e);
                }
            }
        }
        Err(FsError::AlreadyExists(original.to_path_buf()))
    }

    fn unescape_mount(field: &str) -> PathBuf {
        let bytes = field.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 3 < bytes.len() {
                if let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 4])
                    .ok()
                    .and_then(|o| u8::from_str_radix(o, 8).ok())
                {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        PathBuf::from(OsStr::from_bytes(&out))
    }

    fn roots() -> Vec<Root> {
        let mut out = Vec::new();
        if let Some(home) = home_trash_dir() {
            out.push(Root::at(&home, None));
        }
        if let Ok(mounts) = fs::read_to_string("/proc/self/mounts") {
            for line in mounts.lines() {
                let Some(point) = line.split_whitespace().nth(1) else {
                    continue;
                };
                let top = unescape_mount(point);
                let shared = top.join(".Trash").join(uid().to_string());
                if shared.is_dir() {
                    out.push(Root::at(&shared, Some(top.clone())));
                }
                let own = top.join(format!(".Trash-{}", uid()));
                if own.is_dir() {
                    out.push(Root::at(&own, Some(top)));
                }
            }
        }
        out
    }

    fn entries_in(root: &Root) -> Vec<TrashEntry> {
        let Ok(read) = fs::read_dir(&root.info) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in read.flatten() {
            let file_name = entry.file_name();
            let Some(stem) = file_name.to_str().and_then(|n| n.strip_suffix(INFO_SUFFIX)) else {
                continue;
            };
            let data = root.files.join(stem);
            let Ok(data_meta) = fs::symlink_metadata(&data) else {
                continue;
            };
            let info_path = entry.path();
            let Ok(info_meta) = fs::metadata(&info_path) else {
                continue;
            };
            if info_meta.len() > MAX_INFO_BYTES {
                continue;
            }
            let Ok(text) = fs::read_to_string(&info_path) else {
                continue;
            };
            let mut original = None;
            let mut deleted = None;
            let mut in_section = false;
            for line in text.lines() {
                if line.starts_with('[') {
                    in_section = line.trim() == "[Trash Info]";
                } else if in_section {
                    if let Some(v) = line.strip_prefix("Path=") {
                        original.get_or_insert_with(|| decode_path(v));
                    } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                        deleted = deleted.or_else(|| parse_local(v));
                    }
                }
            }
            let original = original.map(|p| match (&root.topdir, p.is_absolute()) {
                (Some(top), false) => top.join(p),
                _ => p,
            });
            let trashed_unix_ms =
                deleted.unwrap_or_else(|| (info_meta.mtime().max(0) as u64) * 1000);
            out.push(TrashEntry {
                path: data,
                key: FileKey {
                    volume: data_meta.dev(),
                    index: data_meta.ino(),
                },
                original,
                trashed_unix_ms,
            });
        }
        out
    }

    pub fn list() -> Result<Vec<TrashEntry>> {
        Ok(roots().iter().flat_map(entries_in).collect())
    }

    pub fn lookup(identity: &TrashIdentity) -> Result<Option<TrashEntry>> {
        let all = list()?;
        if let Some(hint) = &identity.trashed_as {
            if let Some(found) = all
                .iter()
                .find(|e| e.path == *hint && e.key == identity.key)
            {
                return Ok(Some(found.clone()));
            }
        }
        Ok(all.into_iter().find(|e| {
            e.key == identity.key
                && e.original.as_deref() == Some(identity.original.as_path())
                && e.trashed_unix_ms.abs_diff(identity.trashed_unix_ms) <= TIME_TOLERANCE_MS
        }))
    }

    pub fn restore(entry: &TrashEntry, identity: &TrashIdentity) -> Result<()> {
        crate::atomic::rename_exclusive(&entry.path, &identity.original)?;
        // Best effort: drop the `.trashinfo` that described the item.
        if let (Some(files), Some(name)) = (entry.path.parent(), entry.path.file_name()) {
            if let Some(dir) = files.parent() {
                let mut info_name = name.to_os_string();
                info_name.push(INFO_SUFFIX);
                let _ = fs::remove_file(dir.join("info").join(info_name));
            }
        }
        Ok(())
    }
}
