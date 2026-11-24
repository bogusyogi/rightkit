//! Alerts for what waits for the owner's approval (moved from Pulse's `engine/src/push.rs`).
//!
//! Each waiting approval is alerted once. While a computer is in use the alert waits
//! [`APPROVAL_ESCALATE_SECS`] from the first time it was seen, because that computer's own card is
//! in front of the owner; otherwise it goes at once. An approval answered before its turn is never
//! alerted. What was already alerted is kept as a list of ids in a small JSON file, so a restart
//! does not alert a schedule or a rule twice. This module decides and remembers; the host looks
//! up what waits, builds the alert and sends it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;

/// How long an approval that waits has the computers to itself while one is in use: after this the
/// phone is told as well (the routing rule's 3 minutes).
pub const APPROVAL_ESCALATE_SECS: i64 = 180;

struct State {
    first_seen: BTreeMap<String, i64>,
    alerted: BTreeSet<String>,
}

/// What the approvals pass remembers: when it first saw each waiting approval, and which it already
/// alerted (ids only, in `path`).
pub struct ApprovalsAlerts {
    path: PathBuf,
    state: Mutex<State>,
}

impl ApprovalsAlerts {
    /// Reads the alerted ids from `path` (none when it is missing or damaged).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let alerted = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        Self {
            path,
            state: Mutex::new(State {
                first_seen: BTreeMap::new(),
                alerted,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, alerted: &BTreeSet<String>) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let ids: Vec<&String> = alerted.iter().collect();
        if let Ok(bytes) = serde_json::to_vec(&ids) {
            let temporary = self.path.with_extension("json.tmp");
            if std::fs::write(&temporary, bytes).is_ok() {
                let _ = std::fs::rename(&temporary, &self.path);
            }
        }
    }

    /// The waiting approvals owed an alert now. Forgets what is no longer in `waiting`, notes the
    /// first sight of each new one, and returns the ones not yet alerted that are due: all of them
    /// when no computer is in use, else those first seen at least [`APPROVAL_ESCALATE_SECS`] ago.
    /// `computer_in_use` is asked only when something waits.
    pub fn due(
        &self,
        waiting: &[String],
        now: i64,
        computer_in_use: impl FnOnce() -> bool,
    ) -> Vec<String> {
        let mut state = self.lock();
        state.first_seen.retain(|id, _| waiting.contains(id));
        let before = state.alerted.len();
        state.alerted.retain(|id| waiting.contains(id));
        if state.alerted.len() != before {
            self.save(&state.alerted);
        }
        for id in waiting {
            state.first_seen.entry(id.clone()).or_insert(now);
        }
        if waiting.is_empty() {
            return Vec::new();
        }
        let in_use = computer_in_use();
        waiting
            .iter()
            .filter(|id| !state.alerted.contains(*id))
            .filter(|id| {
                !in_use
                    || now - state.first_seen.get(*id).copied().unwrap_or(now)
                        >= APPROVAL_ESCALATE_SECS
            })
            .cloned()
            .collect()
    }

    /// Records that `id` reached at least one phone, so it is not alerted again.
    pub fn mark_alerted(&self, id: String) {
        let mut state = self.lock();
        state.alerted.insert(id);
        self.save(&state.alerted);
    }
}
