use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::options::{ReplaceMode, SharingRetry};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` so readers see the old or the new file, never a mix. Creates missing parent
/// directories, then replaces through `rightkit_fs::atomic_write` (temp file, `fsync`, rename,
/// directory `fsync`).
pub fn atomic_write(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_mode(dest, bytes, &WriteMode::default())
}

/// How the settings file is published. The default is today's behaviour.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteMode {
    pub replace: ReplaceMode,
    pub durable: bool,
    pub retry: Option<SharingRetry>,
}

impl Default for WriteMode {
    fn default() -> Self {
        Self {
            replace: ReplaceMode::Rename,
            durable: true,
            retry: None,
        }
    }
}

/// [`atomic_write`] with an explicit replace mode, durability flag, and sharing-violation retry.
pub(crate) fn atomic_write_mode(dest: &Path, bytes: &[u8], mode: &WriteMode) -> io::Result<()> {
    let parent = parent_of(dest)?;
    fs::create_dir_all(parent)?;
    let options = rightkit_fs::AtomicWriteOptions {
        durable: mode.durable,
        replace: match mode.replace {
            ReplaceMode::Rename => rightkit_fs::ReplaceMode::Rename,
            ReplaceMode::ReplaceFile => rightkit_fs::ReplaceMode::ReplaceFile,
            ReplaceMode::MoveWriteThrough => rightkit_fs::ReplaceMode::MoveWriteThrough,
        },
        retry_sharing_violation: mode
            .retry
            .map(|r| rightkit_fs::RetryPolicy::new(r.attempts, r.delay)),
        ..Default::default()
    };
    rightkit_fs::atomic_write_with_options(dest, options, |f| f.write_all(bytes)).map_err(into_io)
}

/// As [`atomic_write`] with an injectable replace step (used to prove the old file survives a
/// failed replace). `replace(temp, dest)` must move `temp` over `dest`; the default is
/// `fs::rename`, which replaces an existing file on every platform.
pub fn atomic_write_with(
    dest: &Path,
    bytes: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = parent_of(dest)?;
    fs::create_dir_all(parent)?;
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let (tmp, mut file) = loop {
        let cand = parent.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new().write(true).create_new(true).open(&cand) {
            Ok(f) => break (cand, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&tmp, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn parent_of(dest: &Path) -> io::Result<&Path> {
    dest.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))
}

fn into_io(e: rightkit_fs::FsError) -> io::Error {
    match e {
        rightkit_fs::FsError::Io { source, .. } => source,
        other => io::Error::other(other),
    }
}
