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
    #[cfg(test)]
    deadline_tests::start(started);
    let deadline = started.checked_add(budget).unwrap_or(started);
    let clock = || -> Instant {
        #[cfg(test)]
        {
            deadline_tests::now()
        }
        #[cfg(not(test))]
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
                #[cfg(test)]
                let step = deadline_tests::step(&backup)?;
                #[cfg(not(test))]
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
                        #[cfg(test)]
                        deadline_tests::pause(pause);
                        #[cfg(not(test))]
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

// Only unit tests can substitute step outcomes/time. Native integration tests always use SQLite
// & Instant directly; release builds contain neither this state nor the hooks above.
#[cfg(test)]
mod deadline_tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct Script {
        started: Instant,
        elapsed: Duration,
        steps: VecDeque<(Duration, rusqlite::Result<StepResult>)>,
        calls: usize,
    }

    thread_local! {
        static SCRIPT: RefCell<Option<Script>> = const { RefCell::new(None) };
    }

    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SCRIPT.with(|script| *script.borrow_mut() = None);
        }
    }

    pub(super) fn start(started: Instant) {
        SCRIPT.with(|script| {
            if let Some(script) = script.borrow_mut().as_mut() {
                script.started = started;
            }
        });
    }

    pub(super) fn now() -> Instant {
        SCRIPT.with(|script| {
            script
                .borrow()
                .as_ref()
                .map_or_else(Instant::now, |script| script.started + script.elapsed)
        })
    }

    /// A backoff: with a script it only advances the scripted clock, without one it sleeps.
    pub(super) fn pause(duration: Duration) {
        let scripted = SCRIPT.with(|script| {
            script.borrow_mut().as_mut().map(|script| {
                script.elapsed += duration;
            })
        });
        if scripted.is_none() {
            std::thread::sleep(duration);
        }
    }

    pub(super) fn step(backup: &Backup<'_, '_>) -> rusqlite::Result<StepResult> {
        let scripted = SCRIPT.with(|script| {
            script.borrow_mut().as_mut().map(|script| {
                let (elapsed, result) = script.steps.pop_front().expect("unexpected backup step");
                script.elapsed += elapsed;
                script.calls += 1;
                result
            })
        });
        scripted.unwrap_or_else(|| backup.step(512))
    }

    struct Progress {
        calls: usize,
        elapsed: Duration,
    }

    impl BackupProgress for Progress {
        fn step(&mut self, _remaining: i32) -> bool {
            self.calls += 1;
            SCRIPT.with(|script| {
                script.borrow_mut().as_mut().unwrap().elapsed += self.elapsed;
            });
            true
        }
    }

    fn run(
        steps: Vec<(Duration, rusqlite::Result<StepResult>)>,
        callback_time: Option<Duration>,
    ) -> (Result<()>, usize, usize) {
        run_from(Duration::ZERO, steps, callback_time)
    }

    fn run_from(
        initial_elapsed: Duration,
        steps: Vec<(Duration, rusqlite::Result<StepResult>)>,
        callback_time: Option<Duration>,
    ) -> (Result<()>, usize, usize) {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("backup.sqlite");
        std::fs::write(&target, b"previous backup").unwrap();
        let source = Connection::open_in_memory().unwrap();
        source.execute_batch("CREATE TABLE t(v);").unwrap();
        let mut options = DbOptions::new("unused.sqlite");
        options.busy_timeout = Duration::from_millis(50);
        SCRIPT.with(|script| {
            assert!(script.borrow().is_none());
            *script.borrow_mut() = Some(Script {
                started: Instant::now(),
                elapsed: initial_elapsed,
                steps: steps.into(),
                calls: 0,
            });
        });
        let _reset = Reset;
        let mut progress = Progress {
            calls: 0,
            elapsed: callback_time.unwrap_or_default(),
        };
        let callback = callback_time.map(|_| &mut progress as &mut dyn BackupProgress);
        let result = online_backup_to(&options, &source, &target, callback);
        let calls = SCRIPT.with(|script| script.borrow().as_ref().unwrap().calls);
        assert_eq!(std::fs::read(&target).unwrap(), b"previous backup");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "owned staging must be removed"
        );
        (result, calls, progress.calls)
    }

    fn assert_timeout(result: &Result<()>) {
        assert!(
            matches!(result, Err(DbError::Backup(message)) if message.contains("timeout")),
            "expected total operation deadline: {result:?}",
        );
    }

    #[test]
    fn expired_operation_never_starts_first_step() {
        let (result, steps, callbacks) =
            run_from(Duration::from_millis(50), Vec::new(), Some(Duration::ZERO));
        assert_timeout(&result);
        assert_eq!((steps, callbacks), (0, 0));
    }

    #[test]
    fn expiry_message_names_the_elapsed_time_and_the_budget() {
        let (result, _, _) = run_from(Duration::from_millis(50), Vec::new(), Some(Duration::ZERO));
        match result {
            Err(DbError::Backup(message)) => {
                assert!(message.contains("after 50ms"), "{message}");
                assert!(message.contains("budget 50ms"), "{message}");
            }
            other => panic!("expected the total deadline, got {other:?}"),
        }
    }

    #[test]
    fn recurring_progress_does_not_reset_total_deadline() {
        // Two callbacks must occur because this test controls step cost. A third completed
        // chunk crosses the original 50 ms deadline; progress never grants another budget.
        let steps = (0..3)
            .map(|_| (Duration::from_millis(20), Ok(StepResult::More)))
            .collect();
        let (result, steps, callbacks) = run(steps, Some(Duration::ZERO));
        assert_timeout(&result);
        assert_eq!((steps, callbacks), (3, 2));
    }

    #[test]
    fn completed_step_cannot_publish_after_callback_expires_deadline() {
        let (result, steps, callbacks) = run(
            vec![(Duration::ZERO, Ok(StepResult::Done))],
            Some(Duration::from_millis(50)),
        );
        assert_timeout(&result);
        assert_eq!((steps, callbacks), (1, 1));
    }

    #[test]
    fn expired_step_stops_before_callback() {
        let (result, steps, callbacks) = run(
            vec![(Duration::from_millis(50), Ok(StepResult::Done))],
            Some(Duration::ZERO),
        );
        assert_timeout(&result);
        assert_eq!((steps, callbacks), (1, 0));
    }

    #[test]
    fn progressing_steps_without_callback_share_same_deadline() {
        let steps = (0..3)
            .map(|_| (Duration::from_millis(20), Ok(StepResult::More)))
            .collect();
        let (result, steps, callbacks) = run(steps, None);
        assert_timeout(&result);
        assert_eq!((steps, callbacks), (3, 0));
    }

    #[test]
    fn step_error_cleans_staging_without_callback() {
        let (result, steps, callbacks) = run(
            vec![(Duration::ZERO, Err(rusqlite::Error::InvalidQuery))],
            Some(Duration::ZERO),
        );
        assert!(matches!(
            result,
            Err(DbError::Sqlite(rusqlite::Error::InvalidQuery))
        ));
        assert_eq!((steps, callbacks), (1, 0));
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    type Steps = Vec<(Duration, rusqlite::Result<StepResult>)>;

    fn repeated(count: usize, cost: Duration, result: StepResult) -> Steps {
        (0..count).map(|_| (cost, Ok(result))).collect()
    }

    /// `backup_within` under the scripted clock with an explicit whole-copy `budget` and
    /// `busy` wait. Returns the result, the steps taken, whether the previous backup at the target
    /// survived, and how many entries the folder holds afterwards (1 = no staging left behind).
    fn run_budgeted(
        budget: Duration,
        busy: Duration,
        steps: Steps,
    ) -> (Result<()>, usize, bool, usize) {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("backup.sqlite");
        std::fs::write(&target, b"previous backup").unwrap();
        let source = Connection::open_in_memory().unwrap();
        source.execute_batch("CREATE TABLE t(v);").unwrap();
        let mut options = DbOptions::new("unused.sqlite");
        options.busy_timeout = busy;
        SCRIPT.with(|script| {
            assert!(script.borrow().is_none());
            *script.borrow_mut() = Some(Script {
                started: Instant::now(),
                elapsed: Duration::ZERO,
                steps: steps.into(),
                calls: 0,
            });
        });
        let _reset = Reset;
        let result = backup_within(&options, &source, &target, budget, None);
        let calls = SCRIPT.with(|script| script.borrow().as_ref().unwrap().calls);
        let kept = std::fs::read(&target).is_ok_and(|bytes| bytes == b"previous backup");
        let entries = std::fs::read_dir(dir.path()).unwrap().count();
        (result, calls, kept, entries)
    }

    fn message(result: &Result<()>) -> String {
        match result {
            Err(DbError::Backup(message)) => message.clone(),
            other => panic!("expected a backup error, got {other:?}"),
        }
    }

    #[test]
    fn a_slow_copy_is_bounded_by_its_budget_and_the_message_names_it() {
        let slow = || repeated(15, ms(100), StepResult::More);
        // A budget of one second takes ten 100 ms steps to run out, however short the busy wait.
        let (result, steps, kept, entries) = run_budgeted(ms(1000), ms(50), slow());
        let text = message(&result);
        assert!(text.contains("operation timeout expired"), "{text}");
        assert!(text.contains("after 1s (budget 1s)"), "{text}");
        assert_eq!(steps, 10);
        assert!(kept, "the previous backup must survive");
        assert_eq!(entries, 1, "owned staging must be removed");
        // The same copy under the legacy contract (busy_timeout is the whole deadline) stops at 50 ms.
        let (result, steps, _, _) = run_budgeted(ms(50), ms(50), slow());
        assert!(message(&result).contains("budget 50ms"));
        assert_eq!(steps, 1);
    }

    #[test]
    fn a_zero_busy_wait_does_not_bound_a_copy_that_is_never_busy() {
        let steps = repeated(5, ms(100), StepResult::More)
            .into_iter()
            .chain([(ms(0), Ok(StepResult::Done))])
            .collect();
        let (result, calls, kept, entries) = run_budgeted(ms(10_000), Duration::ZERO, steps);
        result.unwrap();
        assert_eq!(calls, 6);
        assert!(!kept, "the finished copy replaces the previous backup");
        assert_eq!(entries, 1);
    }

    #[test]
    fn a_source_that_stays_busy_fails_after_the_busy_wait_not_the_budget() {
        let (result, steps, kept, entries) =
            run_budgeted(ms(10_000), ms(50), repeated(20, ms(10), StepResult::Busy));
        let text = message(&result);
        assert!(text.contains("busy timeout expired"), "{text}");
        assert!(text.contains("busy wait 50ms"), "{text}");
        assert!(!text.contains("operation timeout"), "{text}");
        assert_eq!(
            steps, 3,
            "10 ms step, 25 ms pause, 10 ms step, 15 ms pause, 10 ms step"
        );
        assert!(kept);
        assert_eq!(entries, 1);
    }

    #[test]
    fn a_zero_busy_wait_fails_on_the_first_busy_step() {
        let (result, steps, _, _) = run_budgeted(
            ms(10_000),
            Duration::ZERO,
            repeated(3, ms(10), StepResult::Locked),
        );
        assert!(message(&result).contains("busy timeout expired after 0ns"));
        assert_eq!(steps, 1);
    }

    #[test]
    fn progress_between_busy_spells_restarts_the_busy_wait() {
        // Three busy spells of 25 ms each, separated by steps that make progress: no spell reaches
        // the 50 ms busy wait, although the copy runs for 185 ms in all.
        let steps = vec![
            (ms(10), Ok(StepResult::Busy)),
            (ms(40), Ok(StepResult::More)),
            (ms(10), Ok(StepResult::Busy)),
            (ms(40), Ok(StepResult::More)),
            (ms(10), Ok(StepResult::Busy)),
            (ms(0), Ok(StepResult::Done)),
        ];
        let (result, calls, _, entries) = run_budgeted(ms(10_000), ms(50), steps);
        result.unwrap();
        assert_eq!(calls, 6);
        assert_eq!(entries, 1);
    }

    #[test]
    fn the_budget_caps_a_busy_wait_that_is_longer_than_it() {
        let (result, steps, kept, entries) =
            run_budgeted(ms(100), ms(10_000), repeated(30, ms(10), StepResult::Busy));
        let text = message(&result);
        assert!(text.contains("operation timeout expired"), "{text}");
        assert!(text.contains("after 100ms (budget 100ms)"), "{text}");
        assert_eq!(steps, 3);
        assert!(kept);
        assert_eq!(entries, 1);
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn tiny_database(path: &Path, rows: u32) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE marks(v INTEGER NOT NULL);")
            .unwrap();
        for value in 0..rows {
            conn.execute("INSERT INTO marks(v) VALUES (?1)", [value])
                .unwrap();
        }
        conn
    }

    fn count_rows(path: &Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row("SELECT count(*) FROM marks", [], |row| row.get(0))
            .unwrap()
    }

    fn zero_wait() -> DbOptions {
        let mut options = DbOptions::new("unused.sqlite");
        options.busy_timeout = Duration::ZERO;
        options
    }

    #[test]
    fn the_budget_is_a_floor_plus_time_for_the_size_and_is_capped() {
        assert_eq!(budget_for_bytes(0), Duration::from_secs(120));
        assert_eq!(budget_for_bytes(4 * MIB - 1), Duration::from_secs(120));
        assert_eq!(budget_for_bytes(4 * MIB), Duration::from_secs(121));
        assert_eq!(budget_for_bytes(240 * MIB), Duration::from_secs(180));
        assert_eq!(budget_for_bytes(u64::MAX), Duration::from_secs(6 * 60 * 60));
        let mut last = Duration::ZERO;
        for exponent in 0..64 {
            let budget = budget_for_bytes(1u64 << exponent);
            assert!(budget >= last, "2^{exponent} bytes shrank the budget");
            assert!(budget <= COPY_BUDGET_CEILING);
            last = budget;
        }
    }

    #[test]
    fn the_size_counts_the_database_and_its_wal_and_ignores_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("app.sqlite");
        assert_eq!(sized_copy_bytes(&main), 0);
        std::fs::write(&main, vec![0u8; 1000]).unwrap();
        assert_eq!(sized_copy_bytes(&main), 1000);
        std::fs::write(dir.path().join("app.sqlite-wal"), vec![0u8; 500]).unwrap();
        assert_eq!(sized_copy_bytes(&main), 1500);
        std::fs::write(dir.path().join("app.sqlite-shm"), vec![0u8; 9999]).unwrap();
        assert_eq!(sized_copy_bytes(&main), 1500, "-shm is not copied");
    }

    #[test]
    fn a_small_or_in_memory_source_gets_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        let file = tiny_database(&dir.path().join("app.sqlite"), 10);
        assert_eq!(copy_budget(&file), COPY_BUDGET_FLOOR);
        let memory = Connection::open_in_memory().unwrap();
        assert_eq!(copy_budget(&memory), COPY_BUDGET_FLOOR);
    }

    #[test]
    fn a_copy_that_outlasts_the_busy_wait_completes_under_its_own_budget() {
        let dir = tempfile::tempdir().unwrap();
        let source = tiny_database(&dir.path().join("app.sqlite"), 200);
        let options = zero_wait();

        // The public function keeps its contract: busy_timeout is the whole deadline, so a zero
        // wait cannot even start.
        let legacy = dir.path().join("legacy.sqlite");
        let error = online_backup_to(&options, &source, &legacy, None).unwrap_err();
        assert!(
            matches!(&error, DbError::Backup(m) if m.contains("operation timeout expired")),
            "{error:?}"
        );
        assert!(!legacy.exists());

        // The crate's own copies are bounded by the size-derived budget instead.
        let sized = dir.path().join("sized.sqlite");
        online_backup_sized(&options, &source, &sized, None).unwrap();
        assert_eq!(count_rows(&sized), 200);
    }

    #[test]
    fn a_source_that_stays_locked_fails_after_the_busy_wait_not_after_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        let source = tiny_database(&path, 10);
        source.busy_timeout(Duration::ZERO).unwrap();
        // A second connection holds the exclusive lock of a rollback-journal database.
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let mut options = DbOptions::new(&path);
        options.busy_timeout = Duration::from_millis(60);
        let target = dir.path().join("backup.sqlite");

        let started = Instant::now();
        let error = online_backup_sized(&options, &source, &target, None).unwrap_err();
        let waited = started.elapsed();

        let DbError::Backup(text) = &error else {
            panic!("expected a backup error, got {error:?}");
        };
        assert!(text.contains("busy timeout expired"), "{text}");
        assert!(text.contains("busy wait 60ms"), "{text}");
        assert!(waited >= Duration::from_millis(60), "{waited:?}");
        assert!(
            waited < Duration::from_secs(60),
            "the busy wait, not the 120 s budget, ended it: {waited:?}"
        );
        assert!(!target.exists());
        let staging = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(rightkit_fs::atomic::TEMP_MARKER))
            .collect::<Vec<_>>();
        assert!(staging.is_empty(), "staging left behind: {staging:?}");
        holder.execute_batch("ROLLBACK;").unwrap();
    }

    #[test]
    fn a_busy_spell_is_limited_and_progress_starts_the_wait_again() {
        let origin = Instant::now();
        let at = |millis: u64| origin + Duration::from_millis(millis);
        let limit = Duration::from_millis(50);
        let mut since = None;
        assert_eq!(
            busy_wait_left(&mut since, at(1_000), limit),
            Ok(Duration::from_millis(50))
        );
        assert_eq!(
            busy_wait_left(&mut since, at(1_020), limit),
            Ok(Duration::from_millis(30))
        );
        assert_eq!(
            busy_wait_left(&mut since, at(1_050), limit),
            Err(Duration::from_millis(50))
        );
        // A step that made progress ends the spell (the loops set `busy_since = None`), however
        // late into the copy the next busy step comes.
        since = None;
        assert_eq!(
            busy_wait_left(&mut since, at(90_000), limit),
            Ok(Duration::from_millis(50)),
            "the wait counts from the start of this spell, not from the start of the copy"
        );
        // No wait at all gives up at the first busy step.
        let mut since = None;
        assert_eq!(
            busy_wait_left(&mut since, at(5), Duration::ZERO),
            Err(Duration::ZERO)
        );
        // A clock that reads earlier than the start of the spell cannot underflow.
        let mut since = Some(at(100));
        assert_eq!(busy_wait_left(&mut since, at(10), limit), Ok(limit));
    }

    #[test]
    fn a_restore_into_a_destination_that_stays_locked_fails_after_the_busy_wait() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup.sqlite");
        drop(tiny_database(&backup, 10));
        let live = dir.path().join("live.sqlite");
        let mut destination = tiny_database(&live, 1);
        destination.busy_timeout(Duration::ZERO).unwrap();
        // Another connection holds the exclusive lock of the rollback-journal destination.
        let holder = Connection::open(&live).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let mut options = DbOptions::new(&live);
        options.busy_timeout = Duration::from_millis(60);

        let started = Instant::now();
        let error = restore_from(&options, &mut destination, &backup).unwrap_err();
        let waited = started.elapsed();

        let DbError::Backup(text) = &error else {
            panic!("expected a backup error, got {error:?}");
        };
        assert!(text.contains("restore busy timeout expired"), "{text}");
        assert!(text.contains("busy wait 60ms"), "{text}");
        assert!(waited >= Duration::from_millis(60), "{waited:?}");
        assert!(waited < Duration::from_secs(60), "{waited:?}");
        holder.execute_batch("ROLLBACK;").unwrap();
        drop(holder);
        assert_eq!(count_rows(&live), 1, "the live database was not touched");
        // Unlocked, the same restore completes.
        restore_from(&options, &mut destination, &backup).unwrap();
        assert_eq!(count_rows(&live), 10);
    }
}
