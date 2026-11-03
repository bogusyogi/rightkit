//! The licence operation lock and the fenced worker.
//!
//! One OS lock per app (`<license dir>/.lock`), the crate's own [`CrossProcessLock`], serialises
//! every licence mutation and every authorization read across threads and processes. It is only
//! ever taken with `try_acquire` polled against a deadline, never with the blocking `acquire`
//! (5 s for a mutation, 250 ms for a status or authorization read), so a stuck holder costs a
//! caller a bounded wait and a `Busy`, not a hang.
//!
//! ## Fencing (v3.2 option A)
//! A mutation does not end when its caller stops waiting: the backend call it started keeps
//! running. [`run_fenced`] therefore moves the [`OpGuard`] INTO the worker thread that makes the
//! backend calls. The worker drops it only when it has actually finished (a deactivation
//! tombstone included), so no other process or thread can take the lock, read a half-decided
//! state, or write over a late write, however long the backend takes. The caller gets the guard
//! back with the result when it is still waiting; when its deadline passed first it gets
//! [`Fenced::TimedOut`] and the guard stays with the worker.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::order::{self, Level};
use super::view::LicenseError;
use crate::CrossProcessLock;

/// How often a held lock is retried. The wait ends at the caller's deadline.
const POLL: Duration = Duration::from_millis(5);

/// Wait for a mutation to get the lock.
pub(crate) const MUTATION_WAIT: Duration = Duration::from_secs(5);
/// Wait for a status or authorization read to get the lock.
pub(crate) const STATUS_WAIT: Duration = Duration::from_millis(250);

/// Sleep one poll interval, but never past `deadline`.
pub(crate) fn pause(deadline: Instant) {
    let now = Instant::now();
    if now < deadline {
        std::thread::sleep(POLL.min(deadline - now));
    }
}

/// The held licence operation lock. `Send`, so it can move into the fenced worker; dropping it
/// releases the OS lock.
pub(crate) struct OpGuard {
    _lock: CrossProcessLock,
    /// Exact credential target selected after this licence operation acquired its lock.
    pub(crate) records: Option<Box<super::persist::RecordStore>>,
}
impl std::fmt::Debug for OpGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpGuard").finish_non_exhaustive()
    }
}

/// The per-app licence operation lock file.
#[derive(Debug, Clone)]
pub(crate) struct OpLock {
    path: PathBuf,
}

impl OpLock {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Take the lock, polling until `wait` has passed. `Busy` when it is still held then;
    /// `Store` when the lock file cannot be opened at all. The caller marks its thread with
    /// [`order::enter`]`(Level::Op)` for as long as its operation lasts.
    pub(crate) fn acquire(&self, wait: Duration) -> Result<OpGuard, LicenseError> {
        acquire_polled(&self.path, wait)
    }
}

/// `try_acquire` against a deadline. Never the blocking `acquire`.
pub(crate) fn acquire_polled(
    path: &std::path::Path,
    wait: Duration,
) -> Result<OpGuard, LicenseError> {
    let now = Instant::now();
    let deadline = now.checked_add(wait).unwrap_or(now);
    loop {
        match CrossProcessLock::try_acquire(path) {
            Ok(Some(lock)) => {
                return Ok(OpGuard {
                    _lock: lock,
                    records: None,
                })
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    return Err(LicenseError::Busy);
                }
                pause(deadline);
            }
            Err(error) => return Err(lock_open_error(&error)),
        }
    }
}

fn lock_open_error(_error: &io::Error) -> LicenseError {
    LicenseError::Store
}

/// What [`run_fenced`] gives back.
pub(crate) enum Fenced<R> {
    /// The worker finished inside the caller's wait; the caller holds the guard again.
    Done(R, OpGuard),
    /// The caller's wait passed. The worker keeps running and keeps the guard until it ends.
    TimedOut,
    /// The worker panicked or could not start. The guard was dropped with it.
    Failed,
}

/// Run `job` on a worker thread that owns `guard` until the job has returned.
///
/// The worker enters the operation level for its own thread, so the lock-order assertions see
/// what the worker does (it takes the rotation lock inside the credentials calls). When the
/// caller is still waiting, the guard is sent back with the result; when the caller gave up,
/// `send` fails and the guard is dropped when the worker ends, which is the fence.
pub(crate) fn run_fenced<R: Send + 'static>(
    guard: OpGuard,
    wait: Duration,
    job: impl FnOnce() -> R + Send + 'static,
) -> Fenced<R> {
    run_fenced_at(guard, wait, Level::Op, job)
}

pub(crate) fn run_fenced_at<R: Send + 'static>(
    guard: OpGuard,
    wait: Duration,
    level: Level,
    job: impl FnOnce() -> R + Send + 'static,
) -> Fenced<R> {
    let (tx, rx) = mpsc::channel::<(R, OpGuard)>();
    let spawned = std::thread::Builder::new()
        .name("rightkit-license-write".to_string())
        .spawn(move || {
            let _level = order::enter(level);
            let result = job();
            // A send to a caller that already left returns the pair here, and it drops now.
            let _ = tx.send((result, guard));
        });
    if spawned.is_err() {
        return Fenced::Failed;
    }
    match rx.recv_timeout(wait) {
        Ok((result, guard)) => Fenced::Done(result, guard),
        Err(mpsc::RecvTimeoutError::Timeout) => Fenced::TimedOut,
        Err(mpsc::RecvTimeoutError::Disconnected) => Fenced::Failed,
    }
}
