//! Caller-driven WAL checkpoints: PASSIVE when the log grows, RESTART when it is large and the
//! database has been quiet.
//!
//! The crate starts no thread. The owner calls [`WalCheckpointJob::tick`] from the loop or timer
//! it already runs, and [`WalCheckpointJob::note_activity`] whenever the app uses the database.
//! A tick is single-flight: while one checkpoint runs on a job, a concurrent tick returns
//! `Ok(None)` instead of starting a second. Thresholds are in bytes of the `-wal` file.
//!
//! A RESTART waits (under the connection's busy timeout) for readers still using the log, so call
//! it from the thread that owns the writer connection, not from a request path.

use crate::{checkpoint, checkpoint_restart, wal_path, CheckpointStatus, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

/// When a [`WalCheckpointJob`] checkpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalCheckpointPolicy {
    /// PASSIVE once the log is at least this large. A PASSIVE checkpoint never waits for readers
    /// or writers. Default 16 MiB.
    pub passive_above_bytes: u64,
    /// RESTART once the log is at least this large and the database has been idle for
    /// `quiet_for`. Default 256 MiB.
    pub restart_above_bytes: u64,
    /// How long the database must have been idle before a RESTART. Default 30 seconds.
    pub quiet_for: Duration,
}

impl Default for WalCheckpointPolicy {
    fn default() -> Self {
        Self {
            passive_above_bytes: 16 * MIB,
            restart_above_bytes: 256 * MIB,
            quiet_for: Duration::from_secs(30),
        }
    }
}

/// The checkpoint a tick ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalCheckpointMode {
    Passive,
    Restart,
}

/// What one [`WalCheckpointJob::tick`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalCheckpointRun {
    pub mode: WalCheckpointMode,
    /// Size of the `-wal` file when the mode was chosen.
    pub wal_bytes: u64,
    /// SQLite's report: busy flag, frames in the log, frames copied.
    pub status: CheckpointStatus,
}

/// A checkpoint job for one database. Share it by reference; it holds no connection.
#[derive(Debug)]
pub struct WalCheckpointJob {
    wal: PathBuf,
    policy: WalCheckpointPolicy,
    last_activity: Mutex<Instant>,
    running: AtomicBool,
}

impl WalCheckpointJob {
    /// A job for the database at `db`, idle from now.
    pub fn new(db: impl AsRef<Path>, policy: WalCheckpointPolicy) -> Result<Self> {
        Ok(Self {
            wal: wal_path(db.as_ref())?,
            policy,
            last_activity: Mutex::new(Instant::now()),
            running: AtomicBool::new(false),
        })
    }

    /// The policy this job was built with.
    pub fn policy(&self) -> WalCheckpointPolicy {
        self.policy
    }

    /// Records that the database was used now. Call it on every use; a RESTART needs the database
    /// to have been idle for `quiet_for`.
    pub fn note_activity(&self) {
        self.note_activity_at(Instant::now());
    }

    /// [`Self::note_activity`] at `now`, for callers that keep their own clock.
    pub fn note_activity_at(&self, now: Instant) {
        let mut last = lock_recover(&self.last_activity);
        if now > *last {
            *last = now;
        }
    }

    /// Runs the checkpoint due now on `conn`, the writer for this database, if the policy calls
    /// for one. `Ok(None)` when nothing is due or another tick is still running.
    pub fn tick(&self, conn: &Connection) -> Result<Option<WalCheckpointRun>> {
        self.tick_at(conn, Instant::now())
    }

    /// [`Self::tick`] at `now`, for callers that keep their own clock.
    pub fn tick_at(&self, conn: &Connection, now: Instant) -> Result<Option<WalCheckpointRun>> {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(None);
        }
        let _flight = Flight(&self.running);
        let wal_bytes = std::fs::metadata(&self.wal).map_or(0, |m| m.len());
        if wal_bytes == 0 {
            return Ok(None);
        }
        let idle = now.saturating_duration_since(*lock_recover(&self.last_activity));
        let restart_due =
            wal_bytes >= self.policy.restart_above_bytes && idle >= self.policy.quiet_for;
        let mode = if restart_due {
            WalCheckpointMode::Restart
        } else if wal_bytes >= self.policy.passive_above_bytes {
            WalCheckpointMode::Passive
        } else {
            return Ok(None);
        };
        let status = match mode {
            WalCheckpointMode::Restart => checkpoint_restart(conn)?,
            WalCheckpointMode::Passive => checkpoint(conn, "PRAGMA wal_checkpoint(PASSIVE)")?,
        };
        Ok(Some(WalCheckpointRun {
            mode,
            wal_bytes,
            status,
        }))
    }
}

/// Clears the single-flight flag when a tick ends, including on an error.
struct Flight<'a>(&'a AtomicBool);

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
