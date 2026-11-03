//! A196: cross-session file-collision guard.
//!
//! The daemon hosts many sessions in one process. When two *different*
//! sessions edit the same file inside a short window, the later editor gets a
//! [`FileCollision`] warning naming the other session, so the user (or the
//! engine's runtime span) can see the overlap before one session silently
//! clobbers the other's work. The guard never blocks an edit; it arbitrates by
//! reporting, because the edit has already landed by the time typed mutation
//! evidence exists.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long another session's edit counts as "recent" for collision purposes.
pub const DEFAULT_COLLISION_WINDOW: Duration = Duration::from_secs(30 * 60);

/// Warning returned when a session edits a file another session edited recently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCollision {
    pub path: PathBuf,
    pub editing_session: String,
    pub other_session: String,
}

impl FileCollision {
    pub fn message(&self) -> String {
        format!(
            "file collision: {} was also edited by session {} recently; review before overwriting",
            self.path.display(),
            self.other_session
        )
    }
}

#[derive(Debug)]
struct Touch {
    session: String,
    at: Instant,
}

/// Tracks the most recent editor of each file, per distinct session.
#[derive(Debug)]
pub struct FileCollisionGuard {
    window: Duration,
    // path -> recent touches (at most one per session)
    touches: Mutex<HashMap<PathBuf, Vec<Touch>>>,
}

impl Default for FileCollisionGuard {
    fn default() -> Self {
        Self::new(DEFAULT_COLLISION_WINDOW)
    }
}

impl FileCollisionGuard {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            touches: Mutex::new(HashMap::new()),
        }
    }

    /// Process-wide guard shared by every session in this daemon.
    pub fn global() -> &'static FileCollisionGuard {
        static GUARD: OnceLock<FileCollisionGuard> = OnceLock::new();
        GUARD.get_or_init(FileCollisionGuard::default)
    }

    fn key(path: &Path) -> PathBuf {
        let normalized = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if cfg!(windows) {
            PathBuf::from(normalized.to_string_lossy().to_lowercase())
        } else {
            normalized
        }
    }

    /// Record that `session_id` edited `path`; returns one collision per other
    /// session that edited the same file within the window.
    pub fn record_edit(&self, session_id: &str, path: &Path) -> Vec<FileCollision> {
        self.record_edit_at(session_id, path, Instant::now())
    }

    pub(crate) fn record_edit_at(
        &self,
        session_id: &str,
        path: &Path,
        now: Instant,
    ) -> Vec<FileCollision> {
        let key = Self::key(path);
        let mut map = self.touches.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(key).or_default();
        entry.retain(|t| now.saturating_duration_since(t.at) <= self.window);
        let collisions = entry
            .iter()
            .filter(|t| t.session != session_id)
            .map(|t| FileCollision {
                path: path.to_path_buf(),
                editing_session: session_id.to_string(),
                other_session: t.session.clone(),
            })
            .collect();
        match entry.iter_mut().find(|t| t.session == session_id) {
            Some(t) => t.at = now,
            None => entry.push(Touch {
                session: session_id.to_string(),
                at: now,
            }),
        }
        collisions
    }

    /// Forget a session's touches (session closed), so a finished session
    /// stops triggering warnings.
    pub fn release_session(&self, session_id: &str) {
        let mut map = self.touches.lock().unwrap_or_else(|e| e.into_inner());
        for touches in map.values_mut() {
            touches.retain(|t| t.session != session_id);
        }
        map.retain(|_, v| !v.is_empty());
    }
}
