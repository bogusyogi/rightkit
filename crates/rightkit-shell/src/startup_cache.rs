//! Startup cache repair: keep the app's cache directory usable before a plugin needs it.
//!
//! Donor: ViewRight `startup_cache.rs`. A dangling symlink or a stray file where a cache directory
//! belongs makes `create_dir_all` fail with `AlreadyExists`, which aborts plugins that expect the
//! directory (`tauri-plugin-http` cookie cache). Cache content is disposable, so the bad entry is
//! replaced with a real directory.
//!
//! [`ensure_http_cache_dir`] resolves the platform user cache root (`~/Library/Caches`,
//! `%LOCALAPPDATA%`, or `$XDG_CACHE_HOME` / `~/.cache`) and repairs `<root>/<identifier>`. It needs
//! no Tauri handle, so it can run before the Tauri builder starts.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// What a cache-directory repair did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheDirOutcome {
    /// The directory was already usable.
    Existing,
    /// Nothing was there; the directory was created.
    Created,
    /// A dangling symlink or a file was replaced with a directory.
    Repaired,
}

/// Make `path` a usable directory, replacing a dangling symlink or a regular file.
pub fn ensure_cache_dir(path: &Path) -> io::Result<CacheDirOutcome> {
    let mut repaired = false;
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => return Ok(CacheDirOutcome::Existing),
        Ok(m) if m.file_type().is_symlink() => {
            if fs::metadata(path).is_ok_and(|t| t.is_dir()) {
                return Ok(CacheDirOutcome::Existing);
            }
            fs::remove_file(path)?;
            repaired = true;
        }
        Ok(_) => {
            fs::remove_file(path)?;
            repaired = true;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::create_dir_all(path)?;
    Ok(if repaired {
        CacheDirOutcome::Repaired
    } else {
        CacheDirOutcome::Created
    })
}

/// The platform user cache root that Tauri and `dirs` agree on.
pub fn user_cache_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join("Library").join("Caches"))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|h| !h.is_empty())
                    .map(|h| PathBuf::from(h).join(".cache"))
            })
    }
}

/// Repair `<user cache root>/<identifier>` so a plugin can create its cookie or HTTP cache there.
/// `identifier` is one path component (usually the bundle identifier): empty values, separators,
/// `.` and `..` are refused.
pub fn ensure_http_cache_dir(identifier: &str) -> io::Result<CacheDirOutcome> {
    let valid = !identifier.is_empty()
        && identifier != "."
        && identifier != ".."
        && !identifier.contains(['/', '\\', '\0']);
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache identifier must be one path component",
        ));
    }
    let root = user_cache_root().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "user cache directory unavailable")
    })?;
    ensure_cache_dir(&root.join(identifier))
}
