use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::options::{ReplaceMode, SharingRetry};
use crate::store::{Error, InterruptedReplace};

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
}

impl Default for WriteMode {
    fn default() -> Self {
        Self {
            replace: ReplaceMode::Rename,
            durable: true,
            retry: None,
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
            ReplaceMode::ReplaceFile => rightkit_fs::ReplaceMode::ReplaceFile,
            ReplaceMode::MoveWriteThrough => rightkit_fs::ReplaceMode::MoveWriteThrough,
        },
        retry_sharing_violation: mode
            .retry
            .map(|r| rightkit_fs::RetryPolicy::new(r.attempts, r.delay)),
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
        rightkit_fs::FsError::Io { source, .. } => Error::Io(source),
        // The swap started and did not finish: the settings file may be missing. Settings has no
        // variant that says so without claiming the new file is in place, and `Error` is not
        // non_exhaustive, so this maps to `Error::Io` carrying a typed `InterruptedReplace`.
        rightkit_fs::FsError::ReplaceInterrupted {
            path,
            new_content,
            previous_content,
            source,
        } => Error::Io(io::Error::new(
            source.kind(),
            InterruptedReplace {
                path,
                new_content,
                previous_content,
                source,
            },
        )),
        other => Error::Io(io::Error::other(other)),
    }
}

fn into_io(e: Error) -> io::Error {
    match e {
        Error::Io(source) => source,
        other => io::Error::other(other),
    }
}
