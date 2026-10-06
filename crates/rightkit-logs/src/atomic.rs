//! Crash-safe file replacement shared by consent and the outbox.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to a sibling temp file, fsync, then rename over `path`.
/// Readers see either the old or the new content, never a partial file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("rightkit");
    let temp = loop {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = path.with_file_name(format!(".{name}.tmp-{}-{counter}", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut file) => {
                let result = file.write_all(bytes).and_then(|_| file.sync_all());
                if let Err(error) = result {
                    let _ = fs::remove_file(&candidate);
                    return Err(error);
                }
                break candidate;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    sync_parent(path);
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}
