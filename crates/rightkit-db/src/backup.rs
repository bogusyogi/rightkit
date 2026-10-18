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
///
/// The key is validated before any staging exists, and the staging file is created
/// owner-only with `create_private_new` before SQLite opens it, so plaintext is never
/// exposed under a permissive parent. The target is untouched until the staged copy has
/// passed `quick_check`, been synced, and is published by rename. Busy/locked steps retry
/// only until a finite monotonic operation deadline (`options.busy_timeout`,
/// checked before every step and backoff, with or without a progress callback); every
/// sleep is capped to the remaining budget, and cancellation or expiry removes only this
/// call's still-owned staging.
pub fn online_backup_to(
    options: &DbOptions,
    source: &Connection,
    target: &Path,
    mut progress: Option<&mut dyn BackupProgress>,
) -> Result<()> {
    let started = Instant::now();
    let deadline = started.checked_add(options.busy_timeout).unwrap_or(started);
    let check_deadline = || -> Result<()> {
        if Instant::now() >= deadline {
            Err(DbError::Backup("backup operation timeout expired".into()))
        } else {
            Ok(())
        }
    };
    // Key validation first: an invalid/unavailable key creates no staging at all.
    let key = options.key_pragma_value()?;
    check_deadline()?;
    let (temp, created) = create_private_staging(target)?;
    let result = (|| -> Result<()> {
        // The staged file must still be the one this call exclusively created.
        if let Some(want) = created {
            if rightkit_fs::FileKey::of_path(&temp).ok() != Some(want) {
                return Err(DbError::Backup(
                    "backup staging was replaced before it was opened".into(),
                ));
            }
        }
        let mut dst = Connection::open(&temp)?;
        // Never let SQLite spend a second independent timeout inside a backup step.
        dst.busy_timeout(Duration::ZERO)?;
        apply_key_first(&dst, key.as_ref().map(|k| k.as_str()))?;
        {
            let backup = Backup::new(source, &mut dst)?;
            loop {
                check_deadline()?;
                let step = backup.step(512)?;
                check_deadline()?;
                if let Some(sink) = progress.as_deref_mut() {
                    if !sink.step(backup.progress().remaining) {
                        return Err(DbError::Backup("cancelled".into()));
                    }
                }
                check_deadline()?;
                match step {
                    StepResult::Done => break,
                    StepResult::More => {}
                    // Source busy/locked: back off within the same operation deadline.
                    _ => {
                        let now = Instant::now();
                        let remaining = deadline.saturating_duration_since(now);
                        if remaining.is_zero() {
                            return Err(DbError::Backup("backup busy timeout expired".into()));
                        }
                        std::thread::sleep(remaining.min(Duration::from_millis(25)));
                    }
                }
            }
        }
        check_deadline()?;
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
        check_deadline()?;
        std::fs::rename(&temp, target)
            .map_err(|e| DbError::Backup(format!("publish backup: {e}")))?;
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            rightkit_fs::atomic::sync_dir(parent);
        }
        Ok(())
    })();
    if result.is_err() {
        remove_owned_staging(&temp, created);
    }
    result
}

/// Exclusively creates a unique owner-only staging file beside `target`, retrying on a
/// name collision, and returns its path with the identity captured at creation. The file
/// is private from its first observation, before SQLite opens it.
fn create_private_staging(target: &Path) -> Result<(PathBuf, Option<rightkit_fs::FileKey>)> {
    for _ in 0..32 {
        let temp = temp_path(target)?;
        match rightkit_fs::create_private_new(&temp) {
            Ok(file) => {
                let key = rightkit_fs::FileKey::of_file(&file, &temp).ok();
                drop(file);
                return Ok((temp, key));
            }
            Err(error) if error.kind() == Some(std::io::ErrorKind::AlreadyExists) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(DbError::Backup(
        "could not create a unique backup staging file".into(),
    ))
}

/// Removes this call's staging family only while the main staged file still has the
/// identity captured at its exclusive creation (or is already gone). A replaced staging
/// file is another owner's and is retained whole.
fn remove_owned_staging(temp: &Path, created: Option<rightkit_fs::FileKey>) {
    if let Some(want) = created {
        match rightkit_fs::FileKey::of_path(temp) {
            Ok(now) if now != want => return,
            _ => {}
        }
    }
    remove_with_sidecars(temp);
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

pub(crate) fn validated_backup(options: &DbOptions, path: &Path) -> Result<Connection> {
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
    let deadline = started.checked_add(options.busy_timeout);
    loop {
        match backup.step(512)? {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            StepResult::Busy | StepResult::Locked => {
                let Some(deadline) = deadline else {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                };
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(DbError::Backup("restore busy timeout expired".into()));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
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

/// The lock file that guards the whole store behind the database at `db_path`:
/// `<db_path>.store-lock`.
pub fn store_lock_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(".store-lock");
    PathBuf::from(name)
}

/// A held store-wide lock (see [`store_lock_path`]). Released on drop.
///
/// Processes that use the database (daemon, shell) hold a [`StoreLock::shared`] guard for as long
/// as they keep connections open; [`replace_database`] takes the [`StoreLock::exclusive`] guard,
/// so a replace runs only when no cooperating process is using the store. The lock is advisory
/// and cooperative: a process that never takes the shared guard is not excluded.
#[derive(Debug)]
pub struct StoreLock {
    lock: rightkit_fs::FileLock,
}

impl StoreLock {
    /// Shared guard: any number of holders, excluded only by an exclusive holder. Waits per
    /// `options.timeout` and returns `DbError::Fs` (locked or timed out) when busy.
    pub fn shared(db_path: &Path, options: &rightkit_fs::LockOptions) -> Result<Self> {
        let lock = rightkit_fs::FileLock::acquire_shared(&store_lock_path(db_path), options)?;
        Ok(Self { lock })
    }

    /// Exclusive guard: excludes every shared and exclusive holder. Waits per `options.timeout`.
    pub fn exclusive(db_path: &Path, options: &rightkit_fs::LockOptions) -> Result<Self> {
        let lock = rightkit_fs::FileLock::acquire(&store_lock_path(db_path), options)?;
        Ok(Self { lock })
    }

    /// Path of the lock file.
    pub fn path(&self) -> &Path {
        self.lock.path()
    }
}

/// Atomically replace the database at `options.path` with the database at `replacement`.
///
/// Under the store-wide exclusive lock ([`StoreLock::exclusive`], waiting per `lock`):
/// 1. `replacement` is opened read-only with `options`' key and every `PRAGMA integrity_check`
///    row must be `ok` (`DbError::Integrity` otherwise; the live database is untouched).
/// 2. Its committed contents (WAL included) are copied through SQLite's backup API into a
///    staging file beside the live database, checked, narrowed to owner-only and synced.
/// 3. The live database's `-wal`, `-shm` and `-journal` sidecars are removed, so a stale log
///    can never be replayed onto the new file, and the staging file is renamed onto the path.
///
/// A failure in step 1 or 2 leaves the live database untouched and removes the staging file. The
/// only crash window is between removing the sidecars and the rename in step 3: the old main
/// file is then still in place without its uncheckpointed log, and the staging file remains
/// (reclaimed by `rightkit_fs::sweep_stale_temps`). The replacement keeps the options' key, so
/// it must be encrypted with the same key.
///
/// Every connection to the live database, in this process and in any other, must be closed
/// before the call: other processes by holding only [`StoreLock::shared`] guards (the exclusive
/// guard cannot be taken while one is held), this process by dropping its writer and calling
/// `ReadPool::close_and_wait`. On Windows an open connection also makes the rename fail.
pub fn replace_database(
    options: &DbOptions,
    replacement: &Path,
    lock: &rightkit_fs::LockOptions,
) -> Result<()> {
    let _guard = StoreLock::exclusive(&options.path, lock)?;
    replace_database_locked(options, replacement)
}

/// [`replace_database`] for a caller that already holds the exclusive [`StoreLock`] for
/// `options.path` (for example to do more work under the same lock).
pub fn replace_database_locked(options: &DbOptions, replacement: &Path) -> Result<()> {
    let target = options.path.as_path();
    let source = validated_backup(options, replacement)?;
    let staging = temp_path(target)?;
    let staged = (|| -> Result<()> {
        online_backup_to(options, &source, &staging, None)?;
        drop(source);
        remove_database_files_except_main(target)
            .map_err(|e| DbError::Backup(format!("clear live sidecars: {e}")))?;
        std::fs::rename(&staging, target)
            .map_err(|e| DbError::Backup(format!("publish replacement: {e}")))?;
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            rightkit_fs::atomic::sync_dir(parent);
        }
        Ok(())
    })();
    if staged.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    staged
}

fn remove_database_files_except_main(path: &Path) -> std::io::Result<()> {
    let mut first_error = None;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        if let Err(e) = std::fs::remove_file(PathBuf::from(name)) {
            if e.kind() != std::io::ErrorKind::NotFound && first_error.is_none() {
                first_error = Some(e);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}
