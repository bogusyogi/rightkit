use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::options::{ReplaceMode, SharingRetry, WriteSecurity};
use crate::store::Error;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` so readers see the old or the new file, never a mix. Missing parent directories
/// are created by `rightkit_fs` itself (`create_parents`), which on Unix and macOS syncs the link
/// of every ancestor this process has not yet published before the file is written below it;
/// the file is then replaced through `rightkit_fs::atomic_write_with_options` (temp file, flush,
/// replace, directory sync). On Windows no directory flush is made. A
/// failure after the file was replaced is returned as an error whose message says so; see
/// [`crate::Error::Replaced`] for the typed form used by [`crate::Store`].
pub fn atomic_write(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_mode(dest, bytes, &WriteMode::default()).map_err(into_io)
}

/// How the settings file is published. The default is today's behaviour.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteMode {
    pub replace: ReplaceMode,
    pub durable: bool,
    pub retry: Option<SharingRetry>,
    /// Who may read the written file. Translated into `rightkit_fs::Privacy` by [`fs_options`];
    /// this crate holds no permission or ACL code of its own.
    pub security: WriteSecurity,
}

impl Default for WriteMode {
    fn default() -> Self {
        Self {
            replace: ReplaceMode::Rename,
            durable: true,
            retry: None,
            security: WriteSecurity::Default,
        }
    }
}

/// The `rightkit_fs` options a [`WriteMode`] stands for. Shared by the write and by
/// [`crate::StoreOptions::achieved_durability`], so the two cannot disagree.
pub(crate) fn fs_options(mode: &WriteMode) -> rightkit_fs::AtomicWriteOptions {
    rightkit_fs::AtomicWriteOptions {
        durable: mode.durable,
        create_parents: true,
        replace: match mode.replace {
            ReplaceMode::Rename => rightkit_fs::ReplaceMode::Rename,
            ReplaceMode::MoveWriteThrough => rightkit_fs::ReplaceMode::MoveWriteThrough,
        },
        retry_sharing_violation: mode
            .retry
            .map(|r| rightkit_fs::RetryPolicy::new(r.attempts, r.delay)),
        privacy: match mode.security {
            WriteSecurity::Default => rightkit_fs::Privacy::Inherit,
            WriteSecurity::OwnerPrivate => rightkit_fs::Privacy::OwnerOnly,
        },
        ..Default::default()
    }
}

/// [`atomic_write`] with an explicit replace mode, durability flag, and sharing-violation retry.
/// `Error::Io` means nothing was published; `Error::Replaced` means the new file is in place.
pub(crate) fn atomic_write_mode(dest: &Path, bytes: &[u8], mode: &WriteMode) -> Result<(), Error> {
    // Parents are made by the write itself, not here: a plain `create_dir_all` creates them with
    // no directory sync, so a durable write below them could lose the whole path.
    parent_of(dest)?;
    rightkit_fs::atomic_write_with_options(dest, fs_options(mode), |f| f.write_all(bytes))
        .map_err(|e| publish_error(dest, e))
}

/// As [`atomic_write`] with an injectable replace step (used to prove the old file survives a
/// failed replace). `replace(temp, dest)` must move `temp` over `dest`; the default is
/// `fs::rename`, which replaces an existing file on every platform. Missing parents are made
/// durably (see [`atomic_write`]); the replace itself is the caller's, so this makes no
/// directory sync after it: use [`atomic_write`] for a durable publication.
pub fn atomic_write_with(
    dest: &Path,
    bytes: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = parent_of(dest)?;
    rightkit_fs::atomic::create_dir_all_durable(parent, false).map_err(io::Error::from)?;
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let (tmp, mut file) = loop {
        let cand = parent.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new().write(true).create_new(true).open(&cand) {
            Ok(f) => break (cand, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&tmp, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn parent_of(dest: &Path) -> io::Result<&Path> {
    dest.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))
}

/// Sorts a publish failure. The two post-publication variants mean the destination already holds
/// the new bytes, so they must not be reported as a plain I/O failure (a caller would retry).
fn publish_error(dest: &Path, e: rightkit_fs::FsError) -> Error {
    match e {
        rightkit_fs::FsError::ReplacedButDirSyncFailed { source, .. }
        | rightkit_fs::FsError::ReplacedButPostStepFailed { source, .. } => Error::Replaced {
            path: dest.to_path_buf(),
            source,
        },
        // The requested owner-only protection could not be established; nothing was published.
        refused @ (rightkit_fs::FsError::UnsafeAcl(_)
        | rightkit_fs::FsError::TooPermissive(_)
        | rightkit_fs::FsError::NotOwned(_)) => Error::Protection {
            path: dest.to_path_buf(),
            reason: refused.to_string(),
        },
        rightkit_fs::FsError::Io { source, .. } => Error::Io(source),
        other => Error::Io(io::Error::other(other)),
    }
}

fn into_io(e: Error) -> io::Error {
    match e {
        Error::Io(source) => source,
        other => io::Error::other(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_fs::{FsError, Privacy};
    use std::path::PathBuf;

    #[test]
    fn the_default_profile_asks_for_no_privacy_step() {
        let options = fs_options(&WriteMode::default());
        assert_eq!(options.effective_privacy(), Privacy::Inherit);
    }

    #[test]
    fn the_owner_private_profile_asks_the_fs_layer_for_owner_only() {
        let mode = WriteMode {
            security: WriteSecurity::OwnerPrivate,
            ..WriteMode::default()
        };
        assert_eq!(fs_options(&mode).effective_privacy(), Privacy::OwnerOnly);
        // Nothing else about the write changes with the profile.
        let plain = fs_options(&WriteMode::default());
        let private = fs_options(&mode);
        assert_eq!(private.durable, plain.durable);
        assert_eq!(private.create_parents, plain.create_parents);
        assert_eq!(private.replace, plain.replace);
    }

    #[test]
    fn a_refused_protection_is_a_typed_error_not_an_io_error() {
        let dest = PathBuf::from("settings.json");
        for refused in [
            FsError::UnsafeAcl(dest.clone()),
            FsError::TooPermissive(dest.clone()),
            FsError::NotOwned(dest.clone()),
        ] {
            match publish_error(&dest, refused) {
                Error::Protection { path, reason } => {
                    assert_eq!(path, dest);
                    assert!(!reason.is_empty());
                }
                other => panic!("expected Error::Protection, got {other:?}"),
            }
        }
    }

    #[test]
    fn other_publish_failures_keep_their_classification() {
        let dest = PathBuf::from("settings.json");
        let io = FsError::Io {
            path: dest.clone(),
            source: io::Error::other("disk"),
        };
        assert!(matches!(publish_error(&dest, io), Error::Io(_)));
    }
}
