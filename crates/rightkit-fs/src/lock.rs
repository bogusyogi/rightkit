//! Cross-process advisory file locks.
//!
//! The lock is an OS advisory lock, so the OS releases it when the owner exits or is killed:
//! there is no TTL and no way for a live holder to be displaced. Unix uses POSIX record locks
//! (`fcntl` `F_SETLK`), Windows uses `LockFileEx` through `std::fs::File::lock*`. Owner details
//! (pid, host, label, start time) live in a sibling `<lock>.owner` file because Windows blocks
//! other processes from reading a locked file. An
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
#[cfg(not(unix))]
use std::fs::TryLockError;
use std::fs::{File, OpenOptions};
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
        format!(
            "pid {} on {} ({}) since unix-ms {}",
            self.pid, self.hostname, self.label, self.started_unix_ms
        )
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
        Self {
            timeout: Duration::ZERO,
            poll: Duration::from_millis(50),
            label: String::new(),
        }
    }
}

impl LockOptions {
    pub fn wait(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Self::default()
        }
    }

    pub fn labeled(mut self, label: &str) -> Self {
        self.label = label.to_string();
        self
    }
}

/// Held until dropped (or the process dies).
#[derive(Debug)]
pub struct FileLock {
    /// Held for its `Drop`, which releases the lock; never read.
    #[cfg_attr(unix, allow(dead_code))]
    file: Handle,
    path: PathBuf,
    owner_path: Option<PathBuf>,
    /// Owner of a lock left behind by a process that is gone, when one was found.
    pub recovered_stale: Option<LockOwner>,
}

/// Which lock a caller wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Exclusive,
    Shared,
}

/// Unix: a process-held POSIX record lock. Windows: the locked file.
#[cfg(unix)]
type Handle = posix::Handle;
#[cfg(not(unix))]
type Handle = File;

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
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path).map_err(|e| FsError::io(path, e))
}

impl FileLock {
    /// Exclusive lock; waits up to `options.timeout`.
    pub fn acquire(path: &Path, options: &LockOptions) -> Result<Self> {
        let file = lock_with_retry(path, options, Mode::Exclusive)?;
        let owner_path = owner_path_for(path);
        let recovered_stale = read_owner(&owner_path);
        let record = serde_json::to_vec_pretty(&LockOwner::current(&options.label))
            .expect("owner serializes");
        atomic_write(&owner_path, &record)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            owner_path: Some(owner_path),
            recovered_stale,
        })
    }

    /// Fail immediately when busy.
    pub fn try_acquire(path: &Path) -> Result<Self> {
        Self::acquire(path, &LockOptions::default())
    }

    /// Shared (reader) lock: many readers, excluded by a writer. No owner record.
    pub fn acquire_shared(path: &Path, options: &LockOptions) -> Result<Self> {
        let file = lock_with_retry(path, options, Mode::Shared)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            owner_path: None,
            recovered_stale: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The holder recorded on disk for `lock_path` (diagnostics; may be stale).
    pub fn current_owner(lock_path: &Path) -> Option<LockOwner> {
        read_owner(&owner_path_for(lock_path))
    }
}

/// Retries `mode` on `path` until granted, timed out, or the lock file fails.
#[cfg(unix)]
fn lock_with_retry(path: &Path, options: &LockOptions, mode: Mode) -> Result<Handle> {
    let started = Instant::now();
    loop {
        if let Some(handle) = posix::try_lock(path, mode)? {
            return Ok(handle);
        }
        wait_or_fail(path, options, started, mode)?;
    }
}

/// Windows: `LockFileEx` is per handle and not inherited, so one handle serves every attempt.
#[cfg(not(unix))]
fn lock_with_retry(path: &Path, options: &LockOptions, mode: Mode) -> Result<Handle> {
    let file = open_lock_file(path)?;
    let started = Instant::now();
    loop {
        let locked = match mode {
            Mode::Exclusive => file.try_lock(),
            Mode::Shared => file.try_lock_shared(),
        };
        match locked {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => wait_or_fail(path, options, started, mode)?,
            Err(TryLockError::Error(e)) => return Err(FsError::io(path, e)),
        }
    }
}

/// Returns the timeout error once the wait budget is spent; otherwise sleeps one poll.
fn wait_or_fail(path: &Path, options: &LockOptions, started: Instant, mode: Mode) -> Result<()> {
    if started.elapsed() >= options.timeout {
        let waited_ms = started.elapsed().as_millis() as u64;
        return Err(match mode {
            Mode::Exclusive if options.timeout.is_zero() => {
                let owner = read_owner(&owner_path_for(path))
                    .map(|o| o.describe())
                    .unwrap_or_else(|| "an unknown owner".into());
                FsError::Locked {
                    path: path.to_path_buf(),
                    owner,
                }
            }
            _ => FsError::LockTimeout {
                path: path.to_path_buf(),
                waited_ms,
            },
        });
    }
    std::thread::sleep(options.poll);
    Ok(())
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Owner record goes first: while we still hold the lock nobody can have replaced it.
        if let Some(owner) = &self.owner_path {
            let _ = std::fs::remove_file(owner);
        }
        // Unix: dropping `self.file` releases the POSIX record lock (see `posix`).
        #[cfg(not(unix))]
        let _ = self.file.unlock();
    }
}

/// Unix locks are POSIX record locks, not `flock`.
///
/// `flock` belongs to the open file description. A child forked by another thread inherits
/// the descriptor and keeps the lock alive until it execs, so a released lock looks busy.
/// Record locks belong to the process and are not inherited across `fork`.
///
/// Closing any descriptor for a file releases every record lock this process holds on it.
/// So a process keeps one descriptor per lock file: shared holders share it, and a registry
/// under one mutex records what this process holds. Acquire and release both run under that
/// mutex, and a descriptor closes only under it.
#[cfg(unix)]
mod posix {
    use super::{open_lock_file, Mode};
    use crate::error::{FsError, Result};
    use nix::errno::Errno;
    use nix::fcntl::{fcntl, FcntlArg};
    use nix::libc;
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    struct Entry {
        mode: Mode,
        holders: usize,
        file: Arc<File>,
    }

    fn registry() -> MutexGuard<'static, BTreeMap<PathBuf, Entry>> {
        static REGISTRY: OnceLock<Mutex<BTreeMap<PathBuf, Entry>>> = OnceLock::new();
        REGISTRY
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// A record lock this process holds on one lock file. Released on drop.
    #[derive(Debug)]
    pub struct Handle {
        key: PathBuf,
        /// Taken on drop under the registry mutex, so the last descriptor closes before any
        /// other acquire in this process can open the file again.
        file: Option<Arc<File>>,
    }

    /// Tries `mode` on the lock file at `path`. `Ok(None)` when busy, in this process or another.
    pub fn try_lock(path: &Path, mode: Mode) -> Result<Option<Handle>> {
        let key = registry_key(path);
        let mut registry = registry();
        if let Some(entry) = registry.get_mut(&key) {
            // This process's own record locks never conflict with each other, so contention
            // inside the process is decided here: shared holders join, anything else waits.
            if entry.mode == Mode::Shared && mode == Mode::Shared {
                entry.holders += 1;
                return Ok(Some(Handle {
                    key,
                    file: Some(entry.file.clone()),
                }));
            }
            return Ok(None);
        }
        let file = Arc::new(open_lock_file(path)?);
        if !set_lock(&file, mode, path)? {
            // This process holds no record lock on the file (its own locks never make F_SETLK
            // fail), so closing this descriptor releases nothing.
            return Ok(None);
        }
        registry.insert(
            key.clone(),
            Entry {
                mode,
                holders: 1,
                file: file.clone(),
            },
        );
        Ok(Some(Handle {
            key,
            file: Some(file),
        }))
    }

    /// `Ok(true)` when granted; `Ok(false)` when another process holds a conflicting lock.
    fn set_lock(file: &File, mode: Mode, path: &Path) -> Result<bool> {
        let kind = match mode {
            Mode::Exclusive => libc::F_WRLCK,
            Mode::Shared => libc::F_RDLCK,
        };
        let request = libc::flock {
            l_type: kind as _,
            l_whence: libc::SEEK_SET as _,
            l_start: 0,
            l_len: 0,
            l_pid: 0,
        };
        match fcntl(file, FcntlArg::F_SETLK(&request)) {
            Ok(_) => Ok(true),
            Err(Errno::EAGAIN) | Err(Errno::EACCES) => Ok(false),
            Err(errno) => Err(FsError::io(path, std::io::Error::from(errno))),
        }
    }

    /// Canonical parent plus file name, so one file reached through two spellings of its
    /// directory shares one registry entry.
    fn registry_key(path: &Path) -> PathBuf {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        match (parent.canonicalize(), path.file_name()) {
            (Ok(dir), Some(name)) => dir.join(name),
            _ => path.to_path_buf(),
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            let mut registry = registry();
            let last = match registry.get_mut(&self.key) {
                Some(entry) => {
                    entry.holders -= 1;
                    entry.holders == 0
                }
                None => false,
            };
            if last {
                registry.remove(&self.key);
            }
            drop(self.file.take());
        }
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
