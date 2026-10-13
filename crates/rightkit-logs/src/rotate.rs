//! Size-based rotation shared by [`crate::JsonlStore`] and [`crate::RotatingTextWriter`].
//!
//! Generation `n` of `app.log` is `app.log.n`; generation 1 is the newest predecessor.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Path of rotated generation `n` (`n >= 1`) of `path`.
pub(crate) fn generation_path(path: &Path, n: u32, default_name: &str) -> PathBuf {
    let mut filename = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(default_name));
    filename.push(format!(".{n}"));
    path.with_file_name(filename)
}

pub(crate) fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Rotate `path` when it has reached `max_file_bytes` (`0` disables rotation). Keeps `generations`
/// predecessors (`0` behaves as `1`): the oldest is removed, each `.k` becomes `.k+1`, and the
/// current file becomes `.1`. With `generations == 1` this removes `.1` and renames the current
/// file to it.
pub(crate) fn rotate_if_needed(
    path: &Path,
    max_file_bytes: u64,
    generations: u32,
    default_name: &str,
) -> io::Result<()> {
    if max_file_bytes == 0 {
        return Ok(());
    }
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() < max_file_bytes {
        return Ok(());
    }
    let generations = generations.max(1);
    remove_if_present(&generation_path(path, generations, default_name))?;
    for n in (1..generations).rev() {
        let from = generation_path(path, n, default_name);
        match fs::rename(&from, generation_path(path, n + 1, default_name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    fs::rename(path, generation_path(path, 1, default_name))
}

/// After a failed rotation: refuse the append once the current file is at twice the rotation size.
pub(crate) fn refuse_at_hard_cap(path: &Path, max_file_bytes: u64) -> io::Result<()> {
    if max_file_bytes == 0 {
        return Ok(());
    }
    let hard_cap = max_file_bytes.saturating_mul(2);
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len < hard_cap {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "rotation failed and {} reached its hard cap of {hard_cap} bytes",
        path.display()
    )))
}
