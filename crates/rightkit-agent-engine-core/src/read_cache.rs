//! Read-dedup cache — tracks which files have been read in a session so a
//! repeat read of an unchanged file can be elided to a short reference instead
//! of re-sending full contents to the model.

use rightkit_cache::prefix::fnv1a_64;
use std::collections::HashMap;

#[derive(Default)]
pub struct ReadCache {
    /// path -> content hash of what was last sent to the model.
    seen: HashMap<String, String>,
}

impl ReadCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `path` was read with the given content. Returns true if this
    /// is a NEW or CHANGED read (caller should send full contents), false if the
    /// identical content was already sent earlier (caller may elide).
    pub fn record(&mut self, path: &str, content: &str) -> bool {
        let hash = content_hash(content);
        match self.seen.get(path) {
            Some(prev) if prev == &hash => false,
            _ => {
                self.seen.insert(path.to_string(), hash);
                true
            }
        }
    }

    /// Whether an identical read of this path+content was already seen.
    pub fn already_seen(&self, path: &str, content: &str) -> bool {
        self.seen
            .get(path)
            .map(|h| h == &content_hash(content))
            .unwrap_or(false)
    }

    /// Forget every path. A compaction rewrites model-visible context, so an
    /// earlier "already sent" read no longer exists in what the model sees.
    pub fn clear(&mut self) {
        self.seen.clear();
    }

    /// Invalidate a path (e.g. after an edit), forcing the next read to send full contents.
    pub fn invalidate(&mut self, path: &str) {
        self.seen.remove(path);
    }
}

fn content_hash(s: &str) -> String {
    format!("{:016x}", fnv1a_64(s))
}
