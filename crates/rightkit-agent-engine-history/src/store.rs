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
