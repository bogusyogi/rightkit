//! Move a whole directory to a new place without ever showing a half-moved destination.
//!
//! [`migrate_dir`] (also `AppDirs::migrate_dir` with feature `app-dirs`) is for the folder an app
//! used to keep its data in and now keeps somewhere else (a renamed app folder, a data folder that
//! moves from Local to Roaming, a legacy name). The destination is never visible in a partial state:
//!
//! | Situation | Mechanism | What a crash or a reader can see |
//! |---|---|---|
//! | `from` and `to` on one volume | one `rename` of the directory | the whole tree at `from` or the whole tree at `to` |
//! | different volumes | the tree is copied to a hidden sibling of `to` (`.<name>.rkmigrate-<pid>-<n>`), the sibling is renamed to `to`, then `from` is removed | `to` appears whole or not at all; a crash before the rename leaves `from` untouched and a stray hidden copy (swept by the next call once its process is gone); a crash after it leaves both complete trees |
//!
//! Rules:
//!
//! * `from` must be a real directory (a symlink or a file is refused, never followed).
//! * `to` must be absent or an **empty** real directory. A directory that holds anything is
//!   refused with [`FsError::AlreadyExists`] and nothing is written; nothing is ever merged or
//!   overwritten. `to` may not be inside `from` or the same path; this is decided on canonical
//!   paths (symlinks in `from` or in the existing part of `to` are resolved, case is ignored on
//!   Windows) before anything is created.
//! * Missing parents of `to` are created.
//! * Interrupted, the call is safe to repeat: with `from` untouched it migrates again; with the move
//!   done and `from` still present (`source_removed` was false, or the crash came after the rename)
//!   `to` is populated, so a repeat is refused and the caller removes the leftover `from` itself.
//!
//! Limits: a directory that another process holds open (a Windows working directory inside it, an
//! indexer) can refuse the rename or the final removal; the error is returned. On Windows an empty
//! `to` is replaced in place by the rename (its ACL and timestamps are those of the moved tree);
//! only a platform that refuses that falls back to removing the empty directory first. A read-only
//! file in the copy is flushed with its read-only attribute cleared for the flush and restored.
//! Symlinks inside the
//! tree are kept as symlinks on the copy path. On the copy path every copied file is flushed
//! (`sync_all`) and, on Unix, every directory of the copy is synced before the publishing rename;
//! `from` is removed only after that rename succeeded and the parent directory of `to` was synced
//! (if that sync fails, `from` is kept and `source_removed` is false). Windows cannot sync a
//! directory, so only files are flushed there. Not compiled or run when written.

use crate::error::{FsError, Result};
use crate::tree::{copy_tree, CopyOptions};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// How [`migrate_dir`] moved the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrateMethod {
    /// One `rename` of the directory (same volume): atomic.
    Rename,
    /// `from` and `to` are on different volumes: the tree was copied to a hidden sibling of `to`,
    /// the sibling renamed to `to`, then `from` removed.
    CopyThenRemove,
}

/// What [`migrate_dir`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Migrated {
    /// The mechanism that ran.
    pub method: MigrateMethod,
    /// False only on the copy path when `to` is complete but the old tree could not be removed
    /// entirely (a file in use); the caller may retry `std::fs::remove_dir_all(from)`.
    pub source_removed: bool,
}

/// Move the directory `from` to `to`. See the module docs for the rules and the guarantee.
pub fn migrate_dir(from: &Path, to: &Path) -> Result<Migrated> {
    let meta = fs::symlink_metadata(from).map_err(|e| FsError::io(from, e))?;
    if meta.file_type().is_symlink() {
        return Err(FsError::Symlink(from.to_path_buf()));
    }
    if !meta.is_dir() {
        return Err(FsError::NotADirectory(from.to_path_buf()));
    }
    let abs_from = std::path::absolute(from).map_err(|e| FsError::io(from, e))?;
    let abs_to = std::path::absolute(to).map_err(|e| FsError::io(to, e))?;
    refuse_inside(&abs_from, &abs_to, to)?;
    let parent = match abs_to.parent() {
        Some(p) => p.to_path_buf(),
        None => return Err(FsError::BadName(to.display().to_string())),
    };
    let name = abs_to
        .file_name()
        .ok_or_else(|| FsError::BadName(to.display().to_string()))?
        .to_string_lossy()
        .chars()
        .take(100)
        .collect::<String>();
    // Only now, with the destination known to be outside the source, are parents created.
    fs::create_dir_all(&parent).map_err(|e| FsError::io(&parent, e))?;
    let existing = check_destination(&abs_to)?;

    // Same volume: one rename. A directory rename never replaces a non-empty directory (Unix, and
    // the POSIX-semantics rename current std uses on Windows), so a destination that was filled
    // meanwhile fails the call with nothing lost.
    match rename_into_place(&abs_from, &abs_to, existing) {
        Ok(()) => {
            let _ = crate::atomic::try_sync_dir(&parent);
            return Ok(Migrated {
                method: MigrateMethod::Rename,
                source_removed: true,
            });
        }
        Err(e) if is_cross_device(&e) => {}
        Err(e) if is_occupied(&e) => return Err(FsError::AlreadyExists(abs_to)),
        Err(e) => return Err(FsError::io(&abs_to, e)),
    }

    // Different volumes: copy to a hidden sibling, make it durable, publish it with one rename,
    // then remove `from`.
    sweep_stale_copies(&parent, &name);
    let temp = parent.join(format!(
        ".{name}.rkmigrate-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = copy_tree(&abs_from, &temp, CopyOptions::new(true, true)) {
        let _ = fs::remove_dir_all(&temp);
        return Err(e);
    }
    if let Err(e) = sync_tree(&temp) {
        let _ = fs::remove_dir_all(&temp);
        return Err(e);
    }
    // The copy may have taken long: look at the destination again right before publishing.
    let published = check_destination(&abs_to).and_then(|existing| {
        rename_into_place(&temp, &abs_to, existing).map_err(|e| classify(&abs_to, e))
    });
    if let Err(e) = published {
        let _ = fs::remove_dir_all(&temp);
        return Err(e);
    }
    // `from` is the only other copy: remove it only once `to` is durably named.
    let source_removed =
        crate::atomic::try_sync_dir(&parent).is_ok() && fs::remove_dir_all(&abs_from).is_ok();
    Ok(Migrated {
        method: MigrateMethod::CopyThenRemove,
        source_removed,
    })
}

/// Refuse a destination that is the source or inside it. Decided on canonical paths: `from` is
/// canonicalized, and `to` through its deepest existing ancestor (the missing tail is appended
/// as is, and may not hold `..`), so a symlink into the source is seen. Nothing is created.
fn refuse_inside(abs_from: &Path, abs_to: &Path, to: &Path) -> Result<()> {
    let canon_from =
        crate::plain::canonicalize_plain(abs_from).map_err(|e| FsError::io(abs_from, e))?;
    let canon_to = canonical_with_missing_tail(abs_to).map_err(|e| FsError::io(to, e))?;
    if is_inside(&canon_to, &canon_from) {
        return Err(FsError::io(
            to,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the destination is the source or inside it",
            ),
        ));
    }
    Ok(())
}

fn canonical_with_missing_tail(path: &Path) -> io::Result<PathBuf> {
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor).is_ok() {
            let mut out = crate::plain::canonicalize_plain(ancestor)?;
            let tail = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
            for component in tail.components() {
                match component {
                    Component::Normal(part) => out.push(part),
                    Component::CurDir => {}
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "the missing part of the destination path holds `..`",
                        ))
                    }
                }
            }
            return Ok(out);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no part of the destination path exists",
    ))
}

/// True when `child` is `root` or below it, whole components, case-insensitive on Windows.
fn is_inside(child: &Path, root: &Path) -> bool {
    fn fold(component: Component<'_>) -> String {
        let text = component.as_os_str().to_string_lossy().into_owned();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    }
    let mut child = child.components().map(fold);
    root.components().map(fold).all(|r| child.next() == Some(r))
}

/// Flush the copy: every regular file, and on Unix every directory (Windows cannot sync one).
fn sync_tree(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| FsError::io(dir, e))? {
        let entry = entry.map_err(|e| FsError::io(dir, e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| FsError::io(&path, e))?;
        if kind.is_dir() {
            sync_tree(&path)?;
        } else if kind.is_file() {
            sync_file(&path).map_err(|e| FsError::io(&path, e))?;
        }
    }
    sync_directory(dir).map_err(|e| FsError::io(dir, e))
}

fn sync_file(path: &Path) -> io::Result<()> {
    // Windows needs write access for `FlushFileBuffers`; nothing is written.
    match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file.sync_all(),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => sync_read_only(path, e),
        Err(e) => Err(e),
    }
}

/// A file the write open refused. Unix flushes through a read handle.
#[cfg(not(windows))]
fn sync_read_only(path: &Path, _refused: io::Error) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// Windows cannot flush through a read handle, and a read-only file refuses the write open. When
/// the refusal is the read-only attribute, clear it for the flush and put it back (also when the
/// flush fails), so the copy is made durable instead of silently skipped. Any other refusal (an
/// ACL) is the original error.
#[cfg(windows)]
fn sync_read_only(path: &Path, refused: io::Error) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    if !permissions.readonly() {
        return Err(refused);
    }
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
    let flushed = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all());
    let restored = fs::metadata(path).and_then(|meta| {
        let mut back = meta.permissions();
        back.set_readonly(true);
        fs::set_permissions(path, back)
    });
    flushed.and(restored)
}

#[cfg(unix)]
fn sync_directory(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Path) -> io::Result<()> {
    // Directory handles cannot be flushed on Windows through std; the files were flushed above.
    Ok(())
}

/// `to` must be absent or an empty real directory. Returns whether an empty directory is there.
/// Nothing is removed here: on Unix a rename onto an empty directory works, and on Windows
/// [`rename_into_place`] removes it only right before the rename.
fn check_destination(to: &Path) -> Result<bool> {
    match fs::symlink_metadata(to) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(FsError::io(to, e)),
        Ok(meta) if meta.file_type().is_symlink() => Err(FsError::Symlink(to.to_path_buf())),
        Ok(meta) if !meta.is_dir() => Err(FsError::AlreadyExists(to.to_path_buf())),
        Ok(_) => {
            let mut entries = fs::read_dir(to).map_err(|e| FsError::io(to, e))?;
            if entries.next().is_some() {
                return Err(FsError::AlreadyExists(to.to_path_buf()));
            }
            Ok(true)
        }
    }
}

/// Rename `src` to `to`. `existing_empty` says an empty directory occupies `to`.
#[cfg(not(windows))]
fn rename_into_place(src: &Path, to: &Path, _existing_empty: bool) -> io::Result<()> {
    fs::rename(src, to)
}

/// Windows: the rename is tried first with `to` untouched, because current `std` renames a
/// directory over an empty one in place (keeping the destination's ACL and timestamps), and a
/// cross-volume refusal then leaves the caller's empty `to` exactly as it was. Only when the
/// platform refuses to replace the empty directory (not a volume crossing, not a populated
/// destination) is the empty directory removed right before a second rename and recreated if that
/// fails; that path loses the empty directory's own ACL and timestamps.
#[cfg(windows)]
fn rename_into_place(src: &Path, to: &Path, existing_empty: bool) -> io::Result<()> {
    let first = fs::rename(src, to);
    if !existing_empty {
        return first;
    }
    match first {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => Err(e),
        Err(e) if !(is_occupied(&e) || e.kind() == io::ErrorKind::PermissionDenied) => Err(e),
        Err(_) => {
            // `remove_dir` fails on a directory that became non-empty, so nothing is lost.
            fs::remove_dir(to)?;
            let result = fs::rename(src, to);
            if result.is_err() {
                let _ = fs::create_dir(to);
            }
            result
        }
    }
}

fn classify(path: &Path, e: io::Error) -> FsError {
    if is_occupied(&e) {
        FsError::AlreadyExists(path.to_path_buf())
    } else {
        FsError::io(path, e)
    }
}

/// The destination exists and holds something (`ENOTEMPTY`, `EEXIST`, or the Windows equivalents).
fn is_occupied(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
    ) {
        return true;
    }
    #[cfg(windows)]
    {
        // ERROR_ALREADY_EXISTS (183), ERROR_DIR_NOT_EMPTY (145), ERROR_FILE_EXISTS (80)
        matches!(e.raw_os_error(), Some(183 | 145 | 80))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The rename crossed a volume boundary (`EXDEV`, `ERROR_NOT_SAME_DEVICE`).
fn is_cross_device(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    #[cfg(windows)]
    {
        e.raw_os_error() == Some(17)
    }
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(not(any(windows, unix)))]
    {
        false
    }
}

/// Remove hidden copies an earlier call left behind, but only those whose process is confirmed gone.
fn sweep_stale_copies(parent: &Path, name: &str) {
    let prefix = format!(".{name}.rkmigrate-");
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let Some(rest) = file_name.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid != std::process::id()
            && crate::process_liveness(pid) == crate::ProcessLiveness::Dead
            && entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}
