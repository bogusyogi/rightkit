//! Crash-safe online backups.
//!
//! SQLite's Online Backup API copies through the source connection's own pager, so committed
//! transactions still sitting in the `-wal` are included (a raw file copy would miss them).
//! The copy lands in a sibling temp file, is checked with `PRAGMA quick_check` (re-opened
//! with the same key for SQLCipher), narrowed to owner-only, `fsync`ed, and only then renamed
//! onto the target: a crash leaves the previous target untouched and at worst an orphan temp
//! file that `rightkit_fs::sweep_stale_temps` reclaims. Donor: CodeRight `private_fs::backup`.

use crate::{apply_key_first, DbError, DbOptions, Result};
use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub trait BackupProgress {
    /// Called after every step with the pages remaining; return `false` to cancel.
    fn step(&mut self, remaining: i32) -> bool;
}

/// Back `source` up to `target`. `options` supplies the SQLCipher key (if any) so the backup is
/// encrypted with the same key; its `path` is ignored.
pub fn online_backup_to(
    options: &DbOptions,
    source: &Connection,
    target: &Path,
    mut progress: Option<&mut dyn BackupProgress>,
) -> Result<()> {
    let key = options.key_pragma_value()?;
    let temp = temp_path(target)?;
    let result = (|| -> Result<()> {
        let mut dst = Connection::open(&temp)?;
        apply_key_first(&dst, key.as_ref().map(|k| k.as_str()))?;
        {
            let backup = Backup::new(source, &mut dst)?;
            loop {
                let step = backup.step(512)?;
                if let Some(sink) = progress.as_deref_mut() {
                    if !sink.step(backup.progress().remaining) {
                        return Err(DbError::Backup("cancelled".into()));
                    }
                }
                match step {
                    StepResult::Done => break,
                    StepResult::More => {}
                    // Source busy/locked: back off briefly and retry.
                    _ => std::thread::sleep(Duration::from_millis(25)),
                }
            }
        }
        // Leave one self-contained file: fold any WAL into the main file and drop WAL mode.
        let _ = dst.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        dst.query_row("PRAGMA journal_mode=DELETE", [], |row| {
            row.get::<_, String>(0)
        })?;
        let report: String = dst.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if !report.trim().eq_ignore_ascii_case("ok") {
            return Err(DbError::Backup(format!(
                "quick_check on the backup said: {report}"
            )));
        }
        drop(dst);
        rightkit_fs::restrict_owner_only(&temp)?;
        // Windows FlushFileBuffers needs a writable handle; a read-only open fails "Access is denied".
        std::fs::OpenOptions::new()
            .write(true)
            .open(&temp)
            .and_then(|f| f.sync_all())
            .map_err(|e| DbError::Backup(format!("fsync backup: {e}")))?;
        std::fs::rename(&temp, target)
            .map_err(|e| DbError::Backup(format!("publish backup: {e}")))?;
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            rightkit_fs::atomic::sync_dir(parent);
        }
        Ok(())
    })();
    if result.is_err() {
        remove_with_sidecars(&temp);
    }
    result
}

fn temp_path(target: &Path) -> Result<PathBuf> {
    let name = target
        .file_name()
        .ok_or(DbError::InvalidDatabasePath)?
        .to_string_lossy();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(target.with_file_name(format!(
        ".{name}{}{}-{n}",
        rightkit_fs::atomic::TEMP_MARKER,
        std::process::id()
    )))
}

fn remove_with_sidecars(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
}
