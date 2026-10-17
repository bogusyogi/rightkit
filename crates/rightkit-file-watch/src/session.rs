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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_sessions_poll_empty_and_unwatch_false() {
        let mut manager = WatchSessionManager::new();
        assert!(manager.poll("client", "missing").is_empty());
        assert!(!manager.unwatch("client", "missing"));
        assert!(manager.is_empty());
        assert_eq!(manager.release_owner("client"), 0);
    }

    #[test]
    fn watch_is_keyed_by_owner_and_id_and_release_is_per_owner() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut manager = WatchSessionManager::new();
        manager.watch("a", "1", &root, true).unwrap();
        manager.watch("a", "2", &root, false).unwrap();
        manager.watch("b", "1", &root, true).unwrap();
        assert_eq!(manager.len(), 3);
        assert_eq!(manager.root_of("b", "1"), Some(root.as_path()));
        assert_eq!(manager.release_owner("a"), 2);
        assert_eq!(manager.len(), 1);
        assert!(manager.root_of("a", "1").is_none());
        assert!(manager.unwatch("b", "1"));
        assert!(manager.is_empty());
    }

    fn raw(kind: EventKind) -> notify::Result<Event> {
        Ok(Event::new(kind).add_path(PathBuf::from("/watched/file.txt")))
    }

    #[test]
    fn ignored_access_event_starts_no_timer() {
        use notify::event::AccessKind;
        let mut st = SessionState::default();
        st.absorb(raw(EventKind::Access(AccessKind::Read)), Instant::now());
        assert!(!st.has_pending());
        assert!(st.first_pending.is_none());
        assert!(st.last_event.is_none());
    }

    #[test]
    fn ignored_access_event_does_not_shorten_the_debounce_of_the_first_real_change() {
        use notify::event::{AccessKind, ModifyKind};
        let debounce = Duration::from_millis(100);
        let t0 = Instant::now();
        let mut st = SessionState::default();
        st.absorb(raw(EventKind::Access(AccessKind::Read)), t0);
        // The real change arrives after the max wait has passed since the access event.
        let t1 = t0 + DEFAULT_MAX_WAIT + Duration::from_millis(10);
        st.absorb(raw(EventKind::Modify(ModifyKind::Any)), t1);
        // The clock starts at the modification: not quiet and not overdue yet.
        assert!(!st.due(t1, debounce));
        assert!(!st.due(t1 + debounce - Duration::from_millis(1), debounce));
        assert!(st.due(t1 + debounce, debounce));
    }

    #[test]
    fn created_file_is_reported_after_quiet_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut manager = WatchSessionManager::new().with_debounce(Duration::from_millis(50));
        manager.watch("client", "w", &root, true).unwrap();
        let file = root.join("new.txt");
        std::fs::write(&file, "x").unwrap();
        let mut batches = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while batches.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
            batches = manager.poll("client", "w");
        }
        assert!(batches.iter().any(|b| b.changed.contains(&file)));
    }
}
