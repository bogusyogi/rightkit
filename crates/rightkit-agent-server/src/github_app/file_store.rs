use super::store::{BackendError, GitHubAppStateBackend};
use std::fs;
use std::path::{Path, PathBuf};

/// File-backend construction failures. Lock contention is distinct so daemon
/// startup can report unavailable ownership instead of creating a second store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStoreError {
    InvalidPath(String),
    Unavailable(String),
    LockRequired(String),
}

impl std::fmt::Display for FileStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(reason) => {
                write!(formatter, "invalid GitHub App state path: {reason}")
            }
            Self::Unavailable(reason) => {
                write!(formatter, "GitHub App state file unavailable: {reason}")
            }
            Self::LockRequired(reason) => {
                write!(formatter, "GitHub App state lock required: {reason}")
            }
        }
    }
}

impl std::error::Error for FileStoreError {}

/// Production backend using `rightkit-fs` for owner-only atomic
/// writes and its stable daemon lock for cross-process exclusivity.
#[derive(Debug)]
pub struct FileStateBackend {
    state_path: PathBuf,
    _lock: rightkit_fs::FileLock,
}

impl FileStateBackend {
    pub fn open(state_path: impl Into<PathBuf>) -> Result<Self, FileStoreError> {
        let state_path = state_path.into();
        validate_state_path(&state_path)?;
        // Stable sibling lock file: `<state>.daemon.lock` (same name the
        // retired private-fs `DaemonDbLock` used), held for the backend's life.
        let canonical = state_path
            .canonicalize()
            .unwrap_or_else(|_| state_path.clone());
        let lock = rightkit_fs::FileLock::try_acquire(&canonical.with_extension("daemon.lock"))
            .map_err(|error| {
                FileStoreError::LockRequired(format!("{}: {error}", state_path.display()))
            })?;
        Ok(Self {
            state_path,
            _lock: lock,
        })
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }
}

impl GitHubAppStateBackend for FileStateBackend {
    fn load_bytes(&self) -> Result<Option<Vec<u8>>, BackendError> {
        let metadata = match fs::symlink_metadata(&self.state_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(BackendError::Unavailable(format!(
                    "read metadata {}: {error}",
                    self.state_path.display()
                )))
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(BackendError::Unavailable(format!(
                "state path is not a regular file: {}",
                self.state_path.display()
            )));
        }
        rightkit_fs::restrict_owner_only(&self.state_path).map_err(|error| {
            BackendError::Unavailable(format!(
                "restrict permissions {}: {error}",
                self.state_path.display()
            ))
        })?;
        fs::read(&self.state_path).map(Some).map_err(|error| {
            BackendError::Unavailable(format!("read {}: {error}", self.state_path.display()))
        })
    }

    fn replace_bytes(&self, bytes: &[u8]) -> Result<(), BackendError> {
        rightkit_fs::write_private_file(&self.state_path, bytes).map_err(|error| {
            BackendError::Unavailable(format!(
                "atomic write {}: {error}",
                self.state_path.display()
            ))
        })
    }
}

fn validate_state_path(state_path: &Path) -> Result<(), FileStoreError> {
    if state_path.as_os_str().is_empty() || state_path.file_name().is_none() {
        return Err(FileStoreError::InvalidPath(
            "state path must name a file".to_string(),
        ));
    }
    let parent = state_path.parent().unwrap_or_else(|| Path::new("."));
    let metadata = fs::symlink_metadata(parent).map_err(|error| {
        FileStoreError::Unavailable(format!("state parent {}: {error}", parent.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(FileStoreError::Unavailable(format!(
            "state parent is not a regular directory: {}",
            parent.display()
        )));
    }
    if let Ok(metadata) = fs::symlink_metadata(state_path) {
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(FileStoreError::Unavailable(format!(
                "state path is not safe: {}",
                state_path.display()
            )));
        }
        if metadata.is_dir() {
            return Err(FileStoreError::InvalidPath(
                "state path names a directory".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_app::{
        AccessLevel, GitHubAppStore, GitHubPermission, GitHubStoreError, InstallationBinding,
        RepositoryScope,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn binding() -> InstallationBinding {
        InstallationBinding::new(
            7,
            9,
            Some(11),
            "acme",
            RepositoryScope::Explicit(BTreeSet::from([42])),
            BTreeMap::from([(GitHubPermission::Issues, AccessLevel::Write)]),
            1,
        )
        .unwrap()
    }

    #[test]
    fn file_backend_round_trips_and_reopens_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github-app-state.json");
        let backend = FileStateBackend::open(&path).unwrap();
        let store = GitHubAppStore::open(backend, 8, 300).unwrap();
        store.replace_binding(binding()).unwrap();
        drop(store);
        let reopened =
            GitHubAppStore::open(FileStateBackend::open(&path).unwrap(), 8, 300).unwrap();
        assert_eq!(reopened.binding(7, 9).unwrap().generation, 1);
    }

    #[test]
    fn concurrent_backend_open_requires_existing_workspace_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github-app-state.json");
        let first = FileStateBackend::open(&path).unwrap();
        let second = FileStateBackend::open(&path);
        assert!(matches!(second, Err(FileStoreError::LockRequired(_))));
        drop(first);
        assert!(FileStateBackend::open(&path).is_ok());
    }

    #[test]
    fn corrupt_file_is_typed_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github-app-state.json");
        fs::write(&path, b"not-json").unwrap();
        let result = GitHubAppStore::open(FileStateBackend::open(&path).unwrap(), 8, 300);
        assert!(matches!(result, Err(GitHubStoreError::LoadCorrupt(_))));
    }

    #[test]
    fn missing_parent_is_unavailable_without_directory_creation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing").join("state.json");
        assert!(matches!(
            FileStateBackend::open(path),
            Err(FileStoreError::Unavailable(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn published_state_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github-app-state.json");
        let backend = FileStateBackend::open(&path).unwrap();
        let store = GitHubAppStore::open(backend, 8, 300).unwrap();
        store.replace_binding(binding()).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
