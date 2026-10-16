//! Cross-process advisory lock over a private directory, shared by the durable stores that
//! cannot lean on an OS-native lock (keychain-backed tokens, file-backed pending state).
//! Lock files hold no data; the lock lives in the open file description, so a crashed holder
//! releases it. Works on Unix and Windows (`File::try_lock`).
use super::ServerAuthError;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Create (if needed) and canonicalize a per-user directory. On Unix the leaf is created 0700
/// and must be owned by the effective user with no group/other access. On Windows the
/// directory inherits the ACL of its parent: place it under the user profile
/// (`%LOCALAPPDATA%`), never in a shared location.
pub(super) fn prepare_dir(path: &Path) -> Result<PathBuf, ServerAuthError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|_| ServerAuthError::Storage)?;
        }
    }
    #[cfg(unix)]
    {
        super::store::private_dir(path)?;
    }
    #[cfg(not(unix))]
    {
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ServerAuthError::Storage),
        }
        check_dir(path)?;
    }
    fs::canonicalize(path).map_err(|_| ServerAuthError::Storage)
}

/// Recheck the directory before each operation: still a real directory, still private.
pub(super) fn check_dir(path: &Path) -> Result<(), ServerAuthError> {
    #[cfg(unix)]
    {
        super::store::check_dir(path)
    }
    #[cfg(not(unix))]
    {
        let meta = fs::symlink_metadata(path).map_err(|_| ServerAuthError::Storage)?;
        if meta.is_dir() {
            Ok(())
        } else {
            Err(ServerAuthError::Storage)
        }
    }
}

fn open_lock_file(path: &Path) -> Result<File, ServerAuthError> {
    #[cfg(unix)]
    {
        super::store::private_file(path, false)
    }
    #[cfg(not(unix))]
    {
        if let Ok(meta) = fs::symlink_metadata(path) {
            if !meta.is_file() {
                return Err(ServerAuthError::Storage);
            }
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| ServerAuthError::Storage)
    }
}

/// Take the exclusive lock `name` under `root`, waiting at most `timeout`. Dropping the
/// returned file releases it. `name` must be a plain file name chosen by this crate.
pub(super) fn acquire(root: &Path, name: &str, timeout: Duration) -> Result<File, ServerAuthError> {
    check_dir(root)?;
    let lock = open_lock_file(&root.join(name))?;
    let start = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err(ServerAuthError::Storage),
        }
    }
}

/// Lower-case hex SHA-256 of `text`. Hex (not base64url) because Windows credential targets
/// and file names compare case-insensitively, which would merge digests that differ only in case.
pub(super) fn hex_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(text.as_bytes()))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

#[allow(dead_code)] // used by keychain-backed key provisioning (feature `secrets`)
pub(super) fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}
