//! Owner-only folders with a per-process trust cache. [`ensure_dir`] and
//! [`ensure_private_dir_repairing`] wrap [`crate::private::ensure_private_dir`] so a folder checked
//! recently is not re-checked (and its security descriptor not re-read) on every write, and a
//! folder an elevated run created on Windows is moved aside and recreated instead of failing for good.

use crate::FsError;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a successful owner-only check of a folder is trusted. `ensure_private_dir` reads and
/// may rewrite the folder's security descriptor (GetNamedSecurityInfo / SetNamedSecurityInfo on
/// Windows: ntmarta, ntfs and the filter stack), and a store that called it before every read-modify-write, lock
/// and append on a 5 s tick kept its process at ~29% of a core. The first use
/// of a folder is always checked; a later call re-checks when the folder is gone, when the
/// remembered check is older than this, or after `forget_private_dir` (a write that failed with
/// access denied).
const PRIVATE_DIR_TRUST: std::time::Duration = std::time::Duration::from_secs(300);

type DirIdentity = [u64; 3];

/// Identity of the folder a check was made on, so a folder deleted and recreated (possibly with a
/// weaker ACL) inside the trust window is a miss. Unix: (dev, ino, uid). Windows: creation time and
/// file attributes (std's file index is unstable). None when it cannot be read.
fn private_dir_identity(meta: &std::fs::Metadata) -> Option<DirIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some([meta.dev(), meta.ino(), u64::from(meta.uid())])
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let created = meta.created().ok()?;
        let nanos = created
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos() as u64;
        Some([nanos, u64::from(meta.file_attributes()), 0])
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        None
    }
}

type PrivateDirCache =
    std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::Instant, DirIdentity)>>;

fn private_dir_cache() -> &'static PrivateDirCache {
    static CACHE: std::sync::OnceLock<PrivateDirCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// True when `dir` passed the owner-only check recently in this process and still exists as the
/// same folder (one metadata call, no security descriptor read). A link is never trusted from the
/// cache, and a changed or unreadable folder identity is a miss.
pub fn private_dir_verified(dir: &Path) -> bool {
    let Some((at, identity)) = private_dir_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(dir).copied())
    else {
        return false;
    };
    if at.elapsed() >= PRIVATE_DIR_TRUST {
        return false;
    }
    std::fs::symlink_metadata(dir)
        .ok()
        .filter(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        .and_then(|meta| private_dir_identity(&meta))
        .is_some_and(|now| now == identity)
}

/// Remember that `dir` just passed `crate::private::ensure_private_dir`. A folder whose identity
/// cannot be read is not remembered.
pub fn note_private_dir(dir: &Path) {
    let Some(identity) = std::fs::symlink_metadata(dir)
        .ok()
        .and_then(|meta| private_dir_identity(&meta))
    else {
        return;
    };
    if let Ok(mut cache) = private_dir_cache().lock() {
        if cache.len() > 512 {
            cache.clear();
        }
        cache.insert(dir.to_path_buf(), (std::time::Instant::now(), identity));
    }
}

/// Drop the remembered check for `dir` (a write there failed with access denied), so the next
/// call checks and repairs it again.
pub fn forget_private_dir(dir: &Path) {
    if let Ok(mut cache) = private_dir_cache().lock() {
        cache.remove(dir);
    }
}

/// Makes `dir` and its missing parents owner-only (RightKit's `ensure_private_dir`), so a run from an
/// elevated shell on Windows makes them owned by this user, never by Administrators. A
/// folder that already exists but is owned by another principal is still used as it was before,
/// with nothing moved: what the caller keeps there (a waiting message) stays where it is.
pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    if private_dir_verified(dir) {
        return Ok(());
    }
    match crate::private::ensure_private_dir(dir) {
        Ok(()) => {
            note_private_dir(dir);
            Ok(())
        }
        Err(FsError::NotOwned(_)) if dir.is_dir() => Ok(()),
        Err(other) => Err(other.into()),
    }
}

/// `crate::private::ensure_private_dir`, for a folder that must be usable by this process but may
/// have been created by an elevated one. On Windows a folder made by an elevated process (a
/// shell or agent started "as administrator", the installer, a hook run) is owned by
/// `BUILTIN\Administrators`, and RightKit refuses it for an unelevated caller
/// (`FsError::NotOwned`, `restrict_owner_only`), so every later call
/// failed for good. The folder is then moved aside, never entered,
/// read or reused, and a new one is created owner-only by this user in its place; what it held is
/// left under the returned name for the user to look at or delete. The check keeps its purpose:
/// nothing owned by another principal is ever trusted, and the parent is the user's own folder, so
/// only that user, an administrator or the system could have made the folder at all. `Ok(Some(name))`
/// is a repair (callers log it); a folder that cannot be moved aside fails with the original reason
/// and the name of the folder.
pub fn ensure_private_dir_repairing(dir: &Path) -> io::Result<Option<PathBuf>> {
    if private_dir_verified(dir) {
        return Ok(None);
    }
    match crate::private::ensure_private_dir(dir) {
        Ok(()) => {
            note_private_dir(dir);
            Ok(None)
        }
        Err(FsError::NotOwned(_)) if cfg!(windows) => {
            let Some(name) = dir.file_name() else {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} is not owned by the current user", dir.display()),
                ));
            };
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut aside_name = name.to_os_string();
            aside_name.push(format!(".admin-owned-{stamp}"));
            let aside = dir.with_file_name(aside_name);
            std::fs::rename(dir, &aside).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "{} is not owned by the current user (made by an elevated run?) and could not be moved aside: {error}",
                        dir.display()
                    ),
                )
            })?;
            crate::private::ensure_private_dir(dir)?;
            Ok(Some(aside))
        }
        Err(other) => Err(other.into()),
    }
}
