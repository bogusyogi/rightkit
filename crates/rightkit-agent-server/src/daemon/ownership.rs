//! Process ownership & single-instance enforcement for the RightKit daemon (DL-001).
//!
//! Ported from `grok-build` `crates/codegen/xai-grok-workspace-daemon/src/daemonize.rs`
//! (`PidFile::acquire`), adapted to the RightKit seam: lock lives beside
//! RightKit's existing `~/.coderight/daemon.port` discovery file rather than
//! `/tmp`. This module supplies secure exclusive acquisition, held-lock
//! lifetime, & advisory PID diagnostics. Exact-process pinning
//! (`process_start_epoch_ms`) remains owned by `main_runtime_daemon_resolve.rs`
//! for host-level takeover. Production `serve` acquires and holds this lock
//! before startup becomes observable; see `takeover.rs` for stale-owner
//! escalation.
//!
//! Hostile-owner tests: a directory path, a symlinked path, and a contended
//! second acquire must all behave per the doc comments below — see the test
//! module.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rightkit_fs::{FileLock, FsError};

/// Default single-instance lock path: `~/.coderight/daemon.lock`, sibling of
/// the existing `daemon.port` discovery file owned by
/// `main_runtime_daemon_resolve.rs`.
pub(crate) fn default_lockfile_path() -> Option<PathBuf> {
    Some(default_daemon_dir()?.join("daemon.lock"))
}

pub(crate) fn default_discovery_path() -> Option<PathBuf> {
    Some(default_daemon_dir()?.join("daemon.port"))
}

fn default_daemon_dir() -> Option<PathBuf> {
    rightkit_agent_config::state_root()
}

/// Canonical spelling of a database path for lock-name derivation. The file
/// itself may not exist yet (first start), so canonicalize the parent
/// directory and re-attach the file name; two spellings of the same database
/// (relative vs absolute, `.` segments, symlinked parent) must map to the same
/// lock file or a second daemon races past the per-database lock.
pub(crate) fn canonical_db_path(db: &Path) -> PathBuf {
    if let Ok(canonical) = db.canonicalize() {
        return canonical;
    }
    let absolute = if db.is_absolute() {
        db.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(db))
            .unwrap_or_else(|_| db.to_path_buf())
    };
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|p| p.join(name))
            .unwrap_or(absolute),
        _ => absolute,
    }
}

/// True when both paths name the same database file.
pub(crate) fn same_db_path(a: &Path, b: &Path) -> bool {
    a == b || canonical_db_path(a) == canonical_db_path(b)
}

/// Single-instance lock held for the daemon's lifetime, backed by
/// `rightkit_fs::FileLock` (exclusive lock, symlink / reparse-point refusal,
/// `<lock>.owner` JSON diagnostics). Dropping it releases the lock; the lock
/// file itself is left on disk. The recorded owner is advisory only: the OS
/// lock provides the actual exclusion.
#[derive(Debug)]
pub struct OwnershipLock {
    lock: FileLock,
}

impl OwnershipLock {
    pub(crate) fn lock_path(&self) -> &Path {
        self.lock.path()
    }

    /// Take the exclusive lock and record the current PID as owner.
    ///
    /// - `Ok(Some(_))` - lock acquired; hold the returned guard for the
    ///   daemon's lifetime.
    /// - `Ok(None)` - another live process holds the lock (caller should
    ///   route to `takeover::acquire_or_take_over` or exit cleanly).
    /// - `Err(_)` - an I/O error opening or locking the file.
    pub fn acquire(path: &Path) -> io::Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match FileLock::try_acquire(path) {
            Ok(lock) => Ok(Some(Self { lock })),
            Err(FsError::Locked { .. } | FsError::LockTimeout { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Advisory pid recorded by the lock holder (`<lock>.owner`); `None` if absent,
/// unreadable, or not a positive pid.
pub(crate) fn read_lockfile_pid(path: &Path) -> Option<u32> {
    rightkit_fs::probe_owner(path)
        .map(|owner| owner.pid)
        .filter(|&pid| pid > 0)
}
