//! Atomic file replacement: temp file in the destination directory, `fsync`, rename over the
//! target, `fsync` the directory. Readers see the old content or the new content, never a mix,
//! and a crash at any point leaves the old file intact plus (at worst) an orphan temp file
//! that [`sweep_stale_temps`] reclaims.
//!
//! Consolidates CutRight `fsutil::atomic_write` (project and qa copies), CodeRight
//! `private_fs::atomic` and the settings/model-store writers.

use crate::error::{FsError, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Marker inside every temp name; sweeping only ever touches names containing it.
pub const TEMP_MARKER: &str = ".rktmp-";
static COUNTER: AtomicU64 = AtomicU64::new(0);
const TEMP_ATTEMPTS: u32 = 32;

#[derive(Clone, Copy, Debug)]
pub struct AtomicOptions {
    /// Temp inode is created owner-only (0600 / owner DACL) and the result stays owner-only.
    pub private: bool,
    /// Create missing parent directories first (owner-only when `private`).
    pub create_parents: bool,
    /// `fsync` the file and its directory. Disable only for rebuildable data.
    pub durable: bool,
}

impl Default for AtomicOptions {
    fn default() -> Self {
        Self {
            private: false,
            create_parents: false,
            durable: true,
        }
    }
}

impl AtomicOptions {
    pub fn private() -> Self {
        Self {
            private: true,
            ..Self::default()
        }
    }
}

/// Atomically replace `path` with `bytes`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_with(path, AtomicOptions::default(), |f| f.write_all(bytes))
}

/// Atomically replace `path` with `bytes`, owner-only.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_with(path, AtomicOptions::private(), |f| f.write_all(bytes))
}

/// Atomically replace `path` with whatever `fill` writes. If `fill` fails, the destination is
/// untouched and the temp file is removed.
pub fn atomic_write_with(
    path: &Path,
    options: AtomicOptions,
    fill: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> Result<()> {
    let mut writer = AtomicWriter::create(path, options)?;
    fill(writer.file_mut()).map_err(|e| FsError::io(writer.temp_path(), e))?;
    writer.commit()
}

/// A streaming atomic write. Nothing is visible at the destination until [`commit`]; dropping
/// an uncommitted writer removes its temp file. A process killed mid-write leaves only the
/// temp file behind.
///
/// [`commit`]: AtomicWriter::commit
#[derive(Debug)]
pub struct AtomicWriter {
    file: Option<File>,
    temp: PathBuf,
    target: PathBuf,
    options: AtomicOptions,
}

impl AtomicWriter {
    pub fn create(target: &Path, options: AtomicOptions) -> Result<Self> {
        let parent = parent_of(target);
        if options.create_parents {
            if options.private {
                crate::private::ensure_private_dir(&parent)?;
            } else {
                fs::create_dir_all(&parent).map_err(|e| FsError::io(&parent, e))?;
            }
        }
        // Keep the mode of an existing regular destination (never a symlink's target).
        #[cfg(unix)]
        let existing_mode = match fs::symlink_metadata(target) {
            Ok(m) if m.is_file() => {
                use std::os::unix::fs::PermissionsExt;
                Some(m.permissions().mode() & 0o7777)
            }
            _ => None,
        };
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| FsError::BadName(target.display().to_string()))?;
        let mut last = None;
        for attempt in 0..TEMP_ATTEMPTS {
            let temp = parent.join(temp_name(&name, attempt));
            let mut open = OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mode = if options.private {
                    0o600
                } else {
                    existing_mode.unwrap_or(0o666)
                };
                open.mode(mode)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            match open.open(&temp) {
                Ok(file) => {
                    #[cfg(not(unix))]
                    if options.private {
                        if let Err(e) = crate::private::restrict_owner_only(&temp) {
                            let _ = fs::remove_file(&temp);
                            return Err(e);
                        }
                    }
                    #[cfg(unix)]
                    if let (false, Some(mode)) = (options.private, existing_mode) {
                        // `mode()` above is masked by umask; restore the exact prior bits.
                        use std::os::unix::fs::PermissionsExt;
                        let _ = file.set_permissions(fs::Permissions::from_mode(mode));
                    }
                    return Ok(Self {
                        file: Some(file),
                        temp,
                        target: target.to_path_buf(),
                        options,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(FsError::io(&temp, e)),
            }
        }
        Err(FsError::io(
            target,
            last.unwrap_or_else(|| std::io::Error::other("temp name collisions")),
        ))
    }

    pub fn temp_path(&self) -> &Path {
        &self.temp
    }

    pub fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("writer is open until commit")
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let temp = self.temp.clone();
        self.file_mut()
            .write_all(bytes)
            .map_err(|e| FsError::io(temp, e))
    }

    /// Publish: flush, `fsync`, rename over the target, `fsync` the directory.
    pub fn commit(mut self) -> Result<()> {
        let file = self.file.take().expect("writer is open until commit");
        let result = (|| -> Result<()> {
            if self.options.durable {
                file.sync_all().map_err(|e| FsError::io(&self.temp, e))?;
            }
            drop(file);
            fs::rename(&self.temp, &self.target).map_err(|e| FsError::io(&self.target, e))?;
            if self.options.durable {
                sync_dir(&parent_of(&self.target));
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&self.temp);
        }
        // Either published or cleaned up: nothing left for Drop.
        self.temp = PathBuf::new();
        result
    }
}

impl Drop for AtomicWriter {
    fn drop(&mut self) {
        if !self.temp.as_os_str().is_empty() {
            self.file.take();
            let _ = fs::remove_file(&self.temp);
        }
    }
}

fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn temp_name(name: &str, attempt: u32) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // `<pid>` is parsed back by `sweep_stale_temps` to tell a crashed writer from a live one.
    format!(
        ".{name}{TEMP_MARKER}{}-{n}-{nanos:x}-{attempt}",
        std::process::id()
    )
}

/// Best-effort directory `fsync` so a rename itself survives power loss (no-op off unix).
pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Remove orphaned temp files in `dir` left by crashed writers. A temp file is reclaimed when
/// the process that created it is gone, or when it is older than `max_age` (pid reuse guard).
/// Only names containing [`TEMP_MARKER`] are ever considered. Returns the number removed.
pub fn sweep_stale_temps(dir: &Path, max_age: Duration) -> Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(FsError::io(dir, e)),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = temp_owner_pid(&name) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if ((pid != std::process::id() && !crate::process_alive(pid)) || old)
            && fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

fn temp_owner_pid(name: &str) -> Option<u32> {
    let rest = &name[name.find(TEMP_MARKER)? + TEMP_MARKER.len()..];
    rest.split('-').next()?.parse().ok()
}
