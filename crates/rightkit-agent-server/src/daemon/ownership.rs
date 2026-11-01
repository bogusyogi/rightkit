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
    rightkit_app_settings::agent_config::state_root()
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

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn ownership_acquire_is_exclusive() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("daemon.lock");

        let first = OwnershipLock::acquire(&path).unwrap();
        assert!(first.is_some(), "first acquire should win the lock");

        let second = OwnershipLock::acquire(&path).unwrap();
        assert!(second.is_none(), "contended acquire must report None");

        drop(first);

        let deadline = Instant::now() + Duration::from_secs(2);
        let third = loop {
            match OwnershipLock::acquire(&path).unwrap() {
                Some(guard) => break Some(guard),
                None if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                none => break none,
            }
        };
        assert!(third.is_some(), "acquire should succeed after release");
    }

    #[test]
    fn ownership_records_current_pid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("daemon.lock");

        let guard = OwnershipLock::acquire(&path).unwrap().unwrap();
        assert_eq!(read_lockfile_pid(&path), Some(std::process::id()));
        drop(guard);
    }

    #[test]
    fn ownership_persists_on_disk_after_drop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("daemon.lock");

        let guard = OwnershipLock::acquire(&path).unwrap().unwrap();
        assert!(path.exists());
        drop(guard);
        assert!(path.exists(), "lockfile should remain on disk after drop");
    }

    #[test]
    fn ownership_acquire_creates_parent_dir() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/sub/daemon.lock");

        let guard = OwnershipLock::acquire(&path).unwrap();
        assert!(guard.is_some());
        assert!(path.exists());
    }

    #[test]
    fn contended_acquire_does_not_modify_lockfile() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("daemon.lock");

        let holder = OwnershipLock::acquire(&path).unwrap().unwrap();
        let owner = rightkit_fs::lock::owner_path_for(&path);
        let before = fs::read_to_string(&owner).unwrap();

        let contended = OwnershipLock::acquire(&path).unwrap();
        assert!(contended.is_none());

        let after = fs::read_to_string(&owner).unwrap();
        assert_eq!(
            before, after,
            "contended acquire must not rewrite the owner record"
        );
        drop(holder);
    }

    #[test]
    fn ownership_acquire_errors_on_directory() {
        let dir = TempDir::new().unwrap();
        let as_dir = dir.path().join("a_dir");
        fs::create_dir(&as_dir).unwrap();

        assert!(
            OwnershipLock::acquire(&as_dir).is_err(),
            "acquiring a directory path must error, not report Ok(None)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ownership_acquire_rejects_symlinked_path() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.lock");
        let link = dir.path().join("link.lock");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = OwnershipLock::acquire(&link).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn ownership_created_mode_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("daemon.lock");

        let _guard = OwnershipLock::acquire(&path).unwrap().unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "lockfile must not be group/other-accessible"
        );
    }

    #[test]
    fn read_lockfile_pid_parses_and_rejects() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("daemon.lock");
        let owner = rightkit_fs::lock::owner_path_for(&path);

        fs::write(
            &owner,
            r#"{"pid":1234,"hostname":"h","label":"","started_unix_ms":1}"#,
        )
        .unwrap();
        assert_eq!(read_lockfile_pid(&path), Some(1234));

        fs::write(
            &owner,
            r#"{"pid":0,"hostname":"h","label":"","started_unix_ms":1}"#,
        )
        .unwrap();
        assert_eq!(read_lockfile_pid(&path), None, "pid 0 is not a process");

        fs::write(&owner, "garbage").unwrap();
        assert_eq!(read_lockfile_pid(&path), None);

        assert_eq!(read_lockfile_pid(&dir.path().join("missing")), None);
    }

    #[test]
    fn two_spellings_of_one_new_database_share_one_lock() {
        let dir = TempDir::new().unwrap();
        let plain = dir.path().join("fresh.db");
        let dotted = dir.path().join(".").join("fresh.db");
        assert!(!plain.exists());
        assert!(same_db_path(&plain, &dotted));
        let first = rightkit_fs::FileLock::try_acquire(
            &canonical_db_path(&plain).with_extension("daemon.lock"),
        )
        .expect("first daemon takes the database lock");
        let second = rightkit_fs::FileLock::try_acquire(
            &canonical_db_path(&dotted).with_extension("daemon.lock"),
        );
        assert!(second.is_err(), "second daemon must not race past the lock");
        drop(first);
    }

    #[test]
    fn force_does_not_skip_exclusive_ownership_lock() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("daemon.lock");
        let _held = OwnershipLock::acquire(&path).unwrap().expect("first");
        // serve() always calls acquire() for the primary daemon, even when
        // RIGHTKIT_SERVE_FORCE is set; a second acquire must be refused.
        assert!(OwnershipLock::acquire(&path).unwrap().is_none());
    }
}
