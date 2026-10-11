use std::io;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, FsError>;

/// A step that runs after an atomic write has already published the new content. See
/// [`FsError::ReplacedButPostStepFailed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PostPublishStep {
    /// Removing the temp name that is left behind when publication is a hard link followed by an
    /// unlink (the fallback where the kernel has no exclusive rename). The content is published and
    /// complete; a leftover temp name remains, which [`crate::sweep_stale_temps`] reclaims.
    TempCleanup,
    /// Making the published file owner-only on Windows. No longer reported: a private write is
    /// published by a move that carries the temp file's owner-only ACL, so there is no step after
    /// publication. The variant stays for existing matches.
    OwnerOnlyAcl,
}

impl std::fmt::Display for PostPublishStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TempCleanup => "removing the temp file",
            Self::OwnerOnlyAcl => "making the file owner-only",
        })
    }
}

/// Every failure this crate reports. The enum is `#[non_exhaustive]`: match with a wildcard arm,
/// because variants are added as the crate learns to report more precisely (0.3.4 added
/// [`FsError::ReplacedButDirSyncFailed`], [`FsError::DirSyncFailed`] and
/// [`FsError::ReplacedButPostStepFailed`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FsError {
    #[error("I/O failure on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{0} is or traverses a symlink / reparse point, which is refused")]
    Symlink(PathBuf),
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("{0} is not a regular file")]
    NotAFile(PathBuf),
    #[error("{0} is not owned by the current user")]
    NotOwned(PathBuf),
    #[error("{0} is writable by group or others")]
    TooPermissive(PathBuf),
    #[error("directory identity changed while it was being pinned: {0}")]
    IdentityChanged(PathBuf),
    #[error("invalid entry name {0:?}")]
    BadName(String),
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),
    #[error("{path} is {size} bytes, over the {limit} byte limit")]
    TooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
    #[error("{path} is locked by {owner}")]
    Locked { path: PathBuf, owner: String },
    #[error("timed out after {waited_ms} ms waiting for lock {path}")]
    LockTimeout { path: PathBuf, waited_ms: u64 },
    #[error("entry cap of {0} exceeded")]
    TooManyEntries(usize),
    #[error("corrupt record {path}: {reason}")]
    Corrupt { path: PathBuf, reason: String },
    #[error("unknown stable id {0}")]
    UnknownId(String),
    #[error("platform primitive unavailable: {0}")]
    Unavailable(&'static str),
    /// The new content is in place at `path`, but syncing its directory failed, so the rename
    /// may not survive a power loss. Retrying the write is safe.
    #[error("{} was replaced, but syncing its directory failed: {source}", path.display())]
    ReplacedButDirSyncFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A `StateDir` removal or directory creation took effect, but syncing the containing
    /// directory failed, so it may not survive a power loss. Repeating the call is safe: a retry
    /// that finds a directory this process made and has not yet published syncs it again, and
    /// fails again if the sync still fails (a directory made by another process is not tracked).
    #[error("{} was changed, but syncing its directory failed: {source}", path.display())]
    DirSyncFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The new content is published at `path` and any required directory sync has run, but a step
    /// after publication failed (see [`PostPublishStep`]). The destination holds the new content,
    /// so [`FsError::target_replaced`] is true and retrying the write is safe. When several
    /// post-publication steps fail, the most serious is reported: the owner-only ACL first, then
    /// the directory sync ([`FsError::ReplacedButDirSyncFailed`]), then temp cleanup.
    #[error("{} was replaced, but {step} failed: {source}", path.display())]
    ReplacedButPostStepFailed {
        path: PathBuf,
        step: PostPublishStep,
        #[source]
        source: io::Error,
    },
    /// Windows `ReplaceFileW` failed after it had begun swapping names, so the destination is not
    /// in a known state: it may be missing. Nothing was deleted by this crate. `new_content` is
    /// the file that still holds the new bytes (the temp file) and `previous_content` the one that
    /// holds the old bytes (the backup file, or the destination itself); each is `None` when no
    /// such file exists. Both are left in place for recovery. [`FsError::target_replaced`] is
    /// false: the new content is not at the destination.
    #[error(
        "replacing {} was interrupted; new content: {}, previous content: {}: {source}",
        path.display(),
        show_path(new_content),
        show_path(previous_content)
    )]
    ReplaceInterrupted {
        path: PathBuf,
        new_content: Option<PathBuf>,
        previous_content: Option<PathBuf>,
        #[source]
        source: io::Error,
    },
}

fn show_path(path: &Option<PathBuf>) -> String {
    path.as_ref()
        .map_or_else(|| "gone".to_string(), |p| p.display().to_string())
}

impl FsError {
    pub fn io(path: impl AsRef<Path>, source: io::Error) -> Self {
        Self::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }

    /// The underlying `io::ErrorKind` when this is an I/O (or already-exists) failure.
    pub fn kind(&self) -> Option<io::ErrorKind> {
        match self {
            Self::Io { source, .. }
            | Self::ReplacedButDirSyncFailed { source, .. }
            | Self::ReplacedButPostStepFailed { source, .. }
            | Self::ReplaceInterrupted { source, .. }
            | Self::DirSyncFailed { source, .. } => Some(source.kind()),
            Self::AlreadyExists(_) => Some(io::ErrorKind::AlreadyExists),
            _ => None,
        }
    }

    /// True when the destination already holds the new content, even though the call failed.
    pub fn target_replaced(&self) -> bool {
        matches!(
            self,
            Self::ReplacedButDirSyncFailed { .. } | Self::ReplacedButPostStepFailed { .. }
        )
    }

    pub fn is_not_found(&self) -> bool {
        self.kind() == Some(io::ErrorKind::NotFound)
    }

    /// The `io::ErrorKind` a caller of `std::io` should see for this failure (see the
    /// `From<FsError> for io::Error` impl). Variants that carry an `io::Error` keep its kind.
    fn io_kind(&self) -> io::ErrorKind {
        use io::ErrorKind as K;
        match self {
            Self::Io { source, .. }
            | Self::ReplacedButDirSyncFailed { source, .. }
            | Self::ReplacedButPostStepFailed { source, .. }
            | Self::ReplaceInterrupted { source, .. }
            | Self::DirSyncFailed { source, .. } => source.kind(),
            Self::Symlink(_) | Self::NotAFile(_) | Self::BadName(_) => K::InvalidInput,
            Self::NotADirectory(_) => K::NotADirectory,
            Self::NotOwned(_) | Self::TooPermissive(_) => K::PermissionDenied,
            Self::IdentityChanged(_) => K::Other,
            Self::AlreadyExists(_) => K::AlreadyExists,
            Self::TooLarge { .. } => K::FileTooLarge,
            Self::Locked { .. } => K::WouldBlock,
            Self::LockTimeout { .. } => K::TimedOut,
            Self::TooManyEntries(_) => K::Other,
            Self::Corrupt { .. } => K::InvalidData,
            Self::UnknownId(_) => K::NotFound,
            Self::Unavailable(_) => K::Unsupported,
        }
    }
}

/// Lets a caller of `std::io` keep the kind (`AlreadyExists`, `NotFound`, `PermissionDenied`,
/// and so on) without matching on message text. The original `FsError` is kept as the
/// `io::Error`'s inner error, so `get_ref()` and `into_inner()` recover it and its `source()` chain
/// still reaches the OS error.
impl From<FsError> for io::Error {
    fn from(err: FsError) -> Self {
        let kind = err.io_kind();
        io::Error::new(kind, err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convert and check that the kind is `expected` and the original `FsError` is kept inside.
    fn assert_mapped(err: FsError, expected: io::ErrorKind) {
        let label = format!("{err:?}");
        let io_err: io::Error = err.into();
        assert_eq!(io_err.kind(), expected, "{label}");
        let inner = io_err
            .get_ref()
            .and_then(|e| e.downcast_ref::<FsError>())
            .unwrap_or_else(|| panic!("{label}: original FsError not kept"));
        assert_eq!(inner.io_kind(), expected, "{label}");
    }

    #[test]
    fn io_variant_keeps_the_source_kind() {
        let p = PathBuf::from("/x");
        assert_mapped(
            FsError::io(&p, io::Error::from(io::ErrorKind::PermissionDenied)),
            io::ErrorKind::PermissionDenied,
        );
        assert_mapped(
            FsError::io(&p, io::Error::from(io::ErrorKind::NotFound)),
            io::ErrorKind::NotFound,
        );
    }

    #[test]
    fn already_exists_maps_to_already_exists() {
        assert_mapped(
            FsError::AlreadyExists(PathBuf::from("/x")),
            io::ErrorKind::AlreadyExists,
        );
    }

    #[test]
    fn each_other_variant_maps_deliberately() {
        let p = PathBuf::from("/x");
        let cases: Vec<(FsError, io::ErrorKind)> = vec![
            (FsError::Symlink(p.clone()), io::ErrorKind::InvalidInput),
            (
                FsError::NotADirectory(p.clone()),
                io::ErrorKind::NotADirectory,
            ),
            (FsError::NotAFile(p.clone()), io::ErrorKind::InvalidInput),
            (
                FsError::NotOwned(p.clone()),
                io::ErrorKind::PermissionDenied,
            ),
            (
                FsError::TooPermissive(p.clone()),
                io::ErrorKind::PermissionDenied,
            ),
            (FsError::IdentityChanged(p.clone()), io::ErrorKind::Other),
            (FsError::BadName("a/b".into()), io::ErrorKind::InvalidInput),
            (
                FsError::TooLarge {
                    path: p.clone(),
                    size: 9,
                    limit: 4,
                },
                io::ErrorKind::FileTooLarge,
            ),
            (
                FsError::Locked {
                    path: p.clone(),
                    owner: "other".into(),
                },
                io::ErrorKind::WouldBlock,
            ),
            (
                FsError::LockTimeout {
                    path: p.clone(),
                    waited_ms: 5,
                },
                io::ErrorKind::TimedOut,
            ),
            (FsError::TooManyEntries(3), io::ErrorKind::Other),
            (
                FsError::Corrupt {
                    path: p.clone(),
                    reason: "bad".into(),
                },
                io::ErrorKind::InvalidData,
            ),
            (FsError::UnknownId("id".into()), io::ErrorKind::NotFound),
            (FsError::Unavailable("flock"), io::ErrorKind::Unsupported),
        ];
        for (err, kind) in cases {
            assert_mapped(err, kind);
        }
    }

    #[test]
    fn post_publish_variants_keep_the_source_kind() {
        let p = PathBuf::from("/x");
        let source = || io::Error::from(io::ErrorKind::StorageFull);
        assert_mapped(
            FsError::ReplacedButDirSyncFailed {
                path: p.clone(),
                source: source(),
            },
            io::ErrorKind::StorageFull,
        );
        assert_mapped(
            FsError::DirSyncFailed {
                path: p.clone(),
                source: source(),
            },
            io::ErrorKind::StorageFull,
        );
        assert_mapped(
            FsError::ReplaceInterrupted {
                path: p.clone(),
                new_content: Some(PathBuf::from("/x.tmp")),
                previous_content: None,
                source: source(),
            },
            io::ErrorKind::StorageFull,
        );
        assert_mapped(
            FsError::ReplacedButPostStepFailed {
                path: p,
                step: PostPublishStep::TempCleanup,
                source: source(),
            },
            io::ErrorKind::StorageFull,
        );
    }
}
