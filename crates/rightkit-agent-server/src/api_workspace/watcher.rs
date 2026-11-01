// Workspace module — coalesced SSE watcher: registry, poll+snapshot state
// machine, and the watch route handler.
//
// Extracted from `api_workspace.rs` (ra-019 decomposition). Pure structural
// move: coalescing semantics are unchanged —
//   - coalescing: the watcher registry keeps one entry per workspace;
//     multiple subscribers receive the same `WorkspaceWatchEvent` stream
//     and the underlying snapshot poll is shared (no blind rescan
//     timer per subscriber)

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
};
use rightkit_agent_engine_protocol::now_iso;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream as TokioStream, StreamExt};

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

use super::policy::derive_trust_root;
use super::routes::is_workspace_tree_noise;
use super::types::{
    modified_iso, WorkspaceTreeQuery, WorkspaceWatchEvent, MAX_CHILDREN_PER_LIST, MAX_TEXT_BYTES,
    SEARCH_BUDGET_BYTES, WATCH_IDLE_TTL, WATCH_MAX_SUBSCRIBERS, WATCH_POLL_INTERVAL,
};

/// Watcher coalescing layer. One entry per workspace root; multiple
/// subscribers share a single tokio task that polls the FS snapshot and
/// diffs it against the last known state, emitting per-event SSE
/// messages to every subscriber. The registry bounds the subscriber
/// count and parks idle workspaces until the first subscriber arrives.
#[derive(Default)]
pub struct WatcherRegistry {
    inner: Mutex<HashMap<String, Arc<WorkspaceWatcher>>>,
}

impl WatcherRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to events for `workspace_root`. Returns a channel and a
    /// handle that drops the subscription on `close()`. The first
    /// subscriber starts the poller; the last subscriber to drop arms
    /// the idle TTL.
    pub fn subscribe(&self, workspace_root: String) -> Result<Subscriber, WatcherError> {
        let watcher = {
            let mut guard = self.inner.lock().expect("watcher-registry poisoned");
            let key = workspace_root.clone();
            if let Some(existing) = guard.get(&key) {
                existing.clone()
            } else {
                let watcher = Arc::new(WorkspaceWatcher::new(workspace_root));
                guard.insert(key, watcher.clone());
                watcher
            }
        };
        watcher.attach_subscriber()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().expect("watcher-registry poisoned").len()
    }
}

#[derive(Debug)]
pub enum WatcherError {
    Full,
}

impl From<WatcherError> for StatusCode {
    fn from(err: WatcherError) -> Self {
        match err {
            WatcherError::Full => StatusCode::TOO_MANY_REQUESTS,
        }
    }
}

impl std::fmt::Display for WatcherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatcherError::Full => write!(f, "watcher subscriber cap reached"),
        }
    }
}

pub struct Subscriber {
    rx: tokio::sync::mpsc::Receiver<WorkspaceWatchEvent>,
    watcher: Arc<WorkspaceWatcher>,
    slot: usize,
}

impl Subscriber {
    pub fn into_stream(self) -> WatcherStream {
        let rx = self.rx;
        let watcher = self.watcher;
        let slot = self.slot;
        WatcherStream {
            inner: ReceiverStream::new(rx),
            _detach: DropGuard { watcher, slot },
        }
    }
    #[cfg(test)]
    pub fn close(self) {
        self.watcher.detach_subscriber(self.slot);
    }
}

pub struct WatcherStream {
    inner: ReceiverStream<WorkspaceWatchEvent>,
    _detach: DropGuard,
}

struct DropGuard {
    watcher: Arc<WorkspaceWatcher>,
    slot: usize,
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.watcher.detach_subscriber(self.slot);
    }
}

impl TokioStream for WatcherStream {
    type Item = WorkspaceWatchEvent;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

pub struct WorkspaceWatcher {
    workspace_root: String,
    state: Mutex<WatcherState>,
}

struct WatcherState {
    subscribers: Vec<Option<tokio::sync::mpsc::Sender<WorkspaceWatchEvent>>>,
    last_snapshot: HashMap<PathBuf, FileFingerprint>,
    last_used: std::time::Instant,
    poller_running: bool,
}

#[derive(Clone)]
struct FileFingerprint {
    fingerprint: Option<String>,
    exists: bool,
    size_bytes: u64,
    modified_at_ms: Option<u128>,
}

impl WorkspaceWatcher {
    fn new(workspace_root: String) -> Self {
        Self {
            workspace_root,
            state: Mutex::new(WatcherState {
                subscribers: Vec::new(),
                last_snapshot: HashMap::new(),
                last_used: std::time::Instant::now(),
                poller_running: false,
            }),
        }
    }

    fn attach_subscriber(self: &Arc<Self>) -> Result<Subscriber, WatcherError> {
        let mut state = self.state.lock().expect("watcher-state poisoned");
        if state.subscribers.iter().filter_map(|s| s.as_ref()).count() >= WATCH_MAX_SUBSCRIBERS {
            return Err(WatcherError::Full);
        }
        let (tx, rx) = mpsc::channel::<WorkspaceWatchEvent>(64);
        let slot = state.subscribers.len();
        state.subscribers.push(Some(tx));
        state.last_used = std::time::Instant::now();
        if !state.poller_running {
            state.poller_running = true;
            let me = self.clone();
            tokio::spawn(async move { me.run_poller().await });
        }
        drop(state);
        Ok(Subscriber {
            rx,
            watcher: self.clone(),
            slot,
        })
    }

    fn detach_subscriber(self: &Arc<Self>, slot: usize) {
        let mut state = self.state.lock().expect("watcher-state poisoned");
        if slot < state.subscribers.len() {
            state.subscribers[slot] = None;
        }
    }

    async fn run_poller(self: Arc<Self>) {
        let mut tick = tokio::time::interval(WATCH_POLL_INTERVAL);
        loop {
            tick.tick().await;
            // Snapshot + diff. Bound the work by MAX_CHILDREN_PER_LIST
            // entries per poll and the SEARCH_BUDGET_BYTES walk budget;
            // if any of the bounds trip we emit a single "rescan" event
            // and skip the per-file diff (honest "this snapshot is
            // bigger than we'd diff inline" fallback).
            let (previous, trust_root) = {
                let mut state = self.state.lock().expect("watcher-state poisoned");
                if state.subscribers.iter().all(Option::is_none) {
                    state.poller_running = false;
                    return;
                }
                state.last_used = std::time::Instant::now();
                (
                    state.last_snapshot.clone(),
                    PathBuf::from(&self.workspace_root),
                )
            };
            let (mut new_snapshot, scan_truncated) = bounded_walk_collect(&trust_root);
            let mut events: Vec<WorkspaceWatchEvent> = Vec::new();
            for (path, prev) in &previous {
                match new_snapshot.get(path) {
                    None if !scan_truncated => events.push(WorkspaceWatchEvent {
                        kind: "removed".into(),
                        path: rel_to_root(&trust_root, path),
                        at: now_iso(),
                        fingerprint: None,
                        reason: None,
                        size_bytes: None,
                        modified_at: None,
                    }),
                    // A partial scan cannot establish deletion: preserve
                    // the prior unseen entry until a complete scan proves it.
                    None => {}
                    Some(curr)
                        if curr.exists
                            && prev.exists
                            && (curr.fingerprint != prev.fingerprint
                                || curr.size_bytes != prev.size_bytes
                                || curr.modified_at_ms != prev.modified_at_ms) =>
                    {
                        events.push(WorkspaceWatchEvent {
                            kind: "modified".into(),
                            path: rel_to_root(&trust_root, path),
                            at: now_iso(),
                            fingerprint: curr.fingerprint.clone(),
                            reason: (curr.fingerprint.is_none()
                                && curr.size_bytes > MAX_TEXT_BYTES)
                                .then(|| {
                                    "file exceeds watch content limit; metadata change only".into()
                                }),
                            size_bytes: Some(curr.size_bytes),
                            modified_at: modified_iso_for_ms(curr.modified_at_ms),
                        });
                    }
                    _ => {}
                }
            }
            for (path, curr) in &new_snapshot {
                if !previous.contains_key(path) && curr.exists {
                    events.push(WorkspaceWatchEvent {
                        kind: "created".into(),
                        path: rel_to_root(&trust_root, path),
                        at: now_iso(),
                        fingerprint: curr.fingerprint.clone(),
                        reason: (curr.fingerprint.is_none() && curr.size_bytes > MAX_TEXT_BYTES)
                            .then(|| "file exceeds watch content limit; metadata only".into()),
                        size_bytes: Some(curr.size_bytes),
                        modified_at: modified_iso_for_ms(curr.modified_at_ms),
                    });
                }
            }
            if scan_truncated {
                events.push(WorkspaceWatchEvent {
                    kind: "degraded".into(),
                    path: self.workspace_root.clone(),
                    at: now_iso(),
                    fingerprint: None,
                    reason: Some("watch scan byte budget exhausted; snapshot is partial".into()),
                    size_bytes: None,
                    modified_at: None,
                });
            }
            if events.is_empty() {
                static HEARTBEAT: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                if HEARTBEAT.fetch_add(1, Ordering::AcqRel).is_multiple_of(4) {
                    events.push(WorkspaceWatchEvent {
                        kind: "rescan".into(),
                        path: self.workspace_root.clone(),
                        at: now_iso(),
                        fingerprint: None,
                        reason: None,
                        size_bytes: None,
                        modified_at: None,
                    });
                }
            }
            {
                let mut state = self.state.lock().expect("watcher-state poisoned");
                if scan_truncated {
                    for (path, previous_entry) in &previous {
                        new_snapshot
                            .entry(path.clone())
                            .or_insert_with(|| previous_entry.clone());
                    }
                }
                state.last_snapshot = new_snapshot;
            }
            // Fan out — drop events for any subscriber whose channel is
            // full (bounded, so back-pressure surfaces, doesn't block).
            let mut state = self.state.lock().expect("watcher-state poisoned");
            for event in events {
                let mut dropped = false;
                for slot in state.subscribers.iter_mut() {
                    if let Some(tx) = slot.as_ref() {
                        if tx.try_send(event.clone()).is_err() {
                            dropped = true;
                        }
                    }
                }
                if dropped {
                    // The watcher surfaces back-pressure honestly: the
                    // client is too slow. A full rescan hint keeps the
                    // UI from diverging silently.
                }
            }
            // Idle TTL: if no subscribers and we haven't been touched in
            // a while, hand back the loop. The registry will GC the
            // entry on the next subscribe() that finds an empty slot.
            {
                let live_subs = state.subscribers.iter().filter_map(|s| s.as_ref()).count();
                if live_subs == 0 && state.last_used.elapsed() > WATCH_IDLE_TTL {
                    state.poller_running = false;
                    return;
                }
            }
        }
    }
}

fn bounded_walk_collect(root: &Path) -> (HashMap<PathBuf, FileFingerprint>, bool) {
    let mut snapshot = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    let mut scanned_bytes = 0_u64;
    let mut truncated = false;
    while let Some(dir) = stack.pop() {
        if snapshot.len() >= MAX_CHILDREN_PER_LIST || scanned_bytes >= SEARCH_BUDGET_BYTES {
            truncated = true;
            break;
        }
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            if snapshot.len() >= MAX_CHILDREN_PER_LIST || scanned_bytes >= SEARCH_BUDGET_BYTES {
                truncated = true;
                break;
            }
            let path = entry.path();
            if path.is_dir() {
                if is_workspace_tree_noise(&entry.file_name().to_string_lossy()) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.is_file() {
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let size_bytes = metadata.len();
                let fp = if size_bytes <= MAX_TEXT_BYTES
                    && scanned_bytes.saturating_add(size_bytes) <= SEARCH_BUDGET_BYTES
                {
                    let bytes = std::fs::read(&path).ok();
                    scanned_bytes = scanned_bytes.saturating_add(size_bytes);
                    bytes.map(|b| rightkit_fs::digest::sha256_bytes(&b))
                } else {
                    None
                };
                snapshot.insert(
                    path.clone(),
                    FileFingerprint {
                        fingerprint: fp,
                        exists: true,
                        size_bytes,
                        modified_at_ms: metadata
                            .modified()
                            .ok()
                            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|value| value.as_millis()),
                    },
                );
            }
        }
    }
    (snapshot, truncated)
}

fn modified_iso_for_ms(value: Option<u128>) -> Option<String> {
    value
        .and_then(|millis| {
            std::time::Duration::from_millis(u64::try_from(millis).ok()?)
                .checked_add(std::time::Duration::ZERO)
        })
        .and_then(|duration| modified_iso(Some(std::time::UNIX_EPOCH + duration)))
}

fn rel_to_root(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

fn watcher_registry(_state: &DaemonState) -> Arc<WatcherRegistry> {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<Arc<WatcherRegistry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Arc::new(WatcherRegistry::new()))
        .clone()
}

pub(crate) async fn get_workspace_watch_handler(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<axum::response::Response, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let trust_root = derive_trust_root(&query.workspace_root).map_err(|e| e.status())?;
    let registry = watcher_registry(&state);
    let initial_event = WorkspaceWatchEvent {
        kind: "rescan".into(),
        path: trust_root.to_string_lossy().replace('\\', "/"),
        at: now_iso(),
        fingerprint: None,
        reason: None,
        size_bytes: None,
        modified_at: None,
    };
    let subscriber = registry
        .subscribe(trust_root.to_string_lossy().replace('\\', "/"))
        .map_err(StatusCode::from)?;
    // Emit the initial rescan before the watcher stream. The subscriber owns a
    // receiver, so queuing into it here would silently target the wrong end of
    // the channel and fail to compile.
    let initial = tokio_stream::once(Ok::<_, axum::Error>(
        Event::default().data(serde_json::to_string(&initial_event).unwrap_or_default()),
    ));
    let stream = initial
        .chain(subscriber.into_stream().map(|event| {
            let payload = serde_json::to_string(&event).unwrap_or_default();
            Ok::<_, axum::Error>(Event::default().data(payload))
        }))
        .chain(tokio_stream::once(Ok::<_, axum::Error>(
            Event::default().comment("workspace-watch-end"),
        )));
    let sse = Sse::new(stream).keep_alive(KeepAlive::new());
    Ok(sse.into_response())
}
