//! Hold between sibling sub-agent starts that share a prompt prefix (A224).
//!
//! Several `spawn_subagent` calls from one model reply start children whose requests share the
//! same long prefix. Started together, each one writes the provider's prompt cache on its own;
//! started one after the other, the later ones read what the first wrote. The first sibling for a
//! `(parent session, prefix)` pair starts at once; the others wait until it has streamed its first
//! byte (a cache write has begun by then) or until the configured hold
//! (`[orchestration] sibling-start-hold-ms`, 5000 by default, `0` = no hold) runs out, whichever
//! comes first. The wait never outlives the cap and ends when the first sibling ends, whatever
//! the reason, so a sibling that fails to start holds nobody up.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::watch;

type Key = (String, String);

struct Entry {
    released: watch::Sender<bool>,
}

fn registry() -> &'static Mutex<HashMap<Key, Arc<Entry>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<Key, Arc<Entry>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// What a sibling is: the one that goes first, or one that waits for it.
pub enum SiblingStart {
    Leader(LeaderGuard),
    Follower(FollowerWait),
}

/// Held by the first sibling for as long as it runs. Calling [`LeaderGuard::first_byte`] or dropping
/// the guard releases every sibling waiting on it.
pub struct LeaderGuard {
    key: Key,
    entry: Arc<Entry>,
}

impl LeaderGuard {
    /// The first sibling streamed its first byte: release the waiting siblings. Siblings that
    /// arrive later start at once (the cache is written), as new leaders.
    pub fn first_byte(&self) {
        self.release();
    }

    fn release(&self) {
        let _ = self.entry.released.send(true);
        let mut map = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if map
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.entry))
        {
            map.remove(&self.key);
        }
    }
}

impl Drop for LeaderGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// A sibling that waits for the leader's first byte.
pub struct FollowerWait {
    released: watch::Receiver<bool>,
}

impl FollowerWait {
    /// Wait for the leader's first byte, but no longer than `hold`. Returns `true` when the leader
    /// released it, `false` when the cap ended the wait.
    pub async fn wait(mut self, hold: Duration) -> bool {
        tokio::time::timeout(hold, self.released.wait_for(|released| *released))
            .await
            .is_ok()
    }
}

/// Register a sibling for `(parent_session_id, prefix_key)`: the first one gets
/// [`SiblingStart::Leader`], one that arrives while a leader is still pending gets
/// [`SiblingStart::Follower`].
pub fn claim(parent_session_id: &str, prefix_key: &str) -> SiblingStart {
    let key = (parent_session_id.to_string(), prefix_key.to_string());
    let mut map = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = map.get(&key) {
        return SiblingStart::Follower(FollowerWait {
            released: entry.released.subscribe(),
        });
    }
    let (released, _) = watch::channel(false);
    let entry = Arc::new(Entry { released });
    map.insert(key.clone(), entry.clone());
    SiblingStart::Leader(LeaderGuard { key, entry })
}
