//! Cross-process advisory file locks.
//!
//! The lock is an OS advisory lock (`flock` / `LockFileEx` through `std::fs::File::lock*`), so the
//! OS releases it when the owner exits or is killed: there is no TTL and no way for a live
//! holder to be displaced. Owner details (pid, host, label, start time) live in a sibling
//! `<lock>.owner` file because Windows blocks other processes from reading a locked file. An
//! owner file whose lock can be acquired is stale evidence of a crashed holder and is reported
//! through [`FileLock::recovered_stale`].
//!
//! The lock file itself is never deleted: removing it after unlock lets a second process lock
//! the old inode while a third creates and locks a new one for the same path.
//!
//! Consolidates CutRight project/model-store/qa locks and CodeRight `DaemonDbLock`.

use crate::atomic::atomic_write;
use crate::error::{FsError, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockOwner {
    pub pid: u32,
    pub hostname: String,
    pub label: String,
    pub started_unix_ms: u64,
}

impl LockOwner {
    pub fn current(label: &str) -> Self {
        Self {
            pid: std::process::id(),
            hostname: hostname(),
            label: label.to_string(),
            started_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    pub fn describe(&self) -> String {
        format!("pid {} on {} ({}) since unix-ms {}", self.pid, self.hostname, self.label, self.started_unix_ms)
    }
}

#[derive(Clone, Debug)]
pub struct LockOptions {
    /// How long to wait for a busy lock. `Duration::ZERO` tries exactly once.
    pub timeout: Duration,
    pub poll: Duration,
    /// Recorded in the owner file so a contender can say who holds the lock.
    pub label: String,
}

impl Default for LockOptions {
    fn default() -> Self {
        Self { timeout: Duration::ZERO, poll: Duration::from_millis(50), label: String::new() }
    }
}

impl LockOptions {
    pub fn wait(timeout: Duration) -> Self {
        Self { timeout, ..Self::default() }
    }

    pub fn labeled(mut self, label: &str) -> Self {
        self.label = label.to_string();
        self
    }
}

/// Held until dropped (or the process dies).
#[derive(Debug)]
pub struct FileLock {
    file: File,
    path: PathBuf,
    owner_path: Option<PathBuf>,
    /// Owner of a lock left behind by a process that is gone, when one was found.
    pub recovered_stale: Option<LockOwner>,
}

pub fn owner_path_for(lock_path: &Path) -> PathBuf {
    let mut os = lock_path.as_os_str().to_os_string();
    os.push(".owner");
    PathBuf::from(os)
}

fn open_lock_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path).map_err(|e| FsError::io(path, e))
}

impl FileLock {
    /// Exclusive lock; waits up to `options.timeout`.
    pub fn acquire(path: &Path, options: &LockOptions) -> Result<Self> {
        let file = open_lock_file(path)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    if started.elapsed() >= options.timeout {
                        let owner = read_owner(&owner_path_for(path))
                            .map(|o| o.describe())
                            .unwrap_or_else(|| "an unknown owner".into());
                        return if options.timeout.is_zero() {
                            Err(FsError::Locked { path: path.to_path_buf(), owner })
                        } else {
                            Err(FsError::LockTimeout {
                                path: path.to_path_buf(),
                                waited_ms: started.elapsed().as_millis() as u64,
                            })
                        };
                    }
                    std::thread::sleep(options.poll);
                }
                Err(TryLockError::Error(e)) => return Err(FsError::io(path, e)),
            }
        }
        let owner_path = owner_path_for(path);
        let recovered_stale = read_owner(&owner_path);
        let record = serde_json::to_vec_pretty(&LockOwner::current(&options.label)).expect("owner serializes");
        atomic_write(&owner_path, &record)?;
        Ok(Self { file, path: path.to_path_buf(), owner_path: Some(owner_path), recovered_stale })
    }

    /// Fail immediately when busy.
    pub fn try_acquire(path: &Path) -> Result<Self> {
        Self::acquire(path, &LockOptions::default())
    }

    /// Shared (reader) lock: many readers, excluded by a writer. No owner record.
    pub fn acquire_shared(path: &Path, options: &LockOptions) -> Result<Self> {
        let file = open_lock_file(path)?;
        let started = Instant::now();
        loop {
            match file.try_lock_shared() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    if started.elapsed() >= options.timeout {
                        return Err(FsError::LockTimeout {
                            path: path.to_path_buf(),
                            waited_ms: started.elapsed().as_millis() as u64,
                        });
                    }
                    std::thread::sleep(options.poll);
                }
                Err(TryLockError::Error(e)) => return Err(FsError::io(path, e)),
            }
        }
        Ok(Self { file, path: path.to_path_buf(), owner_path: None, recovered_stale: None })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The holder recorded on disk for `lock_path` (diagnostics; may be stale).
    pub fn current_owner(lock_path: &Path) -> Option<LockOwner> {
        read_owner(&owner_path_for(lock_path))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Owner record goes first: while we still hold the lock nobody can have replaced it.
        if let Some(owner) = &self.owner_path {
            let _ = std::fs::remove_file(owner);
        }
        let _ = self.file.unlock();
    }
}

/// Run `f` while holding the exclusive lock at `path`.
pub fn with_lock<T>(path: &Path, options: &LockOptions, f: impl FnOnce() -> T) -> Result<T> {
    let _guard = FileLock::acquire(path, options)?;
    Ok(f())
}

fn read_owner(path: &Path) -> Option<LockOwner> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: buffer is valid for `buf.len()` bytes and gethostname NUL-terminates within it.
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Some(v) = std::env::var_os(var) {
            let s = v.to_string_lossy().trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "unknown".into()
}
