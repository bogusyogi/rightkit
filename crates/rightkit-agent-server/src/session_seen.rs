//! Which sessions the person has looked at and which they pinned (B-cxagentcenter): the unread
//! tag and the star of the `/resume` picker. One small file, `session-seen.json`, under the state
//! root, so both survive a restart:
//!
//! ```json
//! {"pinned": ["session-id"], "seen": {"session-id": "2026-11-13T10:00:00Z"}}
//! ```
//!
//! `seen` holds the `updatedAt` a session had when it was last open in a terminal. A session is
//! unread when it moved after that, or when it was never seen here and moved since this process
//! started (an older session nobody opened is old news, not unread).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "session-seen.json";
/// Entries kept in `seen`: the newest are kept when the file grows past this.
const MAX_SEEN: usize = 2000;

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeenStore {
    #[serde(default)]
    pub pinned: BTreeSet<String>,
    #[serde(default)]
    pub seen: BTreeMap<String, String>,
}

impl SeenStore {
    pub fn is_pinned(&self, id: &str) -> bool {
        self.pinned.contains(id)
    }

    /// Pin the session, or unpin it when it is pinned. Returns whether it is pinned now.
    pub fn toggle_pin(&mut self, id: &str) -> bool {
        if !self.pinned.remove(id) {
            self.pinned.insert(id.to_string());
            return true;
        }
        false
    }

    /// Record that the session was open at `updated_at`. True when this changed the store.
    pub fn mark_seen(&mut self, id: &str, updated_at: &str) -> bool {
        if updated_at.is_empty()
            || self
                .seen
                .get(id)
                .is_some_and(|known| known.as_str() >= updated_at)
        {
            return false;
        }
        self.seen.insert(id.to_string(), updated_at.to_string());
        if self.seen.len() > MAX_SEEN {
            // Drop the entries with the oldest seen stamp.
            let mut by_age: Vec<(String, String)> = self
                .seen
                .iter()
                .map(|(k, v)| (v.clone(), k.clone()))
                .collect();
            by_age.sort();
            for (_, key) in by_age.into_iter().take(self.seen.len() - MAX_SEEN) {
                self.seen.remove(&key);
            }
        }
        true
    }

    /// Unread: moved after it was last seen, or never seen and moved since `started_at`.
    /// Timestamps are RFC 3339 in one zone, so string order is time order.
    pub fn is_unread(&self, id: &str, updated_at: &str, started_at: &str) -> bool {
        if updated_at.is_empty() {
            return false;
        }
        match self.seen.get(id) {
            Some(seen) => updated_at > seen.as_str(),
            None => updated_at >= started_at,
        }
    }
}

fn store_path() -> Option<PathBuf> {
    rightkit_agent_config::state_root().map(|root| root.join(FILE_NAME))
}

fn load_from(path: &Path) -> SeenStore {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_to(path: &Path, store: &SeenStore) {
    let Ok(text) = serde_json::to_string_pretty(store) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Write beside, then rename: a crash never leaves half a file.
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, text).is_ok() {
        let _ = std::fs::rename(&temp, path);
    }
}

pub fn load() -> SeenStore {
    store_path()
        .map(|path| load_from(&path))
        .unwrap_or_default()
}

pub fn save(store: &SeenStore) {
    if let Some(path) = store_path() {
        save_to(&path, store);
    }
}

/// Pin or unpin `id` in the stored file; true when it is pinned afterwards.
pub fn toggle_pin(id: &str) -> bool {
    let mut store = load();
    let pinned = store.toggle_pin(id);
    save(&store);
    pinned
}

/// The moment this process started, as RFC 3339 UTC: the line between "old" and "unread" for a
/// session no terminal has opened yet.
pub fn process_start() -> &'static str {
    static START: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    START.get_or_init(rightkit_agent_engine_protocol::now_iso)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_moved_after_it_was_seen_is_unread_and_seeing_it_again_clears_that() {
        let mut store = SeenStore::default();
        let start = "2026-11-13T10:00:00Z";
        assert!(store.mark_seen("a", "2026-11-13T10:05:00Z"));
        assert!(!store.is_unread("a", "2026-11-13T10:05:00Z", start));
        assert!(store.is_unread("a", "2026-11-13T10:06:00Z", start));
        assert!(store.mark_seen("a", "2026-11-13T10:06:00Z"));
        assert!(!store.is_unread("a", "2026-11-13T10:06:00Z", start));
        // Never moving backwards.
        assert!(!store.mark_seen("a", "2026-11-13T10:00:00Z"));
    }

    #[test]
    fn a_session_never_seen_is_unread_only_when_it_moved_since_this_process_started() {
        let store = SeenStore::default();
        let start = "2026-11-13T10:00:00Z";
        assert!(!store.is_unread("old", "2026-11-12T09:00:00Z", start));
        assert!(store.is_unread("new", "2026-11-13T10:30:00Z", start));
        assert!(!store.is_unread("none", "", start));
    }

    #[test]
    fn pinning_toggles_and_the_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("seen-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        let mut store = SeenStore::default();
        assert!(store.toggle_pin("a"));
        assert!(store.is_pinned("a"));
        store.mark_seen("b", "2026-11-13T10:00:00Z");
        save_to(&path, &store);
        assert_eq!(load_from(&path), store);
        assert!(!store.toggle_pin("a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_file_is_an_empty_store() {
        let dir = std::env::temp_dir().join(format!("seen-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(FILE_NAME);
        assert_eq!(load_from(&path), SeenStore::default());
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_from(&path), SeenStore::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
