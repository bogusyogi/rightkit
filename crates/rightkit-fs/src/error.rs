use std::io;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, FsError>;

#[derive(Debug, thiserror::Error)]
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
    /// directory failed, so it may not survive a power loss. Repeating the call is safe.
    #[error("{} was changed, but syncing its directory failed: {source}", path.display())]
    DirSyncFailed {
        path: PathBuf,
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
            | Self::DirSyncFailed { source, .. } => Some(source.kind()),
            Self::AlreadyExists(_) => Some(io::ErrorKind::AlreadyExists),
            _ => None,
        }
    }

    /// True when the destination already holds the new content, even though the call failed.
    pub fn target_replaced(&self) -> bool {
        matches!(self, Self::ReplacedButDirSyncFailed { .. })
    }

    pub fn is_not_found(&self) -> bool {
        self.kind() == Some(io::ErrorKind::NotFound)
    }
}
