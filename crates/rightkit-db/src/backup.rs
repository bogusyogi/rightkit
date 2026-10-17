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
use std::time::{Duration, Instant};

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
        dst.busy_timeout(options.busy_timeout)?;
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

/// Validate `backup_path` before copying it into an already-open `destination`.
/// Uses the SQLite backup API, including committed WAL contents. `options` supplies
/// the backup's key and timeout; its path is ignored. Invalid backups never touch
/// `destination`. The destination must be writable and outside a transaction.
pub fn restore_from(
    options: &DbOptions,
    destination: &mut Connection,
    backup_path: &Path,
) -> Result<()> {
    let source = validated_backup(options, backup_path)?;
    restore_validated(options, &source, destination)
}

impl DbOptions {
    /// Validate the backup first, then open the existing live database without
    /// bootstrap and restore it. Neither missing backup nor missing live DB is created.
    /// The backup must use the same SQLCipher key as these options.
    pub fn restore_from(&self, backup_path: impl AsRef<Path>) -> Result<()> {
        let source = validated_backup(self, backup_path.as_ref())?;
        let mut destination = self.open_existing_writer()?;
        restore_validated(self, &source, &mut destination)
    }
}

fn validated_backup(options: &DbOptions, path: &Path) -> Result<Connection> {
    let source = options.open_existing_at(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // Keep the checked snapshot stable until Backup has finished reading it.
    source.execute_batch("BEGIN;")?;
    let messages = {
        let mut statement = source.prepare("PRAGMA integrity_check")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if messages.is_empty()
        || messages
            .iter()
            .any(|message| !message.trim().eq_ignore_ascii_case("ok"))
    {
        return Err(DbError::Integrity(messages.join("; ")));
    }
    Ok(source)
}

fn restore_validated(
    options: &DbOptions,
    source: &Connection,
    destination: &mut Connection,
) -> Result<()> {
    let backup = Backup::new(source, destination)?;
    let started = Instant::now();
    loop {
        match backup.step(512)? {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            StepResult::Busy | StepResult::Locked => {
                if started.elapsed() >= options.busy_timeout {
                    return Err(DbError::Backup("restore busy timeout expired".into()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => return Err(DbError::Backup("unexpected restore step result".into())),
        }
    }
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
    let _ = remove_database_files(path);
}

/// Delete a database file and its `-wal`, `-shm` and `-journal` sidecars.
///
/// A file that does not exist is not an error. Every file is attempted even if an earlier one
/// fails; the first real failure is returned. On Windows a file still open by a connection
/// cannot be deleted, so drop the writer connection and close the read pool first
/// (`ReadPool::close_and_wait` releases every pooled reader).
pub fn remove_database_files(path: &Path) -> std::io::Result<()> {
    let mut first_error = None;
    let mut remove = |p: &Path| {
        if let Err(e) = std::fs::remove_file(p) {
            if e.kind() != std::io::ErrorKind::NotFound && first_error.is_none() {
                first_error = Some(e);
            }
        }
    };
    remove(path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        remove(&PathBuf::from(name));
    }
    first_error.map_or(Ok(()), Err)
}
