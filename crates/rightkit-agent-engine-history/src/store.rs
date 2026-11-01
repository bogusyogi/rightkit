//! Persistent snapshot store — writes content under
//! `<workspace>/<state_dir>/history/<sanitized-path>/<timestamp_ns>.snap`.

use std::path::{Path, PathBuf};

pub struct HistoryStore {
    base: PathBuf,
}

/// A snapshot selected for a transactional restore.
///
/// The file remains in history until [`HistoryStore::consume_snapshot`] is
/// called after the caller's mutation authority has committed the restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySnapshot {
    path: PathBuf,
    pub content: String,
}

impl HistorySnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl HistoryStore {
    /// Store snapshots under `<workspace_root>/<state_dir>/history`.
    ///
    /// `state_dir` is the host app's hidden per-workspace directory name (for
    /// example `.myapp`); it is created on first snapshot and git-ignored.
    pub fn new(workspace_root: &Path, state_dir: &str) -> Self {
        Self {
            base: workspace_root.join(state_dir).join("history"),
        }
    }

    /// Snapshot `content` as the pre-edit state of `path`.
    /// `path` may be relative (to workspace) or absolute.
    pub fn snapshot(&self, path: &Path, content: &str) -> std::io::Result<()> {
        let dir = self.slot_dir(path);
        std::fs::create_dir_all(&dir)?;
        // Keep the snapshot store out of the user's VCS: the state dir is
        // internal state, not project content. (Tool file-discovery already
        // prunes the state dir; this keeps `git status` clean too.)
        if let Some(state_dir) = self.base.parent() {
            let gi = state_dir.join(".gitignore");
            if !gi.exists() {
                let _ = std::fs::write(&gi, "*\n");
            }
        }

        let ts = timestamp_ns();
        let snap = dir.join(format!("{ts:020}.snap"));
        std::fs::write(snap, content)
    }

    /// Read the most-recent snapshot for `path` without consuming it.
    /// Returns `Ok(None)` if no snapshot exists.
    pub fn peek_latest(&self, path: &Path) -> std::io::Result<Option<HistorySnapshot>> {
        let Some(latest) = self.latest_path(path)? else {
            return Ok(None);
        };
        Ok(Some(HistorySnapshot {
            content: std::fs::read_to_string(&latest)?,
            path: latest,
        }))
    }

    /// Consume exactly the snapshot previously returned by `peek_latest`.
    /// Refuse to remove it if a newer snapshot has appeared meanwhile.
    pub fn consume_snapshot(&self, snapshot: &HistorySnapshot) -> std::io::Result<()> {
        let Some(dir) = snapshot.path().parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "history snapshot no longer exists",
            ));
        };
        let Some(latest) = Self::latest_path_in_dir(dir)? else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "history snapshot no longer exists",
            ));
        };
        if latest != snapshot.path() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "a newer history snapshot exists",
            ));
        }
        std::fs::remove_file(snapshot.path())
    }

    /// Return and remove the most-recent snapshot for `path`.
    /// Returns `Ok(None)` if no snapshot exists.
    pub fn restore_latest(&self, path: &Path) -> std::io::Result<Option<String>> {
        let Some(snapshot) = self.peek_latest(path)? else {
            return Ok(None);
        };
        let content = snapshot.content.clone();
        self.consume_snapshot(&snapshot)?;
        Ok(Some(content))
    }

    fn latest_path(&self, path: &Path) -> std::io::Result<Option<PathBuf>> {
        Self::latest_path_in_dir(&self.slot_dir(path))
    }

    fn latest_path_in_dir(dir: &Path) -> std::io::Result<Option<PathBuf>> {
        if !dir.exists() {
            return Ok(None);
        }

        // Collect .snap files, sort descending (latest first by name).
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("snap"))
            .collect();
        entries.sort_unstable_by(|a, b| b.cmp(a));
        Ok(entries.into_iter().next())
    }

    fn slot_dir(&self, path: &Path) -> PathBuf {
        // Turn the path into a flat, filesystem-safe key.
        let key = sanitize(path);
        self.base.join(key)
    }
}

/// Replace path separators and other special chars with `_` so the result
/// is a single valid directory name component.
fn sanitize(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Monotonic timestamp in nanoseconds.  Falls back to seconds*1e9 if the
/// platform doesn't support sub-second precision (extremely unlikely).
fn timestamp_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn snapshot_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit");
        let p = Path::new("src/main.rs");

        store.snapshot(p, "version 1").unwrap();
        let restored = store.restore_latest(p).unwrap();
        assert_eq!(restored, Some("version 1".to_string()));

        // No more snapshots.
        let second = store.restore_latest(p).unwrap();
        assert_eq!(second, None);
    }

    #[test]
    fn multiple_snapshots_restore_in_lifo_order() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit");
        let p = Path::new("file.txt");

        store.snapshot(p, "v1").unwrap();
        // Ensure timestamps differ.
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.snapshot(p, "v2").unwrap();

        // Most recent is v2.
        let r1 = store.restore_latest(p).unwrap();
        assert_eq!(r1, Some("v2".to_string()));

        // Next is v1.
        let r2 = store.restore_latest(p).unwrap();
        assert_eq!(r2, Some("v1".to_string()));

        // Empty.
        let r3 = store.restore_latest(p).unwrap();
        assert_eq!(r3, None);
    }

    #[test]
    fn no_snapshot_returns_none() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit");
        let result = store.restore_latest(Path::new("nonexistent.rs")).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn absolute_path_snapshots_correctly() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit");
        let abs = dir.path().join("src").join("lib.rs");
        store.snapshot(&abs, "abs content").unwrap();
        let r = store.restore_latest(&abs).unwrap();
        assert_eq!(r, Some("abs content".to_string()));
    }

    #[test]
    fn peek_keeps_snapshot_until_explicit_consume() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit");
        let path = dir.path().join("transactional.txt");
        store.snapshot(&path, "before").unwrap();

        let snapshot = store.peek_latest(&path).unwrap().expect("snapshot");
        assert_eq!(snapshot.content, "before");
        assert!(store.peek_latest(&path).unwrap().is_some());

        store.consume_snapshot(&snapshot).unwrap();
        assert!(store.peek_latest(&path).unwrap().is_none());
    }
}
