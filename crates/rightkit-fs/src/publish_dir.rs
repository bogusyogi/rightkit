//! Publish a finished directory at a name, replacing the directory that is there.
//!
//! [`publish_dir`] is the directory counterpart of [`crate::publish_file`]: a staging directory the
//! caller has filled (an unpacked model, a built bundle, a generated cache) becomes visible at
//! `dest` in one step, so a reader sees the whole old tree or the whole new one, never half of
//! either.
//!
//! | `dest` | Mechanism | Window a crash or a reader can see |
//! |---|---|---|
//! | absent | [`crate::rename_exclusive`] (`renameat2(RENAME_NOREPLACE)`, `renamex_np(RENAME_EXCL)`, `MoveFileExW` without replace) | none: `dest` appears whole or not at all |
//! | a directory, macOS (APFS) | [`crate::swap_paths`] (`RENAME_SWAP`), then the old tree is removed | none: each name always resolves to a complete tree |
//! | a directory, Linux | `renameat2(RENAME_EXCHANGE)`, then the old tree is removed | none |
//! | a directory, anywhere the exchange is unsupported (HFS+, network and FAT volumes, Windows) | the old directory is renamed **aside** to a sibling `.<name>.rkold-<pid>-<n>`, the new one is published with `rename_exclusive`, the old tree is removed | `dest` is missing for the instant between the two renames; a crash there leaves the old tree under its `.rkold-` name and no `dest` |
//!
//! A volume without an exclusive rename also cannot hard-link a directory, so there the exclusive steps
//! are a check followed by a plain rename (a plain rename of a directory fails onto a file or a
//! non-empty directory; only an empty directory made by someone else in that instant could be
//! replaced).
//!
//! If publishing the new directory fails after the old one was moved aside, the old one is moved
//! back; if that also fails the error says where the old tree is. Nothing is ever deleted before the
//! new tree is in place.
//!
//! What this does not do: it does not flush the files inside `src` (the caller does that before
//! publishing, with [`crate::durable_sync`] or by writing them through [`crate::atomic_write`]); it
//! flushes the parent directory (Unix) so the rename itself survives a power loss, and Windows has no
//! directory flush (see [`crate::publication_durability`]). `src` and `dest` must be on the same
//! volume. A directory that other processes hold open (a Windows process with a working directory
//! inside it, an indexer) can refuse the rename; the error is returned and nothing changed.
//! A `dest` that is a symlink, a file or anything but a real directory is refused, never replaced
//! and never followed.
//!
//! # Strict policies: never delete what is at `dest`
//!
//! [`publish_dir`] replaces a **non-empty** `dest` and deletes the displaced tree. A caller that must
//! not destroy anything that is already there uses [`publish_dir_with`] and a strict
//! [`DirPublishPolicy`]. The strict policies never exchange, never move `dest` aside, and never call
//! `remove_dir_all`; they publish only through a **native atomic no-replace rename** (see
//! [`DirPublishPolicy::AbsentOnly`] for the primitive per platform) and refuse with
//! [`FsError::Unavailable`] where the platform or volume has none, instead of the check-then-rename
//! fallback that `publish_dir` accepts for an absent `dest`.

use crate::error::{FsError, PostPublishStep, Result};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// How a directory ended up at its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DirPublishMethod {
    /// `dest` was absent: one atomic create-new rename.
    ExclusiveRename,
    /// `dest` existed and the two directories were exchanged atomically
    /// (`RENAME_SWAP` / `RENAME_EXCHANGE`); the old tree was then removed.
    Exchange,
    /// `dest` existed and no exchange was available: the old directory was renamed aside, the new
    /// one published exclusively, and the old tree removed. Not atomic for a reader (see the module
    /// table).
    AsideRename,
    /// [`DirPublishPolicy::AbsentOrEmpty`] only: `dest` was an empty directory, it was removed with
    /// `remove_dir` (which cannot remove a directory that has anything in it), and the new directory
    /// was then published with the native atomic no-replace rename. Between those two steps `dest` is
    /// absent.
    EmptyRemovedThenExclusiveRename,
}

/// What a directory publish does when something is already at `dest`. Used by [`publish_dir_with`].
///
/// `Replace` is [`publish_dir`] and the default. The two strict policies never delete or displace
/// anything at `dest`: they refuse instead. They share one publishing primitive, a native atomic
/// no-replace rename, and they never use the check-then-plain-rename fallback that
/// [`publish_dir`] uses for an absent `dest` on a volume without one (that fallback could replace an
/// empty directory a competitor created in the instant between its check and its rename, and
/// [`publish_dir`] accepts that; a strict policy does not).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DirPublishPolicy {
    /// Replace the directory at `dest` if there is one, deleting the displaced tree. Exactly
    /// [`publish_dir`].
    #[default]
    Replace,
    /// Publish only if nothing exists at `dest`. If anything exists there (a directory empty or not,
    /// a file, a symlink) the call returns [`FsError::AlreadyExists`] and touches neither `src` nor
    /// `dest`.
    ///
    /// The one primitive is a native atomic no-replace rename; it is also the only step, so there is
    /// no window and nothing to retry:
    ///
    /// | Platform | Primitive |
    /// |---|---|
    /// | Linux | `renameat2(RENAME_NOREPLACE)` |
    /// | macOS and other Apple | `renamex_np(RENAME_EXCL)` |
    /// | Windows | `MoveFileExW` with no flags (no `MOVEFILE_REPLACE_EXISTING`) |
    /// | anything else, Android | none |
    ///
    /// Where the primitive is missing (an old kernel or a filesystem that does not implement the
    /// flag, a platform with none) the call returns [`FsError::Unavailable`] and nothing changed;
    /// it does not fall back to a `link` then `unlink`, a placeholder, or a check followed by a plain
    /// rename. Identity, parent and symlink checks of `src` and `dest` are those of [`publish_dir`].
    AbsentOnly,
    /// Publish if `dest` is absent **or an empty real directory**; refuse if it holds anything.
    ///
    /// If `dest` exists it must be a real directory ([`FsError::Symlink`] for a symlink,
    /// [`FsError::NotADirectory`] for a file). It is removed only with [`std::fs::remove_dir`], which
    /// fails atomically if the directory is not empty; that failure returns
    /// [`FsError::AlreadyExists`] with `dest` and everything in it untouched. The new directory is
    /// then published with the native atomic no-replace rename of [`DirPublishPolicy::AbsentOnly`]
    /// (same primitive per platform, same [`FsError::Unavailable`]); the first attempt is the
    /// rename itself, so nothing is removed on a platform that has no such rename.
    ///
    /// **The gap.** Between the `remove_dir` and the rename `dest` is absent. A competing process that
    /// creates `dest` (or puts anything in it) in that gap wins: the rename then fails with
    /// "exists", the call tries again (at most four passes, each starting with the rename), finds
    /// the competitor's directory and either removes it again only if it is still empty or refuses
    /// with [`FsError::AlreadyExists`]. The competitor's files are never deleted: a file can only be
    /// created inside `dest` while it exists, and `remove_dir` refuses a directory that has one.
    /// A crash in the gap leaves `dest` absent. The removed empty directory is not preserved; if the
    /// publish fails for a reason other than a competitor, an empty directory is recreated at `dest`
    /// with default permissions (best effort) and the original error is returned. A successful
    /// call reports `replaced: true` and [`DirPublishMethod::EmptyRemovedThenExclusiveRename`] when
    /// it removed an empty directory, and `replaced: false` with
    /// [`DirPublishMethod::ExclusiveRename`] when `dest` was absent.
    AbsentOrEmpty,
}

/// What [`publish_dir`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PublishedDir {
    /// True when a directory already at `dest` was replaced.
    pub replaced: bool,
    /// The mechanism that made the new directory visible.
    pub method: DirPublishMethod,
}

/// Publish the directory `src` at `dest`, replacing the directory at `dest` if there is one. See the
/// module docs for the mechanisms and what a reader or a crash can observe.
///
/// `src` must be a real directory (a symlink is refused as [`FsError::Symlink`]); `dest` must be absent
/// or a real directory, must have a final name that is not shaped like a temp name this crate
/// generates ([`FsError::BadName`]), and must not be `src`, inside `src`, or an ancestor of `src`
/// ([`FsError::Io`] with `InvalidInput`). On success `src` no longer exists (it became `dest`, or its
/// old contents were removed after an exchange).
///
/// Errors: any failure before the new directory is in place leaves `dest` as it was (or restores it).
/// After it is in place: [`FsError::ReplacedButPostStepFailed`] with
/// [`PostPublishStep::OldTreeCleanup`] when the displaced tree could not be removed (the new
/// directory is published, the old one remains and the error's source names where), and
/// [`FsError::ReplacedButDirSyncFailed`] when the parent directory sync failed.
///
/// This replaces a non-empty `dest` and deletes its contents. To refuse instead, use
/// [`publish_dir_with`] with [`DirPublishPolicy::AbsentOnly`] or [`DirPublishPolicy::AbsentOrEmpty`].
/// `publish_dir(src, dest)` is exactly `publish_dir_with(src, dest, DirPublishPolicy::Replace)`.
pub fn publish_dir(src: &Path, dest: &Path) -> Result<PublishedDir> {
    publish_dir_with(src, dest, DirPublishPolicy::Replace)
}

/// Publish the directory `src` at `dest` under `policy`. See [`DirPublishPolicy`] for what each
/// policy does with something already at `dest`, and the module docs for the mechanisms.
///
/// Every policy makes the same operand checks as [`publish_dir`]: `src` must be a real directory
/// ([`FsError::Symlink`], [`FsError::NotADirectory`]); `dest` must have a final name that is not shaped
/// like a temp name this crate generates ([`FsError::BadName`]) and must not be `src`, inside `src`,
/// or an ancestor of `src`, including the same directory reached by two spellings
/// ([`FsError::Io`] with `InvalidInput`). On success `src` no longer exists. A refusal leaves `src`
/// and `dest` as they were.
///
/// Strict policies ([`DirPublishPolicy::AbsentOnly`], [`DirPublishPolicy::AbsentOrEmpty`]) return
/// [`FsError::AlreadyExists`] when `dest` is, or becomes, occupied, [`FsError::Unavailable`] when the
/// platform or volume has no native atomic no-replace rename, and after the new directory is in place
/// [`FsError::ReplacedButDirSyncFailed`] when the parent directory sync failed. They never return
/// [`FsError::ReplacedButPostStepFailed`] because they delete no tree.
pub fn publish_dir_with(src: &Path, dest: &Path, policy: DirPublishPolicy) -> Result<PublishedDir> {
    check_operands(src, dest)?;
    match policy {
        DirPublishPolicy::Replace => publish_replacing(src, dest),
        DirPublishPolicy::AbsentOnly => publish_strict(src, dest, false),
        DirPublishPolicy::AbsentOrEmpty => publish_strict(src, dest, true),
    }
}

/// The strict policies: native no-replace rename only, nothing at `dest` is ever deleted except an
/// empty directory removed with `remove_dir` when `allow_empty`.
fn publish_strict(src: &Path, dest: &Path, allow_empty: bool) -> Result<PublishedDir> {
    let mut removed_empty = false;
    // A few passes absorb a competitor that creates `dest` and removes it again; each pass starts
    // with the rename, which is also the probe for whether the platform has a native one.
    for _ in 0..4 {
        match rename_noreplace_native(src, dest) {
            Ok(()) => {
                sync_parents(src, dest)?;
                return Ok(PublishedDir {
                    replaced: removed_empty,
                    method: if removed_empty {
                        DirPublishMethod::EmptyRemovedThenExclusiveRename
                    } else {
                        DirPublishMethod::ExclusiveRename
                    },
                });
            }
            Err(FsError::AlreadyExists(_)) if !allow_empty => {
                return Err(FsError::AlreadyExists(dest.to_path_buf()))
            }
            Err(FsError::AlreadyExists(_)) => {}
            Err(e) => {
                if removed_empty {
                    // Best effort: give the caller back an (empty) directory at the name.
                    let _ = std::fs::create_dir(dest);
                }
                return Err(e);
            }
        }
        // `dest` exists and `allow_empty`: it must be a real directory, and it goes only if empty.
        match std::fs::symlink_metadata(dest) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(FsError::Symlink(dest.to_path_buf()))
            }
            Ok(meta) if !meta.is_dir() => return Err(FsError::NotADirectory(dest.to_path_buf())),
            Ok(_) => {}
            // Removed by someone else since the rename saw it: publish again.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(FsError::io(dest, e)),
        }
        // `remove_dir` (rmdir / RemoveDirectoryW) refuses a directory with anything in it; nothing
        // else removes `dest`.
        match std::fs::remove_dir(dest) {
            Ok(()) => removed_empty = true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) if dir_not_empty(&e) => return Err(FsError::AlreadyExists(dest.to_path_buf())),
            Err(e) => return Err(FsError::io(dest, e)),
        }
    }
    Err(FsError::io(
        dest,
        io::Error::other(
            "the destination kept appearing and disappearing while it was being published",
        ),
    ))
}

/// Whether a failed `remove_dir` means "the directory has something in it".
fn dir_not_empty(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::DirectoryNotEmpty {
        return true;
    }
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        // POSIX lets `rmdir` report a non-empty directory as either.
        return code == libc::ENOTEMPTY || code == libc::EEXIST;
    }
    false
}

fn publish_replacing(src: &Path, dest: &Path) -> Result<PublishedDir> {
    // A few passes absorb a racing creator or remover of `dest`; each pass is complete on its own.
    for _ in 0..4 {
        match rename_dir_exclusive(src, dest) {
            Ok(()) => {
                sync_parents(src, dest)?;
                return Ok(PublishedDir {
                    replaced: false,
                    method: DirPublishMethod::ExclusiveRename,
                });
            }
            Err(FsError::AlreadyExists(_)) => {}
            Err(e) => return Err(e),
        }
        // `dest` exists: it must be a real directory.
        match std::fs::symlink_metadata(dest) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(FsError::Symlink(dest.to_path_buf()))
            }
            Ok(meta) if !meta.is_dir() => return Err(FsError::NotADirectory(dest.to_path_buf())),
            Ok(_) => {}
            // Removed by someone else since the exclusive rename saw it: publish exclusively again.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(FsError::io(dest, e)),
        }
        match exchange(src, dest) {
            Ok(()) => {
                // `src` now names the old tree.
                let cleanup = std::fs::remove_dir_all(src);
                sync_parents(src, dest)?;
                finish_replace(dest, src, cleanup)?;
                return Ok(PublishedDir {
                    replaced: true,
                    method: DirPublishMethod::Exchange,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && !src.exists() => {
                return Err(FsError::io(src, e))
            }
            // `dest` vanished between the check and the exchange: start over.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) if crate::swap::is_swap_unsupported(&e) => {}
            Err(e) => return Err(FsError::io(dest, e)),
        }
        match replace_via_aside(src, dest) {
            Ok(()) => {
                return Ok(PublishedDir {
                    replaced: true,
                    method: DirPublishMethod::AsideRename,
                })
            }
            Err(AsideError::DestVanished) => continue,
            Err(AsideError::Failed(e)) => return Err(e),
        }
    }
    Err(FsError::io(
        dest,
        io::Error::other(
            "the destination kept appearing and disappearing while it was being replaced",
        ),
    ))
}

fn check_operands(src: &Path, dest: &Path) -> Result<()> {
    match std::fs::symlink_metadata(src) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(FsError::Symlink(src.to_path_buf()))
        }
        Ok(meta) if !meta.is_dir() => return Err(FsError::NotADirectory(src.to_path_buf())),
        Ok(_) => {}
        Err(e) => return Err(FsError::io(src, e)),
    }
    let name = dest
        .file_name()
        .ok_or_else(|| FsError::BadName(dest.display().to_string()))?;
    crate::atomic::reject_reserved_name(&name.to_string_lossy())?;
    let (abs_src, abs_dest) = (absolute(src)?, absolute(dest)?);
    if abs_src == abs_dest || abs_dest.starts_with(&abs_src) || abs_src.starts_with(&abs_dest) {
        return Err(FsError::io(
            dest,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the source and destination directories are the same, or one is inside the other",
            ),
        ));
    }
    // The same directory reached by two spellings (a symlinked parent, `..`).
    if let (Ok(a), Ok(b)) = (
        crate::file_key::FileKey::of_path(src),
        crate::file_key::FileKey::of_path(dest),
    ) {
        if a == b {
            return Err(FsError::io(
                dest,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the source and destination are the same directory",
                ),
            ));
        }
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).map_err(|e| FsError::io(path, e))
}

/// The directory a rename into or out of `path` changes.
fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn sync_parents(src: &Path, dest: &Path) -> Result<()> {
    let (dest_parent, src_parent) = (parent_of(dest), parent_of(src));
    crate::atomic::try_sync_dir(&dest_parent).map_err(|source| {
        FsError::ReplacedButDirSyncFailed {
            path: dest.to_path_buf(),
            source,
        }
    })?;
    if src_parent != dest_parent {
        crate::atomic::try_sync_dir(&src_parent).map_err(|source| {
            FsError::ReplacedButDirSyncFailed {
                path: dest.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

/// Turn the outcome of removing the displaced tree into the published-but-not-clean error.
fn finish_replace(dest: &Path, old_tree: &Path, cleanup: io::Result<()>) -> Result<()> {
    match cleanup {
        Ok(()) => Ok(()),
        Err(e) => Err(FsError::ReplacedButPostStepFailed {
            path: dest.to_path_buf(),
            step: PostPublishStep::OldTreeCleanup,
            source: io::Error::new(
                e.kind(),
                format!(
                    "the replaced directory remains at {}: {e}",
                    old_tree.display()
                ),
            ),
        }),
    }
}

/// Atomically exchange two existing directories where the OS can: macOS `RENAME_SWAP`, Linux
/// `RENAME_EXCHANGE`. Anywhere else (and on a volume without it) the error is one
/// [`crate::swap::is_swap_unsupported`] recognises.
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        crate::swap::swap_paths(a, b)
    }
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        const RENAME_EXCHANGE: libc::c_uint = 2;
        let invalid = |_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte");
        let ca = CString::new(a.as_os_str().as_bytes()).map_err(invalid)?;
        let cb = CString::new(b.as_os_str().as_bytes()).map_err(invalid)?;
        // SAFETY: both paths are NUL-terminated C strings that outlive the call.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                ca.as_ptr(),
                libc::AT_FDCWD,
                cb.as_ptr(),
                RENAME_EXCHANGE,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (a, b);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "an atomic directory exchange is not available on this platform",
        ))
    }
}

/// Rename the directory `src` to `dest` only if `dest` does not exist.
///
/// [`crate::rename_exclusive`] where the volume has an exclusive rename. A volume without one (HFS+,
/// FAT, some shares) also cannot hard-link a directory, so its fallback cannot move a directory; there
/// the name is checked and a plain rename follows. A plain rename of a directory fails onto a file and
/// onto a non-empty directory, so the only thing the check-then-rename could replace is an empty
/// directory another process made in the instant between the two.
fn rename_dir_exclusive(src: &Path, dest: &Path) -> Result<()> {
    match crate::atomic::rename_exclusive_reporting(src, dest) {
        Ok(_) => Ok(()),
        Err(e @ FsError::AlreadyExists(_)) => Err(e),
        Err(first) => {
            match std::fs::symlink_metadata(dest) {
                Ok(_) => return Err(FsError::AlreadyExists(dest.to_path_buf())),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return Err(first),
            }
            std::fs::rename(src, dest).map_err(|e| FsError::io(dest, e))
        }
    }
}

/// Rename the directory `src` to `dest` with the platform's native atomic no-replace rename and
/// nothing else: `renameat2(RENAME_NOREPLACE)` (Linux), `renamex_np(RENAME_EXCL)` (Apple),
/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (Windows). Unlike [`rename_dir_exclusive`] there
/// is no `link`, placeholder or check-then-rename fallback: where the primitive is missing the result
/// is [`FsError::Unavailable`] and nothing changed. An existing `dest` of any kind is
/// [`FsError::AlreadyExists`].
#[cfg(target_os = "linux")]
fn rename_noreplace_native(src: &Path, dest: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    const RENAME_NOREPLACE: libc::c_uint = 1;
    let c_path = |path: &Path| {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            FsError::io(
                path,
                io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"),
            )
        })
    };
    let (csrc, cdest) = (c_path(src)?, c_path(dest)?);
    // SAFETY: both paths are NUL-terminated C strings that outlive the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            csrc.as_ptr(),
            libc::AT_FDCWD,
            cdest.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    Err(noreplace_error(
        dest,
        io::Error::last_os_error(),
        "renameat2(RENAME_NOREPLACE) is not available here (old kernel or a filesystem without it)",
    ))
}

#[cfg(target_vendor = "apple")]
fn rename_noreplace_native(src: &Path, dest: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    const RENAME_EXCL: libc::c_uint = 0x0000_0004;
    let c_path = |path: &Path| {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            FsError::io(
                path,
                io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"),
            )
        })
    };
    let (csrc, cdest) = (c_path(src)?, c_path(dest)?);
    // SAFETY: both paths are NUL-terminated C strings that outlive the call.
    let rc = unsafe { libc::renamex_np(csrc.as_ptr(), cdest.as_ptr(), RENAME_EXCL) };
    if rc == 0 {
        return Ok(());
    }
    Err(noreplace_error(
        dest,
        io::Error::last_os_error(),
        "renamex_np(RENAME_EXCL) is not available on this volume (HFS+, FAT, some shares)",
    ))
}

#[cfg(windows)]
fn rename_noreplace_native(src: &Path, dest: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let wide =
        |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (wsrc, wdest) = (wide(src), wide(dest));
    // SAFETY: both buffers are NUL-terminated and outlive the call. Flags 0: no
    // `MOVEFILE_REPLACE_EXISTING` and no `MOVEFILE_COPY_ALLOWED`, so an existing name fails and a
    // move across volumes fails instead of copying.
    if unsafe { MoveFileExW(wsrc.as_ptr(), wdest.as_ptr(), 0) } != 0 {
        return Ok(());
    }
    Err(noreplace_error(
        dest,
        io::Error::last_os_error(),
        "MoveFileExW is not available for this directory",
    ))
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn rename_noreplace_native(src: &Path, dest: &Path) -> Result<()> {
    let _ = (src, dest);
    Err(FsError::Unavailable(
        "an atomic no-replace directory rename is not available on this platform",
    ))
}

/// Classify the failure of a native no-replace rename: the name exists, the primitive is missing, or
/// a real error.
#[cfg(any(target_os = "linux", target_vendor = "apple", windows))]
fn noreplace_error(dest: &Path, error: io::Error, unavailable: &'static str) -> FsError {
    if error.kind() == io::ErrorKind::AlreadyExists {
        return FsError::AlreadyExists(dest.to_path_buf());
    }
    #[cfg(unix)]
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::ENOTSUP)
    ) {
        return FsError::Unavailable(unavailable);
    }
    #[cfg(not(unix))]
    let _ = unavailable;
    FsError::io(dest, error)
}

enum AsideError {
    /// `dest` was gone by the time it was renamed aside; the caller retries from the start.
    DestVanished,
    Failed(FsError),
}

/// A sibling name for the displaced directory: `.<name>.rkold-<pid>-<n>`, not a temp-name shape.
fn aside_name(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut end = name.len().min(64);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    parent_of(dest).join(format!(
        ".{}.rkold-{}-{n}",
        &name[..end],
        std::process::id()
    ))
}

/// The portable replacement: move the old directory aside, publish the new one exclusively, remove
/// the old tree. If the publish fails the old directory is moved back.
fn replace_via_aside(src: &Path, dest: &Path) -> std::result::Result<(), AsideError> {
    let aside = aside_name(dest);
    // A plain rename: the aside name is unique to this call (pid and counter), and this is the very
    // path taken on a volume whose exclusive rename does not exist.
    match std::fs::rename(dest, &aside) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(AsideError::DestVanished),
        Err(e) => return Err(AsideError::Failed(FsError::io(dest, e))),
    }
    match rename_dir_exclusive(src, dest) {
        Ok(()) => {
            let cleanup = std::fs::remove_dir_all(&aside);
            let synced = sync_parents(src, dest);
            finish_replace(dest, &aside, cleanup).map_err(AsideError::Failed)?;
            synced.map_err(AsideError::Failed)
        }
        Err(publish_error) => {
            // Put the old directory back so `dest` is as the caller left it.
            match rename_dir_exclusive(&aside, dest) {
                Ok(()) => Err(AsideError::Failed(publish_error)),
                Err(restore_error) => Err(AsideError::Failed(FsError::io(
                    dest,
                    io::Error::other(format!(
                        "publishing the new directory failed ({publish_error}) and the previous \
                         directory could not be restored from {} ({restore_error})",
                        aside.display()
                    )),
                ))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn staged(dir: &Path, name: &str, marker: &str) -> PathBuf {
        let path = dir.join(name);
        fs::create_dir_all(path.join("sub")).unwrap();
        fs::write(path.join("marker"), marker).unwrap();
        fs::write(path.join("sub/deep"), marker).unwrap();
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn publishes_at_an_absent_name_and_consumes_the_source() {
        let t = tempfile::tempdir().unwrap();
        let src = staged(t.path(), "stage", "new");
        let dest = t.path().join("model");
        let out = publish_dir(&src, &dest).unwrap();
        assert_eq!(
            out,
            PublishedDir {
                replaced: false,
                method: DirPublishMethod::ExclusiveRename
            }
        );
        assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "new");
        assert_eq!(fs::read_to_string(dest.join("sub/deep")).unwrap(), "new");
        assert!(!src.exists());
        assert_eq!(names(t.path()), ["model"]);
    }

    #[test]
    fn replaces_an_existing_directory_and_leaves_no_old_tree_behind() {
        let t = tempfile::tempdir().unwrap();
        let dest = staged(t.path(), "model", "old");
        fs::write(dest.join("only-in-old"), "x").unwrap();
        let src = staged(t.path(), "stage", "new");
        let out = publish_dir(&src, &dest).unwrap();
        assert!(out.replaced);
        assert_ne!(out.method, DirPublishMethod::ExclusiveRename);
        assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "new");
        assert!(!dest.join("only-in-old").exists(), "an old file survived");
        assert!(!src.exists());
        assert_eq!(
            names(t.path()),
            ["model"],
            "a stage or .rkold- name was left"
        );
    }

    #[test]
    fn the_aside_path_replaces_and_cleans_up() {
        let t = tempfile::tempdir().unwrap();
        let dest = staged(t.path(), "model", "old");
        let src = staged(t.path(), "stage", "new");
        replace_via_aside(&src, &dest).ok().expect("replaced");
        assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "new");
        assert!(!src.exists());
        assert_eq!(names(t.path()), ["model"]);
    }

    #[test]
    fn a_failed_publish_restores_the_old_directory() {
        let t = tempfile::tempdir().unwrap();
        let dest = staged(t.path(), "model", "old");
        // The source does not exist, so the second rename fails after the first moved `dest` aside.
        let missing = t.path().join("never-made");
        match replace_via_aside(&missing, &dest) {
            Err(AsideError::Failed(e)) => assert!(e.is_not_found(), "{e:?}"),
            _ => panic!("expected a failure"),
        }
        assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "old");
        assert_eq!(names(t.path()), ["model"], "the aside name was left behind");
    }

    #[test]
    fn a_missing_source_changes_nothing() {
        let t = tempfile::tempdir().unwrap();
        let dest = staged(t.path(), "model", "old");
        let err = publish_dir(&t.path().join("absent"), &dest).unwrap_err();
        assert!(err.is_not_found(), "{err:?}");
        assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "old");
    }

    #[test]
    fn a_file_source_or_file_destination_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("f");
        fs::write(&file, "x").unwrap();
        let stage = staged(t.path(), "stage", "new");
        assert!(matches!(
            publish_dir(&file, &t.path().join("d")),
            Err(FsError::NotADirectory(_))
        ));
        let err = publish_dir(&stage, &file).unwrap_err();
        assert!(matches!(err, FsError::NotADirectory(_)), "{err:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "x");
        assert!(stage.exists(), "the source survives a refusal");
    }

    #[test]
    fn nesting_and_identity_are_refused() {
        let t = tempfile::tempdir().unwrap();
        let src = staged(t.path(), "stage", "new");
        for dest in [src.clone(), src.join("inside"), t.path().to_path_buf()] {
            let err = publish_dir(&src, &dest).unwrap_err();
            assert_eq!(
                err.kind(),
                Some(io::ErrorKind::InvalidInput),
                "{dest:?}: {err:?}"
            );
        }
        assert!(src.join("marker").exists());
    }

    #[test]
    fn a_temp_shaped_destination_name_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let src = staged(t.path(), "stage", "new");
        let err = publish_dir(&src, &t.path().join(".model.rktmp-123-1-0")).unwrap_err();
        assert!(matches!(err, FsError::BadName(_)), "{err:?}");
    }

    #[test]
    fn the_aside_name_is_a_sibling_and_not_a_temp_name() {
        let dest = Path::new("/data/models/whisper");
        let aside = aside_name(dest);
        assert_eq!(aside.parent(), dest.parent());
        let name = aside.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".whisper.rkold-"), "{name}");
        assert!(crate::atomic::reject_reserved_name(&name).is_ok());
        let long = Path::new("/data").join("n".repeat(250));
        assert!(aside_name(&long).file_name().unwrap().len() < 255);
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::symlink;

        #[test]
        fn a_symlink_destination_is_refused_and_its_target_untouched() {
            let t = tempfile::tempdir().unwrap();
            let real = staged(t.path(), "real", "precious");
            let link = t.path().join("link");
            symlink(&real, &link).unwrap();
            let stage = staged(t.path(), "stage", "new");
            let err = publish_dir(&stage, &link).unwrap_err();
            assert!(matches!(err, FsError::Symlink(_)), "{err:?}");
            assert_eq!(fs::read_to_string(real.join("marker")).unwrap(), "precious");
            assert!(fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(stage.exists());
        }

        #[test]
        fn a_symlink_source_is_refused() {
            let t = tempfile::tempdir().unwrap();
            let real = staged(t.path(), "real", "x");
            let link = t.path().join("link");
            symlink(&real, &link).unwrap();
            let err = publish_dir(&link, &t.path().join("out")).unwrap_err();
            assert!(matches!(err, FsError::Symlink(_)), "{err:?}");
            assert!(!t.path().join("out").exists());
        }

        #[test]
        fn a_symlink_inside_the_displaced_tree_is_removed_not_followed() {
            let t = tempfile::tempdir().unwrap();
            let outside = t.path().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("keep"), "keep").unwrap();
            let dest = staged(t.path(), "model", "old");
            symlink(&outside, dest.join("escape")).unwrap();
            let src = staged(t.path(), "stage", "new");
            publish_dir(&src, &dest).unwrap();
            assert_eq!(fs::read_to_string(outside.join("keep")).unwrap(), "keep");
            assert!(!dest.join("escape").exists());
        }
    }
}
