//! Owner-keyed watch sessions drained by polling (CodeRight `FsWatchManager` port).
//!
//! A host that serves watches to several clients keeps one watch per `(owner, id)` pair: a client
//! registers a path, polls for debounced change batches, and releases everything it owns in one
//! call when it goes away. Each session is one OS watch on its path, its events folded through a
//! [`ChangeCoalescer`], so a batch reports net filesystem state.
//!
//! Batches are released when the session has been quiet for the debounce window, or when pending
//! changes have waited for [`crate::sync::DEFAULT_MAX_WAIT`], so a continuous storm still flushes.
//! A backend error or a rescan request does not drop information silently: the next batch carries
//! the watched root in `changed`, which tells the caller to rescan that root.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::coalesce::{ChangeBatch, ChangeCoalescer};
use crate::sync::{DEFAULT_DEBOUNCE, DEFAULT_MAX_WAIT};

#[derive(Default)]
struct SessionState {
    coalescer: ChangeCoalescer,
    /// Time of the most recent backend event (change, error or rescan request).
    last_event: Option<Instant>,
    /// Time the oldest still-undrained event arrived.
    first_pending: Option<Instant>,
    /// Backend error or rescan request since the last batch: the root must be rescanned.
    rescan: bool,
}

impl SessionState {
    fn has_pending(&self) -> bool {
        self.rescan || !self.coalescer.is_empty()
    }

    /// Folds one backend result into the state. Only results that change pending work start or
    /// refresh the timers. An ignored event (access, other) leaves them untouched, so it cannot
    /// make the first real change look overdue.
    fn absorb(&mut self, result: notify::Result<Event>, now: Instant) {
        match result {
            Ok(event) if event.need_rescan() => self.rescan = true,
            Ok(event) if is_relevant(&event) => self.coalescer.push(&event),
            Ok(_) => return,
            Err(_) => self.rescan = true,
        }
        self.last_event = Some(now);
        self.first_pending.get_or_insert(now);
    }

    /// Whether pending work is due at `now`: quiet for `debounce`, or pending past the max wait.
    fn due(&self, now: Instant, debounce: Duration) -> bool {
        let quiet = self
            .last_event
            .is_none_or(|t| now.duration_since(t) >= debounce);
        let overdue = self
            .first_pending
            .is_some_and(|t| now.duration_since(t) >= DEFAULT_MAX_WAIT);
        quiet || overdue
    }
}

/// Events that can change a file: create, modify and remove.
fn is_relevant(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

struct Session {
    root: PathBuf,
    state: Arc<Mutex<SessionState>>,
    /// Held only to keep the OS watch alive; dropping it stops the watch.
    _watcher: RecommendedWatcher,
}

/// Owner-keyed watch sessions with debounced poll drain.
///
/// Not internally threaded for polling: the caller drives it with [`WatchSessionManager::poll`].
/// Backend events are absorbed on the watcher's own thread.
pub struct WatchSessionManager {
    debounce: Duration,
    sessions: HashMap<(String, String), Session>,
}

impl Default for WatchSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchSessionManager {
    /// A manager with the crate's default debounce window ([`DEFAULT_DEBOUNCE`]).
    pub fn new() -> Self {
        Self {
            debounce: DEFAULT_DEBOUNCE,
            sessions: HashMap::new(),
        }
    }

    /// Replace the quiet window after which a session's pending changes are released.
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Start watching `path` for the session `(owner, id)`. An existing session with the same key
    /// is replaced: its OS watch stops and its pending changes are discarded.
    pub fn watch(
        &mut self,
        owner: &str,
        id: &str,
        path: impl AsRef<Path>,
        recursive: bool,
    ) -> notify::Result<()> {
        let root = path.as_ref().to_path_buf();
        let state = Arc::new(Mutex::new(SessionState::default()));
        let callback_state = Arc::clone(&state);
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            lock(&callback_state).absorb(result, Instant::now());
        })?;
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        watcher.watch(&root, mode)?;
        self.sessions.insert(
            (owner.to_string(), id.to_string()),
            Session {
                root,
                state,
                _watcher: watcher,
            },
        );
        Ok(())
    }

    /// Stop the session `(owner, id)`. Returns `false` when no such session exists.
    pub fn unwatch(&mut self, owner: &str, id: &str) -> bool {
        self.sessions
            .remove(&(owner.to_string(), id.to_string()))
            .is_some()
    }

    /// Drain the session `(owner, id)` when it is due: quiet for the debounce window, or pending
    /// longer than [`DEFAULT_MAX_WAIT`]. Returns the batch in a one-element vector, or an empty
    /// vector when nothing is due or the session does not exist.
    pub fn poll(&mut self, owner: &str, id: &str) -> Vec<ChangeBatch> {
        let Some(session) = self.sessions.get(&(owner.to_string(), id.to_string())) else {
            return Vec::new();
        };
        let mut st = lock(&session.state);
        if !st.has_pending() {
            // Nothing pending: no timer may keep running, or a stale one would shorten the next
            // debounce.
            st.first_pending = None;
            st.last_event = None;
            return Vec::new();
        }
        if !st.due(Instant::now(), self.debounce) {
            return Vec::new();
        }
        let mut batch = st.coalescer.drain();
        if std::mem::take(&mut st.rescan) {
            push_sorted_unique(&mut batch.changed, session.root.clone());
        }
        st.first_pending = None;
        st.last_event = None;
        if batch.is_empty() {
            Vec::new()
        } else {
            vec![batch]
        }
    }

    /// The watched root of the session `(owner, id)`, if it exists.
    pub fn root_of(&self, owner: &str, id: &str) -> Option<&Path> {
        self.sessions
            .get(&(owner.to_string(), id.to_string()))
            .map(|s| s.root.as_path())
    }

    /// Stop every session the owner holds. Returns how many were stopped.
    pub fn release_owner(&mut self, owner: &str) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|(o, _), _| o != owner);
        before - self.sessions.len()
    }

    /// Number of live sessions across all owners.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no sessions are live.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

fn lock(state: &Mutex<SessionState>) -> MutexGuard<'_, SessionState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn push_sorted_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if let Err(at) = paths.binary_search(&path) {
        paths.insert(at, path);
    }
}
