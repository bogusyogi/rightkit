//! Per-entry operation lock: a cross-process lock file, so two Right Suite
//! apps (or an app and its engine sidecar — or two threads in one process)
//! never download or migrate the same model at once.
//!
//! The lock file is `<entry-dir>.lock` next to the entry dir. New holders use
//! an OS advisory lock plus metadata; legacy create_new holders remain
//! conservative until their TTL expires.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A lock older than this is assumed abandoned (a download of the largest
/// model takes far less even on a slow link).
const LOCK_TTL: Duration = Duration::from_secs(4 * 3600);
const WAIT_POLL: Duration = Duration::from_millis(400);

#[derive(Debug, Serialize, Deserialize)]
struct LockBody {
    #[serde(default)]
    format: Option<String>,
    pid: u32,
    app: String,
    acquired_at_ms: u64,
}

/// Held while a model/tool install, repair, or migration mutates the store.
/// Dropping removes the lock file.
pub struct StoreLock {
    lock_path: PathBuf,
    guard_file: File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
        let _ = self.guard_file.unlock();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Path of the cross-process lock file for `entry_dir`.
pub fn lock_path_for(entry_dir: &Path) -> PathBuf {
    let mut os = entry_dir.as_os_str().to_os_string();
    os.push(".lock");
    PathBuf::from(os)
}

fn advisory_path_for(entry_dir: &Path) -> PathBuf {
    let mut os = entry_dir.as_os_str().to_os_string();
    os.push(".operation.lock");
    PathBuf::from(os)
}

fn lock_is_stale(lock_path: &Path) -> bool {
    let Ok(meta) = fs::metadata(lock_path) else {
        return true;
    };
    let age = meta
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .unwrap_or(LOCK_TTL + Duration::from_secs(1));
    age > LOCK_TTL
}

fn try_acquire_file(lock_path: &Path, app: &str) -> Result<Option<StoreLock>, String> {
    let guard_path = advisory_path_for(
        lock_path
            .to_str()
            .and_then(|path| path.strip_suffix(".lock"))
            .map(Path::new)
            .unwrap_or(lock_path),
    );
    if let Some(parent) = guard_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create lock dir: {e}"))?;
    }
    let guard_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&guard_path)
        .map_err(|error| format!("open advisory lock {}: {error}", guard_path.display()))?;
    match guard_file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(error)) => {
            return Err(format!("lock {}: {error}", guard_path.display()))
        }
    }
    let created = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
    {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(format!("create {}: {error}", lock_path.display())),
    };
    let mut existing = String::new();
    if !created {
        let mut reader = OpenOptions::new()
            .read(true)
            .open(lock_path)
            .map_err(|error| format!("read {}: {error}", lock_path.display()))?;
        reader
            .read_to_string(&mut existing)
            .map_err(|error| format!("read {}: {error}", lock_path.display()))?;
    }
    let is_v2 = serde_json::from_str::<LockBody>(&existing)
        .ok()
        .and_then(|body| body.format)
        .as_deref()
        == Some("advisory-v2")
        || existing.contains("\"format\":\"advisory-v2\"");
    if !created && !is_v2 && !lock_is_stale(lock_path) {
        let _ = guard_file.unlock();
        return Ok(None);
    }
    let body = LockBody {
        format: Some("advisory-v2".to_string()),
        pid: std::process::id(),
        app: app.to_string(),
        acquired_at_ms: now_ms(),
    };
    let bytes = serde_json::to_vec(&body).map_err(|error| format!("serialize lock: {error}"))?;
    let mut marker = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .open(lock_path)
        .map_err(|error| format!("open {}: {error}", lock_path.display()))?;
    marker
        .write_all(&bytes)
        .map_err(|error| format!("write {}: {error}", lock_path.display()))?;
    marker
        .sync_all()
        .map_err(|error| format!("sync {}: {error}", lock_path.display()))?;
    Ok(Some(StoreLock {
        lock_path: lock_path.to_path_buf(),
        guard_file,
    }))
}

/// Try to acquire the store lock for `entry_dir`. `Ok(None)` means another
/// process (or thread) holds a live lock — caller should wait or report
/// "in progress".
pub fn try_lock(entry_dir: &Path, app: &str) -> Result<Option<StoreLock>, String> {
    let lock_path = lock_path_for(entry_dir);
    try_acquire_file(&lock_path, app)
}

/// Acquire the store lock, waiting until it is free.
pub fn lock(entry_dir: &Path, app: &str) -> Result<StoreLock, String> {
    loop {
        if let Some(guard) = try_lock(entry_dir, app)? {
            return Ok(guard);
        }
        std::thread::sleep(WAIT_POLL);
    }
}

/// Same as [`lock`] but bounded — `Ok(None)` on timeout.
pub fn lock_timeout(
    entry_dir: &Path,
    app: &str,
    timeout: Duration,
) -> Result<Option<StoreLock>, String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(guard) = try_lock(entry_dir, app)? {
            return Ok(Some(guard));
        }
        if start.elapsed() >= timeout {
            return Ok(None);
        }
        std::thread::sleep(WAIT_POLL);
    }
}

/// True while a *live* (non-stale) lock file exists for `entry_dir` — used to
/// report "another app is installing this" and to wait installs out.
pub fn install_in_progress(entry_dir: &Path) -> bool {
    let lock_path = lock_path_for(entry_dir);
    lock_path.exists() && !lock_is_stale(&lock_path)
}

/// Wait until no live install is in progress for `entry_dir`, or `timeout`.
/// Returns `true` when the path is free.
pub fn wait_for_install(entry_dir: &Path, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while install_in_progress(entry_dir) {
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(WAIT_POLL);
    }
    true
}
