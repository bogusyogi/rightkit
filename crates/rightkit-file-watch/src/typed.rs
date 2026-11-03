//! Recursive typed streams, with backend errors and rescan notices preserved.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvError, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::native::{self, BoundedQueue, TypedOptions, WatchCursor};
use crate::WatchEventRecord;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// A watch startup failure or a gap in a live stream, independent of `notify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchError {
    Backend {
        message: String,
        paths: Vec<PathBuf>,
    },
    /// Events were dropped; reconcile a filesystem snapshot before claiming freshness.
    Overflow,
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend { message, .. } => f.write_str(message),
            Self::Overflow => f.write_str("filesystem events were dropped; rescan required"),
        }
    }
}

impl std::error::Error for WatchError {}

impl From<notify::Error> for WatchError {
    fn from(error: notify::Error) -> Self {
        Self::Backend {
            message: error.to_string(),
            paths: error.paths,
        }
    }
}

/// Filesystem hints. Backends may duplicate or coalesce hints; snapshots remain truth.
/// Paths use the backend's spelling; the registered root is canonicalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    Create(PathBuf),
    Modify(PathBuf),
    Delete(PathBuf),
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Error(WatchError),
    /// Backend overflow/dropped-event/rescan signal, even when it has no paths.
    Overflow,
}

/// Owns a recursive OS watch and a typed event stream. No async runtime required.
/// The queue is unbounded: callers should drain it regularly. Dropping this handle
/// stops the OS watch and joins its worker. Half-renames wait at most 250 ms for
/// a mate, then become Delete/Create. Tracker IDs take precedence; untracked
/// halves pair only when adjacent, as on ReadDirectoryChangesW.
pub struct TypedWatcher {
    /// Legacy backend guard, dropped before joining its translator.
    watcher: Option<LegacyBackend>,
    thread: Option<thread::JoinHandle<()>>,
    source: Source,
    /// Platform backend started by [`TypedWatcher::watch_with`]; dropped before the queue closes.
    native: Option<native::Handle>,
}

/// Where received records come from: the legacy unbounded channel, or the bounded queue of
/// `watch_with` (which also carries the resume cursor).
enum Source {
    Legacy {
        rx: mpsc::Receiver<WatchEventRecord>,
        counters: Arc<Counters>,
    },
    Bounded {
        queue: Arc<BoundedQueue>,
        /// Volume the cursor belongs to; `None` when the backend has no replay.
        volume: Option<[u8; 16]>,
    },
}

// A handle may move to another thread (apps keep it in shared state).
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<TypedWatcher>();
};

/// Cumulative counters for one typed stream, read with [`TypedWatcher::metrics`].
/// `queue_depth` is the unread backlog. The queue of the legacy constructors is unbounded, so a
/// host that stops draining it grows memory by that many records; a [`TypedWatcher::watch_with`]
/// queue is bounded (`queue_capacity`) and counts what it discarded in `dropped_total`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WatchMetrics {
    /// Records queued for the caller since the watcher started (events, errors, overflow).
    pub queued_total: u64,
    /// Records the caller has taken off the queue through any receive method.
    pub received_total: u64,
    /// `queued_total - received_total`: records waiting to be received.
    pub queue_depth: u64,
    /// Overflow / dropped-event / rescan notices queued.
    pub overflow_total: u64,
    /// Backend error records queued.
    pub error_total: u64,
    /// Records a bounded queue discarded because it was full (each gap also queues one
    /// `Overflow`). Always 0 for the legacy constructors.
    pub dropped_total: u64,
    /// Records merged into an identical unread tail record instead of being queued (directory
    /// granularity). Always 0 for the legacy constructors.
    pub coalesced_total: u64,
    /// Most records the queue holds before it clears; 0 means unbounded.
    pub queue_capacity: u64,
}

/// Runtime figures of one typed stream, computed on call by [`TypedWatcher::status`].
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct WatchStatus {
    /// Cumulative counters and the unread backlog (`queue_depth`).
    pub metrics: WatchMetrics,
    /// Resident memory, CPU time and I/O counters of the whole process (the watcher runs in it),
    /// or `None` when the host cannot report them. See `rightkit_child_process::ProcessUsage`.
    pub process: Option<rightkit_child_process::ProcessUsage>,
}

#[derive(Debug, Default)]
struct Counters {
    queued: AtomicU64,
    received: AtomicU64,
    overflow: AtomicU64,
    error: AtomicU64,
}

impl TypedWatcher {
    /// Starts a recursive, typed event stream without an async runtime (and without the `async`
    /// feature). Backend creation and root registration failures are returned to the caller.
    /// Keep the returned watcher alive; dropping it stops the backend and worker.
    pub fn watch(path: impl AsRef<Path>) -> Result<Self, WatchError> {
        Self::start(path.as_ref())
    }

    pub(crate) fn start(path: &Path) -> Result<Self, WatchError> {
        let root = path.canonicalize().map_err(|error| WatchError::Backend {
            message: error.to_string(),
            paths: vec![path.to_path_buf()],
        })?;
        let (raw_tx, raw_rx) = mpsc::channel();
        let (watcher, watched_file) = arm_legacy(&root, raw_tx)?;
        let (tx, rx) = mpsc::channel();
        let counters = Arc::new(Counters::default());
        let worker = counters.clone();
        let thread = thread::Builder::new()
            .name("rightkit-watch-typed".into())
            .spawn(move || {
                run_translated(raw_rx, watched_file, |record| {
                    // Count before sending so the caller never sees received > queued.
                    worker.queued.fetch_add(1, Ordering::Relaxed);
                    match record.event {
                        WatchEvent::Overflow => {
                            worker.overflow.fetch_add(1, Ordering::Relaxed);
                        }
                        WatchEvent::Error(_) => {
                            worker.error.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    tx.send(record).is_ok()
                })
            })
            .map_err(|error| WatchError::Backend {
                message: error.to_string(),
                paths: vec![root],
            })?;
        Ok(Self {
            watcher: Some(watcher),
            thread: Some(thread),
            source: Source::Legacy { rx, counters },
            native: None,
        })
    }

    /// Starts a recursive stream with the opt-in features of [`TypedOptions`]: a bounded queue,
    /// path exclusions, directory granularity and, on macOS, a native FSEvents backend that can
    /// resume from a [`WatchCursor`]. Both constructors use the same event translator.
    ///
    /// `WatchBackend::Native` on a host without a native backend is an error, never a fallback.
    /// Keep the returned watcher alive; dropping it stops the backend.
    pub fn watch_with(path: impl AsRef<Path>, options: TypedOptions) -> Result<Self, WatchError> {
        let started = native::start(path.as_ref(), options)?;
        Ok(Self {
            watcher: started
                .watcher
                .map(|watcher| LegacyBackend::Notify { _watcher: watcher }),
            thread: started.thread,
            source: Source::Bounded {
                queue: started.queue,
                volume: started.volume,
            },
            native: started.native,
        })
    }

    /// Resume position covering every record this handle has returned from any `recv*` call.
    /// `None` unless the backend can replay (macOS native).
    ///
    /// Persist `cursor().to_bytes()` only after applying everything received so far; delivery
    /// after a resume is at-least-once. A queue-overflow notice carries the highest id it
    /// dropped, so the cursor read after receiving it and rescanning is safe to persist. When one
    /// FSEvents event yields several records, only the last carries its id.
    pub fn cursor(&self) -> Option<WatchCursor> {
        match &self.source {
            Source::Bounded {
                queue,
                volume: Some(volume),
            } => queue.last_id().map(|id| WatchCursor::new(*volume, id)),
            _ => None,
        }
    }

    /// The position "now" for the volume holding `path`, for scan-then-watch: take it, scan, then
    /// `watch_with(replay_from(cursor))`. `None` where no backend can replay.
    pub fn current_cursor(path: impl AsRef<Path>) -> Option<WatchCursor> {
        native::current_cursor(path.as_ref())
    }

    /// True while a replayed history is still being delivered (until FSEvents' history-done
    /// marker). Always false without `replay_from`.
    pub fn is_replaying(&self) -> bool {
        match &self.source {
            Source::Bounded { queue, .. } => queue.is_replaying(),
            Source::Legacy { .. } => false,
        }
    }

    /// Number of translator threads this handle owns: 1 for notify, 0 for a native backend,
    /// which translates inline. Test support.
    #[doc(hidden)]
    pub fn translator_threads(&self) -> usize {
        usize::from(self.thread.is_some())
    }

    /// Snapshot of this stream's cumulative counters and unread backlog.
    pub fn metrics(&self) -> WatchMetrics {
        match &self.source {
            Source::Legacy { counters, .. } => {
                let queued = counters.queued.load(Ordering::Relaxed);
                let received = counters.received.load(Ordering::Relaxed);
                WatchMetrics {
                    queued_total: queued,
                    received_total: received,
                    queue_depth: queued.saturating_sub(received),
                    overflow_total: counters.overflow.load(Ordering::Relaxed),
                    error_total: counters.error.load(Ordering::Relaxed),
                    dropped_total: 0,
                    coalesced_total: 0,
                    queue_capacity: 0,
                }
            }
            Source::Bounded { queue, .. } => queue.metrics(),
        }
    }

    /// Backlog, counters and process resource usage, read now. No polling thread runs for this.
    pub fn status(&self) -> WatchStatus {
        WatchStatus {
            metrics: self.metrics(),
            process: rightkit_child_process::process_usage().ok(),
        }
    }

    fn taken<E>(&self, result: Result<WatchEventRecord, E>) -> Result<WatchEventRecord, E> {
        if let (Ok(_), Source::Legacy { counters, .. }) = (&result, &self.source) {
            counters.received.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn next(&self) -> Result<WatchEventRecord, RecvError> {
        match &self.source {
            Source::Legacy { rx, .. } => self.taken(rx.recv()),
            Source::Bounded { queue, .. } => queue.recv().map(|entry| entry.record),
        }
    }

    fn next_now(&self) -> Result<WatchEventRecord, TryRecvError> {
        match &self.source {
            Source::Legacy { rx, .. } => self.taken(rx.try_recv()),
            Source::Bounded { queue, .. } => queue.try_recv().map(|entry| entry.record),
        }
    }

    fn next_within(&self, timeout: Duration) -> Result<WatchEventRecord, RecvTimeoutError> {
        match &self.source {
            Source::Legacy { rx, .. } => self.taken(rx.recv_timeout(timeout)),
            Source::Bounded { queue, .. } => queue.recv_timeout(timeout).map(|entry| entry.record),
        }
    }

    /// Wait for a typed event. Backend failures arrive as events, not receive errors.
    pub fn recv(&self) -> Result<WatchEvent, RecvError> {
        self.next().map(|record| record.event)
    }

    /// Poll without blocking; Empty means there are no events ready yet.
    pub fn try_recv(&self) -> Result<WatchEvent, TryRecvError> {
        self.next_now().map(|record| record.event)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<WatchEvent, RecvTimeoutError> {
        self.next_within(timeout).map(|record| record.event)
    }

    /// Receive typed output with its original backend kind. Mixed typed/record
    /// receive calls consume the same queue, with no duplicate delivery.
    pub fn recv_record(&self) -> Result<WatchEventRecord, RecvError> {
        self.next()
    }

    pub fn try_recv_record(&self) -> Result<WatchEventRecord, TryRecvError> {
        self.next_now()
    }

    pub fn recv_record_timeout(
        &self,
        timeout: Duration,
    ) -> Result<WatchEventRecord, RecvTimeoutError> {
        self.next_within(timeout)
    }
}

impl Drop for TypedWatcher {
    fn drop(&mut self) {
        if let Source::Bounded { queue, .. } = &self.source {
            // Tell the backend nobody is listening, then stop it (its own Drop waits out any
            // callback in flight).
            queue.close_receiver();
        }
        self.native.take();
        self.watcher.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Keeps each concrete backend's ownership and auto-traits; dropping it disarms its watch.
enum LegacyBackend {
    Notify {
        _watcher: RecommendedWatcher,
    },
    #[cfg(windows)]
    Native {
        _watcher: native::windows::DirWatcher,
    },
}

/// Owns the legacy unbounded stream's backend. Windows uses the event-driven reader already
/// shared by directory watches; other hosts and native arm failures keep notify. A file watch
/// follows notify's Windows contract: watch its parent non-recursively and filter exact paths.
fn arm_legacy(
    root: &Path,
    tx: mpsc::Sender<notify::Result<Event>>,
) -> Result<(LegacyBackend, Option<PathBuf>), WatchError> {
    #[cfg(windows)]
    {
        let file = root.is_file().then(|| root.to_path_buf());
        let directory = if file.is_some() {
            root.parent().unwrap_or(root)
        } else {
            root
        };
        match native::windows::DirWatcher::start(directory, file.is_none(), tx.clone()) {
            Ok(watcher) => return Ok((LegacyBackend::Native { _watcher: watcher }, file)),
            Err(error) => {
                tracing::debug!(%error, path = %root.display(), "native typed watch unavailable; using notify");
            }
        }
    }
    let mut watcher = notify::recommended_watcher(move |result| {
        let _ = tx.send(result);
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok((LegacyBackend::Notify { _watcher: watcher }, None))
}

const RENAME_WAIT: Duration = Duration::from_millis(250);

struct HalfRename {
    path: PathBuf,
    from: bool,
    deadline: Instant,
    raw_kind: EventKind,
}

impl HalfRename {
    fn degrade(self) -> WatchEventRecord {
        let event = if self.from {
            WatchEvent::Delete(self.path)
        } else {
            WatchEvent::Create(self.path)
        };
        WatchEventRecord {
            event,
            raw_kind: Some(self.raw_kind),
        }
    }
}

#[derive(Default)]
pub(crate) struct Translator {
    halves: HashMap<Option<usize>, HalfRename>,
    // inotify emits From, To, then Both with the same tracker. Suppress that echo.
    paired: HashMap<usize, (PathBuf, PathBuf, Instant)>,
}

impl Translator {
    pub(crate) fn expire_records(&mut self, now: Instant) -> Vec<WatchEventRecord> {
        self.paired.retain(|_, (_, _, deadline)| *deadline > now);
        let keys: Vec<_> = self
            .halves
            .iter()
            .filter(|(_, half)| half.deadline <= now)
            .map(|(key, _)| *key)
            .collect();
        keys.into_iter()
            .filter_map(|key| self.halves.remove(&key))
            .map(HalfRename::degrade)
            .collect()
    }

    pub(crate) fn flush_records(&mut self) -> Vec<WatchEventRecord> {
        self.paired.clear();
        self.halves
            .drain()
            .map(|(_, half)| half.degrade())
            .collect()
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.halves.values().map(|half| half.deadline).min()
    }

    // Pure seam: injected time and backend results, no filesystem or sleeping.

    pub(crate) fn translate_records(
        &mut self,
        result: notify::Result<Event>,
        now: Instant,
    ) -> Vec<WatchEventRecord> {
        let mut out = self.expire_records(now);
        let event = match result {
            Ok(event) => event,
            Err(error) => {
                out.extend(self.flush_records());
                out.push(WatchEventRecord {
                    event: WatchEvent::Error(error.into()),
                    raw_kind: None,
                });
                return out;
            }
        };
        if event.need_rescan() {
            out.extend(self.flush_records());
            out.push(WatchEventRecord {
                event: WatchEvent::Overflow,
                raw_kind: Some(event.kind),
            });
        }
        let event_kind = event.kind;
        let record = |event| WatchEventRecord {
            event,
            raw_kind: Some(event_kind),
        };
        let tracker = event.tracker();
        // Without a correlation ID, only adjacent complementary halves can pair.
        let complementary = matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(RenameMode::From | RenameMode::To))
        ) && tracker.is_none()
            && event.paths.len() == 1;
        let completes_untracked = tracker.is_none()
            && matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            )
            && event.paths.len() == 2;
        if !complementary && !completes_untracked {
            if let Some(half) = self.halves.remove(&None) {
                out.push(half.degrade());
            }
        }
        match event.kind {
            EventKind::Modify(ModifyKind::Name(RenameMode::From | RenameMode::To)) => {
                let from = matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Name(RenameMode::From))
                );
                for path in event.paths {
                    if let Some(old) = self.halves.remove(&tracker) {
                        if old.from != from {
                            let (from, to) = if from {
                                (path, old.path)
                            } else {
                                (old.path, path)
                            };
                            if let Some(tracker) = tracker {
                                self.paired
                                    .insert(tracker, (from.clone(), to.clone(), now + RENAME_WAIT));
                            }
                            out.push(record(WatchEvent::Rename { from, to }));
                            continue;
                        }
                        out.push(old.degrade());
                    }
                    self.halves.insert(
                        tracker,
                        HalfRename {
                            path,
                            from,
                            deadline: now + RENAME_WAIT,
                            raw_kind: event_kind,
                        },
                    );
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                if let [from, to] = event.paths.as_slice() {
                    if let Some(half) = self.halves.remove(&tracker) {
                        let endpoint = if half.from { from } else { to };
                        if half.path != *endpoint {
                            out.push(half.degrade());
                        }
                    }
                    let echo = tracker
                        .and_then(|id| self.paired.get(&id))
                        .is_some_and(|(old_from, old_to, _)| old_from == from && old_to == to);
                    if !echo {
                        out.push(record(WatchEvent::Rename {
                            from: from.clone(),
                            to: to.clone(),
                        }));
                    }
                } else {
                    // Malformed/unpaired "Both" still provides conservative path hints.
                    out.extend(
                        event
                            .paths
                            .into_iter()
                            .map(|path| record(WatchEvent::Modify(path))),
                    );
                }
            }
            EventKind::Create(_) => out.extend(
                event
                    .paths
                    .into_iter()
                    .map(|path| record(WatchEvent::Create(path))),
            ),
            EventKind::Remove(_) => out.extend(
                event
                    .paths
                    .into_iter()
                    .map(|path| record(WatchEvent::Delete(path))),
            ),
            EventKind::Modify(_) | EventKind::Any => {
                out.extend(
                    event
                        .paths
                        .into_iter()
                        .map(|path| record(WatchEvent::Modify(path))),
                );
            }
            EventKind::Access(_) | EventKind::Other => {}
        }
        out
    }
}

fn run_translated(
    rx: mpsc::Receiver<notify::Result<Event>>,
    watched_file: Option<PathBuf>,
    mut send: impl FnMut(WatchEventRecord) -> bool,
) {
    let mut translator = Translator::default();
    loop {
        let result = match translator.next_deadline() {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let events = match result {
            Ok(mut result) => {
                let relevant = if let Some(file) = &watched_file {
                    match &mut result {
                        Ok(event) => {
                            // Filter before pairing rename halves, as notify does for file watches.
                            // A pathless overflow still invalidates this file's watch history.
                            event.paths.retain(|path| path == file);
                            !event.paths.is_empty() || event.need_rescan()
                        }
                        Err(error) => {
                            error.paths = vec![file.clone()];
                            true
                        }
                    }
                } else {
                    true
                };
                if relevant {
                    translator.translate_records(result, Instant::now())
                } else {
                    // Sibling traffic must not postpone expiry of this file's pending rename.
                    translator.expire_records(Instant::now())
                }
            }
            Err(RecvTimeoutError::Timeout) => translator.expire_records(Instant::now()),
            Err(RecvTimeoutError::Disconnected) => {
                for event in translator.flush_records() {
                    if !send(event) {
                        return;
                    }
                }
                return;
            }
        };
        for event in events {
            if !send(event) {
                return;
            }
        }
    }
}
