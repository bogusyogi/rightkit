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

/// Floor of the time one verified copy of a database may take.
const COPY_BUDGET_FLOOR: Duration = Duration::from_secs(120);
/// Pessimistic sustained rate (bytes per second) that grows the copy budget with the database.
const COPY_BYTES_PER_SECOND: u64 = 4 * 1024 * 1024;
/// The copy budget never exceeds this, so a stalled copy always ends.
const COPY_BUDGET_CEILING: Duration = Duration::from_secs(6 * 60 * 60);

/// The finite budget for one verified copy of the database behind `source`: a floor plus time
/// proportional to its size (database and `-wal`), capped. An in-memory database, or one whose
/// size cannot be read, counts as zero bytes, so the floor still applies.
///
/// This is the whole-copy deadline for every copy this crate makes for its own callers (restore,
/// the encrypted store's `backup_to`, the pre-migration backup, relocation). It is not derived
/// from `DbOptions::busy_timeout`: that is SQLite's lock wait (five seconds by default), and a
/// copy that creates a staging file, derives the SQLCipher key, writes, syncs and verifies can
/// take longer than that on a slow or scanned disk without anything being wrong.
pub(crate) fn copy_budget(source: &Connection) -> Duration {
    let bytes = source
        .path()
        .filter(|path| !path.is_empty())
        .map_or(0, |path| sized_copy_bytes(Path::new(path)));
    budget_for_bytes(bytes)
}

/// Bytes of the database at `main` and of its `-wal`; an unreadable file counts as zero.
fn sized_copy_bytes(main: &Path) -> u64 {
    ["", "-wal"].iter().fold(0u64, |total, suffix| {
        let mut name = main.as_os_str().to_os_string();
        name.push(suffix);
        let len = std::fs::metadata(PathBuf::from(name)).map_or(0, |meta| meta.len());
        total.saturating_add(len)
    })
}

fn budget_for_bytes(bytes: u64) -> Duration {
    COPY_BUDGET_FLOOR
        .saturating_add(Duration::from_secs(bytes / COPY_BYTES_PER_SECOND))
        .min(COPY_BUDGET_CEILING)
}

/// One uninterrupted spell of busy or locked steps. `busy_since` is when the spell began (`None`
/// while the copy is making progress); every step that makes progress ends the spell, so a copy
/// that is merely slow never spends a wait that belongs to a source that is stuck. Returns how
/// much of `limit` is left at `now`, or, once `limit` is spent, how long the spell has lasted.
fn busy_wait_left(
    busy_since: &mut Option<Instant>,
    now: Instant,
    limit: Duration,
) -> std::result::Result<Duration, Duration> {
    let waited = now.saturating_duration_since(*busy_since.get_or_insert(now));
    if waited >= limit {
        Err(waited)
    } else {
        Ok(limit.saturating_sub(waited))
    }
}

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
///
/// That makes `options.busy_timeout` the time allowed for the **whole** copy, five seconds by
/// default. A caller that copies a database of any size, or onto a slow or scanned disk, should
/// not rely on that: restore, the encrypted store's `backup_to`, the pre-migration backup and
/// relocation bound the copy by a budget derived from the size of the source instead, and use
/// `busy_timeout` only for how long a busy or locked source is waited for.
pub fn online_backup_to(
    options: &DbOptions,
    source: &Connection,
    target: &Path,
    progress: Option<&mut dyn BackupProgress>,
) -> Result<()> {
    backup_within(options, source, target, options.busy_timeout, progress)
}

/// [`online_backup_to`] as this crate's own operations make it. The whole copy is bounded by
/// [`copy_budget`] (never less than `options.busy_timeout`, so a caller can still allow longer),
/// and `options.busy_timeout` bounds only an uninterrupted spell of busy or locked steps, which
/// restarts after every step that makes progress. A copy that is merely slow is not a busy
/// source; a source that stays busy still fails after `busy_timeout`.
pub(crate) fn online_backup_sized(
    options: &DbOptions,
    source: &Connection,
    target: &Path,
    progress: Option<&mut dyn BackupProgress>,
) -> Result<()> {
    let budget = copy_budget(source).max(options.busy_timeout);
    backup_within(options, source, target, budget, progress)
}

/// The copy itself. `budget` is the monotonic deadline for the whole operation, checked before
/// every step and backoff; `options.busy_timeout` bounds one uninterrupted spell of busy or
/// locked steps inside it, and nothing here lets either bound grow.
fn backup_within(
    options: &DbOptions,
    source: &Connection,
    target: &Path,
    budget: Duration,
    mut progress: Option<&mut dyn BackupProgress>,
) -> Result<()> {
    let started = Instant::now();
    let deadline = started.checked_add(budget).unwrap_or(started);
    let clock = || -> Instant {
        {
            Instant::now()
        }
    };
    let check_deadline = || -> Result<()> {
        let now = clock();
        if now >= deadline {
            Err(DbError::Backup(format!(
                "backup operation timeout expired after {:?} (budget {:?})",
                now.saturating_duration_since(started),
                budget
            )))
        } else {
            Ok(())
        }
    };
    // Start of the current uninterrupted spell of busy or locked steps, if one is running.
    let mut busy_since: Option<Instant> = None;
    // Key validation first: an invalid/unavailable key creates no staging at all.
    let key = options.key_pragma_value()?;
    check_deadline()?;
    let (temp, created) = create_private_staging(target)?;
    let result = (|| -> Result<()> {
        // The staged file must still be the one this call exclusively created.
        // A scanner that is looking at the new file for a moment can make the identity open fail
        // with a transient Windows refusal; that is retried, and what still cannot be read after
        // the bound is reported as unreadable, not as a replaced file.
        if let Some(want) = created {
            match rightkit_fs::transient::retry_transient_fs(|| {
                rightkit_fs::FileKey::of_path(&temp)
            }) {
                Ok(now) if now == want => {}
                Ok(_) => {
                    return Err(DbError::Backup(
                        "backup staging was replaced before it was opened".into(),
                    ))
                }
                Err(error) => {
                    return Err(DbError::Backup(format!(
                        "backup staging could not be identified before it was opened: {error}"
                    )))
                }
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
                    StepResult::More => busy_since = None,
                    // Source busy/locked: back off, but only for `busy_timeout` without progress
                    // and never past the operation deadline.
                    _ => {
                        let now = clock();
                        let left = busy_wait_left(&mut busy_since, now, options.busy_timeout)
                            .map_err(|waited| {
                                DbError::Backup(format!(
                                    "backup busy timeout expired after {waited:?} (busy wait {:?})",
                                    options.busy_timeout
                                ))
                            })?;
                        let remaining = left.min(deadline.saturating_duration_since(now));
                        if remaining.is_zero() {
                            // Only the operation deadline can leave nothing to wait for. Report it
                            // as the error; this arm must never end the closure with `Ok`, which
                            // would go on to publish a copy that was not finished.
                            check_deadline()?;
                            return Err(DbError::Backup(format!(
                                "backup stopped waiting for a busy source at the operation \
                                 deadline (budget {budget:?})"
                            )));
                        }
                        let pause = remaining.min(Duration::from_millis(25));
                        std::thread::sleep(pause);
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
        // The staged copy is finished and verified. An indexer or scanner that still holds it on
        // Windows makes the next open or rename fail with a sharing or access error that clears
        // by itself, so both steps retry that class a bounded number of times, and only while the
        // staging is still the file this call created.
        let mut synced_before = false;
        // Windows FlushFileBuffers needs a writable handle; a read-only open fails "Access is denied".
        rightkit_fs::transient::retry_transient(|| {
            if std::mem::replace(&mut synced_before, true) {
                staging_still_ours(&temp, created)?;
            }
            std::fs::OpenOptions::new()
                .write(true)
                .open(&temp)
                .and_then(|f| f.sync_all())
        })
        .map_err(|e| DbError::Backup(format!("fsync backup: {e}")))?;
        check_deadline()?;
        let mut published_before = false;
        rightkit_fs::transient::retry_transient(|| {
            if std::mem::replace(&mut published_before, true) {
                staging_still_ours(&temp, created)?;
            }
            std::fs::rename(&temp, target)
        })
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

/// A retry of a publish step may only touch the staging this call created: a staging file whose
/// identity changed during the pause belongs to someone else and is left alone.
fn staging_still_ours(temp: &Path, created: Option<rightkit_fs::FileKey>) -> std::io::Result<()> {
    if let (Some(want), Ok(now)) = (created, rightkit_fs::FileKey::of_path(temp)) {
        if now != want {
            return Err(std::io::Error::other(
                "backup staging was replaced while a publish step was retried",
            ));
        }
    }
    Ok(())
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
    // `busy_timeout` bounds one uninterrupted spell of busy or locked steps, and restarts after
    // every step that makes progress. It used to run from the start of the whole copy, so a busy
    // spell that began long into a slow restore failed at once.
    let mut busy_since: Option<Instant> = None;
    loop {
        match backup.step(512)? {
            StepResult::Done => return Ok(()),
            StepResult::More => busy_since = None,
            StepResult::Busy | StepResult::Locked => {
                let left = busy_wait_left(&mut busy_since, Instant::now(), options.busy_timeout)
                    .map_err(|waited| {
                        DbError::Backup(format!(
                            "restore busy timeout expired after {waited:?} (busy wait {:?})",
                            options.busy_timeout
                        ))
                    })?;
                std::thread::sleep(left.min(Duration::from_millis(25)));
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
        online_backup_sized(options, &source, &staging, None)?;
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
