//! Cross-process arrival order for [`crate::JobLocks`] waiters.
//!
//! One directory per job id, `<locks dir>/<job id>.queue/`, holds the queue:
//!
//! - `queue.mutex`: a short-lived file lock that serialises ticket creation and the
//!   dead-ticket sweep. It is held for a handful of file operations, never while waiting.
//! - `<seq:020>-<pid>.ticket`: one file per waiter, locked (a `FileLock`) for as long as the
//!   waiter is in line. Order is the `seq` in the name.
//!
//! A ticket's lock is the liveness proof: a ticket that can be locked has no live owner (the
//! owner exited, or crashed, and the OS released its lock), so whoever finds it removes it. A
//! ticket is created and locked while the creator holds `queue.mutex`, and every probe of a
//! predecessor also runs under `queue.mutex`, so a probe never sees a live ticket in the instant
//! between its creation and its lock. The waiter whose ticket has no live predecessor is the head;
//! only the head waits on the job's own file lock, so the lock is handed to waiters in the order
//! their tickets were taken, across processes. Mutual exclusion is still the job lock file's.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rightkit_fs::{FileLock, FsError, LockOptions};

use crate::error::{JobError, JobResult};
use crate::runner::lock_error;

const MUTEX_FILE: &str = "queue.mutex";
const TICKET_SUFFIX: &str = ".ticket";
/// One attempt at the queue mutex waits this long; callers loop and re-check cancellation.
const MUTEX_SLICE: Duration = Duration::from_millis(100);
const MUTEX_POLL: Duration = Duration::from_millis(2);

/// Outcome of [`TicketSlot::join`].
pub(crate) enum Joined {
    Slot(TicketSlot),
    Cancelled,
    TimedOut,
}

/// A waiter's place in the cross-process line for one job id; leaves the line on drop.
pub(crate) struct TicketSlot {
    dir: PathBuf,
    name: String,
    /// `None` only while dropping.
    lock: Option<FileLock>,
}

fn mutex_options() -> LockOptions {
    LockOptions {
        timeout: MUTEX_SLICE,
        poll: MUTEX_POLL,
        label: "job-queue ticket mutex".into(),
    }
}

/// The queue mutex, or `None` when another holder kept it for the whole slice (the caller tries
/// again on its next turn).
fn take_mutex(dir: &Path) -> JobResult<Option<FileLock>> {
    match FileLock::acquire(&dir.join(MUTEX_FILE), &mutex_options()) {
        Ok(lock) => Ok(Some(lock)),
        Err(FsError::Locked { .. } | FsError::LockTimeout { .. }) => Ok(None),
        Err(error) => Err(lock_error(error)),
    }
}

/// Ticket file names in `dir`, in arrival order.
fn tickets(dir: &Path) -> JobResult<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ticket_seq(&name).is_some() {
            names.push(name);
        }
    }
    // Zero-padded sequence first, so a text sort is arrival order.
    names.sort();
    Ok(names)
}

fn ticket_seq(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(TICKET_SUFFIX)?;
    stem.split_once('-')?.0.parse().ok()
}

/// Removes a ticket file whose lock this call holds, then releases the lock. Unix unlinks while
/// the lock is held, so nobody locks the old file in between; elsewhere an open file cannot be
/// removed, so release first (a ticket that stays is found unlocked and removed by the next sweep).
fn remove_locked(path: &Path, lock: FileLock) {
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(path);
        drop(lock);
    }
    #[cfg(not(unix))]
    {
        drop(lock);
        let _ = std::fs::remove_file(path);
    }
}

impl TicketSlot {
    /// Takes the next ticket in `dir` (created if missing). Checks `cancel` before each attempt at
    /// the queue mutex, and gives up at `deadline` (always after at least one attempt).
    pub(crate) fn join(
        dir: &Path,
        deadline: Option<Instant>,
        cancel: &dyn Fn() -> bool,
    ) -> JobResult<Joined> {
        loop {
            // Inside the loop: `discard` may remove an emptied queue directory between attempts.
            std::fs::create_dir_all(dir)?;
            if cancel() {
                return Ok(Joined::Cancelled);
            }
            if let Some(mutex) = take_mutex(dir)? {
                let next = tickets(dir)?
                    .iter()
                    .filter_map(|n| ticket_seq(n))
                    .max()
                    .map_or(1, |m| m.saturating_add(1));
                let name = format!("{next:020}-{}{TICKET_SUFFIX}", std::process::id());
                let lock = FileLock::acquire(
                    &dir.join(&name),
                    &LockOptions::default().labeled("job-queue ticket"),
                )
                .map_err(lock_error)?;
                drop(mutex);
                return Ok(Joined::Slot(Self {
                    dir: dir.to_path_buf(),
                    name,
                    lock: Some(lock),
                }));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(Joined::TimedOut);
            }
        }
    }

    /// True when no live ticket precedes this one. Removes the dead tickets it finds on the way.
    /// `Ok(false)` also when the queue mutex stayed busy; the caller asks again on its next turn.
    pub(crate) fn is_head(&self) -> JobResult<bool> {
        let Some(_mutex) = take_mutex(&self.dir)? else {
            return Ok(false);
        };
        for name in tickets(&self.dir)? {
            if name == self.name {
                return Ok(true);
            }
            let path = self.dir.join(&name);
            match FileLock::try_acquire(&path) {
                // Held: a live waiter is ahead.
                Err(FsError::Locked { .. } | FsError::LockTimeout { .. }) => return Ok(false),
                // Lockable: its owner is gone.
                Ok(dead) => remove_locked(&path, dead),
                Err(error) => return Err(lock_error(error)),
            }
        }
        // Our own ticket is not in the directory: it was removed from outside while we hold it.
        Err(JobError::new(
            "internal",
            format!(
                "job lock ticket {} vanished from {}",
                self.name,
                self.dir.display()
            ),
        ))
    }
}

impl Drop for TicketSlot {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            remove_locked(&self.dir.join(&self.name), lock);
        }
    }
}

/// Removes `dir` (a `<job id>.queue` directory) when no waiter is in line. Best effort and only
/// for a job that is finished or gone: a waiter that joins while the mutex file is being removed
/// can take its ticket under a mutex nobody else shares, which costs that one wait its order, never
/// the job lock's exclusion.
pub(crate) fn discard(dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    if !tickets(dir).is_ok_and(|t| t.is_empty()) {
        return;
    }
    let mutex_path = dir.join(MUTEX_FILE);
    let Ok(Some(mutex)) = take_mutex_once(dir) else {
        return;
    };
    // The owner record lives beside the lock file.
    let mut owner = mutex_path.as_os_str().to_os_string();
    owner.push(".owner");
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(&mutex_path);
        let _ = std::fs::remove_file(PathBuf::from(&owner));
        drop(mutex);
    }
    #[cfg(not(unix))]
    {
        drop(mutex);
        let _ = std::fs::remove_file(&mutex_path);
        let _ = std::fs::remove_file(PathBuf::from(&owner));
    }
    let _ = std::fs::remove_dir(dir);
}

/// The queue mutex tried once, without waiting.
fn take_mutex_once(dir: &Path) -> JobResult<Option<FileLock>> {
    match FileLock::try_acquire(&dir.join(MUTEX_FILE)) {
        Ok(lock) => Ok(Some(lock)),
        Err(FsError::Locked { .. } | FsError::LockTimeout { .. }) => Ok(None),
        Err(error) => Err(lock_error(error)),
    }
}
