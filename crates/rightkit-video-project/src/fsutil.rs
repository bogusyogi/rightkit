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

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_fs::atomic::TEMP_MARKER;
    use std::fs;

    #[test]
    fn replaces_content_and_leaves_no_temp_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.json");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.json".to_string()]);
    }

    #[test]
    fn failed_write_keeps_old_content() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.json");
        atomic_write(&p, b"old").unwrap();
        // A directory occupying the target path makes the final rename fail.
        let blocked = d.path().join("blocked");
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("x"), b"x").unwrap();
        assert!(atomic_write(&blocked, b"new").is_err());
        assert_eq!(fs::read(&p).unwrap(), b"old");
        let leftovers = fs::read_dir(d.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(TEMP_MARKER)
            })
            .count();
        assert_eq!(leftovers, 0);
    }
}
