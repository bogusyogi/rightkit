//! Rename an entry to a name that differs only in letter case.
//!
//! On a case-insensitive volume (the default on macOS and Windows) `Trips` and `trips` are the same
//! folder, so [`crate::rename_exclusive`] sees the destination as existing and refuses.
//! [`rename_case_only`] decides, from the filesystem's own identity ([`FileKey`]: device and inode,
//! or volume serial and file index), whether the destination name is the very same entry, and if so
//! performs the rename through a unique hidden name in the same directory.
//!
//! Crash semantics: the two renames are each atomic, never overwrite, and never leave the entry
//! missing. A crash between them leaves the entry, whole and unchanged, under the hidden name
//! `.<name>.rkcase-<pid>-<n>` in the same directory (its content is intact; the caller or the user
//! renames it). A failure of the second rename is undone by renaming the entry back to its original
//! name; only when that also fails is the entry left under the hidden name, and the error says so.

use crate::error::{FsError, Result};
use crate::file_key::FileKey;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Rename `from` to `to`, where the two paths are in the same directory and their final names
/// differ only in letter case (`Trips` to `trips`).
///
/// * A symlink at `from` is [`FsError::Symlink`]; a `from` that does not exist is an I/O error.
/// * If `to` does not exist (a case-sensitive volume) this is [`crate::rename_exclusive`].
/// * If `to` exists and is the same entry as `from` (same [`FileKey`]) the rename goes through a
///   hidden unique name in the same directory; nothing is ever overwritten.
/// * If `to` exists and is a different entry the result is [`FsError::AlreadyExists`] and nothing
///   moves.
/// * Paths that are not in the same directory, or whose names are not equal ignoring case, are
///   [`FsError::BadName`]; a rename to the identical name is also refused (there is nothing to do).
///
/// Where the volume has neither an exclusive rename nor hard links (FAT, exFAT, some shares), the
/// underlying [`crate::rename_exclusive`] refuses a folder rename and nothing moves.
pub fn rename_case_only(from: &Path, to: &Path) -> Result<()> {
    let (from_name, to_name) = case_pair(from, to)?;
    let from_meta = fs::symlink_metadata(from).map_err(|e| FsError::io(from, e))?;
    if from_meta.file_type().is_symlink() {
        return Err(FsError::Symlink(from.to_path_buf()));
    }
    match fs::symlink_metadata(to) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => crate::atomic::rename_exclusive(from, to),
        Err(e) => Err(FsError::io(to, e)),
        Ok(to_meta) => {
            if to_meta.file_type().is_symlink() {
                return Err(FsError::AlreadyExists(to.to_path_buf()));
            }
            if FileKey::of_path(from)? != FileKey::of_path(to)? {
                return Err(FsError::AlreadyExists(to.to_path_buf()));
            }
            via_hidden_name(from, to, &from_name, &to_name)
        }
    }
}

/// Same directory, names differ, names equal ignoring case. Returns both final names.
fn case_pair(from: &Path, to: &Path) -> Result<(String, String)> {
    let bad = |path: &Path| FsError::BadName(path.display().to_string());
    let name = |path: &Path| {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| bad(path))
    };
    let (from_name, to_name) = (name(from)?, name(to)?);
    let dir = |path: &Path| match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if dir(from) != dir(to)
        || from_name == to_name
        || from_name.to_lowercase() != to_name.to_lowercase()
    {
        return Err(bad(to));
    }
    Ok((from_name, to_name))
}

fn via_hidden_name(from: &Path, to: &Path, from_name: &str, to_name: &str) -> Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = match from.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let short: String = from_name.chars().take(100).collect();
    let hidden = dir.join(format!(
        ".{short}.rkcase-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    crate::atomic::rename_exclusive(from, &hidden)?;
    match crate::atomic::rename_exclusive(&hidden, to) {
        Ok(()) => Ok(()),
        Err(first) => match crate::atomic::rename_exclusive(&hidden, from) {
            Ok(()) => Err(first),
            Err(_) => Err(FsError::io(
                &hidden,
                io::Error::other(format!(
                    "renaming to {to_name} failed ({first}) and the entry could not be put back; \
                     it is intact at this hidden name"
                )),
            )),
        },
    }
}
