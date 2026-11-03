//! Atomic file writes, delegated to RightKit: temp file in the same directory, fsync,
//! rename over the target, directory fsync (`rightkit_fs::atomic`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::ProjectError;

pub use rightkit_fs::atomic::sync_dir;

static FFMPEG_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// External encoders need a path in RightKit's sweepable temp namespace.
pub(crate) fn ffmpeg_temp_path(out: &Path) -> Result<PathBuf, ProjectError> {
    let leaf = out
        .file_name()
        .ok_or_else(|| ProjectError::BadInput("output needs a file name".into()))?;
    let mut name = std::ffi::OsString::from(".");
    name.push(leaf);
    name.push(format!(
        ".rktmp-{}-{}-0",
        std::process::id(),
        FFMPEG_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    Ok(out.with_file_name(name))
}

/// Writes `bytes` to `path` so readers see either the old or the new content, never a mix.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    rightkit_fs::atomic_write(path, bytes).map_err(|e| {
        ProjectError::io(
            format!("writing {}", path.display()),
            std::io::Error::other(e),
        )
    })
}
