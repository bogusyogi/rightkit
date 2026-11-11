//! The exclusive OS lock file behind the app-root writer lock and the root-cutover lock, also
//! used by `rightkit-daemon` for its host election and spawn claim. Moved unchanged from
//! `rightkit-daemon`'s `discovery` module (0.7.0).

use std::io;
use std::path::Path;

/// An exclusive OS lock on a file, held until this value is dropped or the process dies.
///
/// On Unix the lock is a `flock`, which belongs to the open file description, not to the
/// descriptor. A child that another thread of this process forks (or `posix_spawn`s) while the
/// descriptor is open holds a copy of the description until its `exec` closes it, so closing our
/// descriptor alone would leave a released lock reading as held for that long (rightkit#1323,
/// #788). Drop therefore unlocks explicitly, which ends the lock for every copy of the
/// description, as `rightkit-fs`'s `FileLock` does for a `flock`. The descriptor is
/// close-on-exec, so no exec'd child ever holds it. Windows holds the lock through the
/// no-share handle itself, which a spawned child does not inherit.
#[derive(Debug)]
pub struct LockedFile {
    #[cfg_attr(not(unix), allow(dead_code))]
    file: std::fs::File,
}

impl Drop for LockedFile {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // Closing right after still ends the lock when this descriptor is the only copy, so
            // a failure here has nothing left to report.
            let _ = self.file.unlock();
        }
    }
}

/// Open `path` and take a non-blocking exclusive lock. `Ok(None)` = held elsewhere.
#[cfg(unix)]
pub fn try_lock_exclusive(path: &Path) -> io::Result<Option<LockedFile>> {
    use std::fs::TryLockError;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    // std's `File::try_lock` is a non-blocking `flock(LOCK_EX | LOCK_NB)` on Unix.
    match f.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => return Err(e),
    }
    // Informational only (we hold the lock, so the write is ours).
    let _ = f.set_len(0);
    let _ = f.write_all(std::process::id().to_string().as_bytes());
    Ok(Some(LockedFile { file: f }))
}

/// Windows: open with share mode 0 (no sharing). A second open fails with
/// ERROR_SHARING_VIOLATION (32) while the first handle lives; the kernel closes
/// the handle on process death. Not compiled on this host; std-only API
/// (`OpenOptionsExt::share_mode`), checked by reasoning against its docs.
#[cfg(windows)]
pub fn try_lock_exclusive(path: &Path) -> io::Result<Option<LockedFile>> {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
    {
        Ok(mut f) => {
            let _ = f.write_all(std::process::id().to_string().as_bytes());
            Ok(Some(LockedFile { file: f }))
        }
        Err(e) if e.raw_os_error() == Some(32) => Ok(None),
        Err(e) => Err(e),
    }
}
