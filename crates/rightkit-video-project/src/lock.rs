//! Cross-process exclusive project lock, delegated to RightKit (`rightkit_fs::FileLock`).
//!
//! `.lock` carries a `flock(2)` advisory lock (`LockKind::Flock`), the primitive
//! `std::fs::File::try_lock` uses, so a previous CutRight build and this one exclude each other
//! on the same project during rollover. RightKit writes and removes `.lock.owner` (pid, host,
//! start time), because Windows file locks block other processes from reading the locked file.
//! A leftover owner file whose lock can be acquired is a stale lock: the previous owner is gone,
//! so the new holder takes over and reports it.

use std::path::Path;
use std::time::Duration;

use rightkit_fs::{FileLock, FsError, LockKind};
use serde::{Deserialize, Serialize};

use crate::error::ProjectError;

pub const LOCK_FILE: &str = ".lock";
pub const OWNER_FILE: &str = ".lock.owner";

/// Owner of the project lock as CutRight reports it: pid, hostname, RFC 3339 UTC start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockOwner {
    pub pid: u32,
    pub hostname: String,
    /// RFC 3339 UTC, whole seconds.
    pub started: String,
}

impl LockOwner {
    fn from_rightkit(owner: &rightkit_fs::LockOwner) -> Self {
        let started: chrono::DateTime<chrono::Utc> = owner.started().into();
        Self {
            pid: owner.pid,
            hostname: owner.hostname.clone(),
            started: started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }
    }

    fn describe(&self) -> String {
        format!(
            "pid {} on {} since {}",
            self.pid, self.hostname, self.started
        )
    }
}

/// The current holder as the `project is locked` message names it, or `unknown owner`.
fn current_owner_text(lock_path: &Path) -> String {
    rightkit_fs::probe_owner(lock_path)
        .map(|o| LockOwner::from_rightkit(&o).describe())
        .unwrap_or_else(|| "unknown owner".into())
}

/// Exclusive lock on a project directory; released on drop (RightKit removes the owner file,
/// then unlocks).
#[derive(Debug)]
pub struct ProjectLock {
    /// Held for its lifetime only: dropping it releases the lock.
    _lock: FileLock,
    /// Owner of a lock left behind by a process that is gone, if one was found.
    pub recovered_stale: Option<LockOwner>,
}

impl ProjectLock {
    pub fn acquire(dir: &Path) -> Result<Self, ProjectError> {
        let lock_path = dir.join(LOCK_FILE);
        // Owner record shape, hostname source and 0600 lock mode follow RightKit's intended lock behaviour.
        let lock =
            FileLock::try_lock_with(&lock_path, LockKind::Flock, Duration::ZERO).map_err(|e| {
                match e {
                    FsError::Locked { .. } => ProjectError::Locked(current_owner_text(&lock_path)),
                    FsError::Io { source, .. } => {
                        ProjectError::io(format!("locking {}", lock_path.display()), source)
                    }
                    other => ProjectError::io(
                        format!("locking {}", lock_path.display()),
                        std::io::Error::other(other),
                    ),
                }
            })?;
        let recovered_stale = lock.recovered_stale.as_ref().map(LockOwner::from_rightkit);
        Ok(Self {
            _lock: lock,
            recovered_stale,
        })
    }
}
