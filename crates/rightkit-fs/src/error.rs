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
    /// Making the published file owner-only on Windows (`ReplaceFileW` keeps the destination's
    /// ACL, so a private write re-applies the owner-only ACL after the replace). When this fails
    /// the published file may be readable by more principals than the caller asked for.
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
    /// directory failed, so it may not survive a power loss. Repeating the call is safe, but a
    /// retry that finds the directory already present does not sync again.
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
}
