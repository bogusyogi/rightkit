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

/// How rotated generations are named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RotatedNaming {
    /// `app.log.N` (the default).
    #[default]
    Suffix,
    /// `app.N.log`: the number goes between the stem and the extension (`app.N` when the file
    /// has no extension).
    StemNumbered,
}

/// Path of rotated generation `n` (`n >= 1`) of `path` under `naming`.
pub(crate) fn named_generation_path(
    path: &Path,
    n: u32,
    default_name: &str,
    naming: RotatedNaming,
) -> PathBuf {
    match naming {
        RotatedNaming::Suffix => generation_path(path, n, default_name),
        RotatedNaming::StemNumbered => {
            let fallback = Path::new(default_name);
            let base = if path.file_name().is_some() { path } else { fallback };
            let stem = base
                .file_stem()
                .map(OsString::from)
                .unwrap_or_else(|| OsString::from(default_name));
            let mut filename = stem;
            filename.push(format!(".{n}"));
            if let Some(ext) = base.extension() {
                filename.push(".");
                filename.push(ext);
            }
            path.with_file_name(filename)
        }
    }
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
    roll_generations(path, generations, default_name, RotatedNaming::Suffix)
}

/// Unconditionally roll `path` into generation 1 (shifting older ones; the oldest is removed).
/// A missing or empty current file is left alone.
pub(crate) fn roll_generations(
    path: &Path,
    generations: u32,
    default_name: &str,
    naming: RotatedNaming,
) -> io::Result<()> {
    let generations = generations.max(1);
    let gen_path = |n: u32| named_generation_path(path, n, default_name, naming);
    remove_if_present(&gen_path(generations))?;
    for n in (1..generations).rev() {
        match fs::rename(gen_path(n), gen_path(n + 1)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    fs::rename(path, gen_path(1))
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
