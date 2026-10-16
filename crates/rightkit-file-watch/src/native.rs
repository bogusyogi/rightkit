//! Native watch backends and the options that reach them through [`TypedWatcher::watch_with`].
//!
//! Every pre-existing constructor stays on `notify`, unchanged. `watch_with` adds, on top of the
//! same [`Translator`](crate::typed::Translator): a bounded queue, path exclusions, directory
//! granularity, and (macOS) a native FSEvents backend with a resumable [`WatchCursor`].
//!
//! # Seam for platform backends
//!
//! A platform module (`macos`, `windows`) exposes `Handle`, `start` and `current_cursor`.
//! `start` receives a [`BackendSink`]: feed it `notify::Event` values (or errors) from the
//! backend's own thread or callback with [`BackendSink::event`], optionally tagged with the
//! backend's private resume id, call [`BackendSink::commit`] once per backend batch, and
//! [`BackendSink::finish`] when the stream ends. The sink runs the `Translator`, applies
//! exclusions and granularity folding, and pushes into the bounded queue. A backend never
//! blocks the consumer and never grows memory beyond the queue capacity.
//!
//! # Windows
//!
//! `src/native/windows.rs` holds a native `ReadDirectoryChangesW` reader (`DirWatcher`) that,
//! unlike notify's Windows backend, reports a buffer overflow. `DirWatchSet` (`watch_batches`,
//! `watch_records`) uses it on Windows and falls back to notify if it cannot open the folder.
//! The typed `watch_with` path uses it too: [`WatchBackend::Native`] and [`WatchBackend::Auto`]
//! wrap a `DirWatcher` in a pump thread that feeds the [`BackendSink`], so a lost report becomes
//! `WatchEvent::Overflow`. The buffer is `windows_buffer_bytes` clamped to [4 KiB, 64 KiB]. If the
//! native open or first arm fails, `watch_with` falls back to notify on Windows.

use std::fmt;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::typed::Translator;
use crate::{WatchError, WatchEvent, WatchEventRecord};

pub(crate) mod queue;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
use windows as platform;

#[cfg(not(any(target_os = "macos", windows)))]
mod unsupported;
#[cfg(not(any(target_os = "macos", windows)))]
use unsupported as platform;

pub(crate) use platform::Handle;
pub(crate) use queue::{BoundedQueue, Entry};

/// Whether [`WatchBackend::Auto`] picks the native backend on Windows (it does; a native open
/// failure still falls back to notify).
pub(crate) const AUTO_IS_NATIVE_ON_WINDOWS: bool = true;

/// Which implementation `watch_with` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WatchBackend {
    /// Native on Windows and macOS, notify elsewhere.
    #[default]
    Auto,
    /// The platform's own API. On a host without one this is an error, never a fallback.
    Native,
    /// `notify`, with the bounded queue, exclusions and directory folding of `watch_with`.
    Notify,
}

/// How finely changes are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Granularity {
    /// One record per changed file.
    #[default]
    File,
    /// One `Modify` per changed directory.
    Directory,
}

const CURSOR_VERSION: u8 = 1;

/// Opaque resume position: the volume's FSEvents UUID and an event id. Persist it with
/// [`WatchCursor::to_bytes`] and pass it back through [`TypedOptions::replay_from`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatchCursor {
    volume: [u8; 16],
    event_id: u64,
}

impl WatchCursor {
    /// Length of the encoding: a version byte, the 16-byte volume UUID, a little-endian `u64`.
    pub const ENCODED_LEN: usize = 25;

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn new(volume: [u8; 16], event_id: u64) -> Self {
        Self { volume, event_id }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn volume(&self) -> [u8; 16] {
        self.volume
    }

    pub fn to_bytes(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0] = CURSOR_VERSION;
        out[1..17].copy_from_slice(&self.volume);
        out[17..].copy_from_slice(&self.event_id.to_le_bytes());
        out
    }

    /// Decodes [`WatchCursor::to_bytes`] output; `None` for any other length or version.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::ENCODED_LEN || bytes[0] != CURSOR_VERSION {
            return None;
        }
        let volume: [u8; 16] = bytes[1..17].try_into().ok()?;
        let event_id = u64::from_le_bytes(bytes[17..].try_into().ok()?);
        Some(Self { volume, event_id })
    }

    /// The backend event id (FSEvents). Meaningful only with the same volume.
    pub fn event_id(&self) -> u64 {
        self.event_id
    }
}

type Predicate = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// Options for [`TypedWatcher::watch_with`](crate::TypedWatcher::watch_with).
#[derive(Clone)]
pub struct TypedOptions {
    backend: WatchBackend,
    queue_capacity: NonZeroUsize,
    exclude_prefixes: Vec<PathBuf>,
    exclude_with: Vec<Predicate>,
    granularity: Granularity,
    latency: Duration,
    replay_from: Option<WatchCursor>,
    windows_buffer_bytes: u32,
    windows_network_buffer_bytes: u32,
}

impl Default for TypedOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TypedOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedOptions")
            .field("backend", &self.backend)
            .field("queue_capacity", &self.queue_capacity)
            .field("exclude_prefixes", &self.exclude_prefixes)
            .field("exclude_with", &self.exclude_with.len())
            .field("granularity", &self.granularity)
            .field("latency", &self.latency)
            .field("replay_from", &self.replay_from)
            .field("windows_buffer_bytes", &self.windows_buffer_bytes)
            .field(
                "windows_network_buffer_bytes",
                &self.windows_network_buffer_bytes,
            )
            .finish()
    }
}

const DEFAULT_QUEUE_CAPACITY: usize = 32_768;
const MIN_WINDOWS_BUFFER: u32 = 4_096;

impl TypedOptions {
    pub fn new() -> Self {
        Self {
            backend: WatchBackend::Auto,
            queue_capacity: NonZeroUsize::new(DEFAULT_QUEUE_CAPACITY).expect("non-zero"),
            exclude_prefixes: Vec::new(),
            exclude_with: Vec::new(),
            granularity: Granularity::File,
            latency: Duration::ZERO,
            replay_from: None,
            windows_buffer_bytes: 262_144,
            windows_network_buffer_bytes: 65_536,
        }
    }

    pub fn backend(mut self, backend: WatchBackend) -> Self {
        self.backend = backend;
        self
    }

    /// Most records the queue holds before it clears and reports one `Overflow` (default 32,768).
    pub fn queue_capacity(mut self, records: NonZeroUsize) -> Self {
        self.queue_capacity = records;
        self
    }

    /// Drops every record under `path`. Canonicalised like the watch root when it exists.
    pub fn exclude_prefix(mut self, path: impl Into<PathBuf>) -> Self {
        self.exclude_prefixes.push(path.into());
        self
    }

    /// Drops every record whose path the predicate accepts. It runs on the backend thread
    /// and must not block.
    pub fn exclude_with(
        mut self,
        excluded: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.exclude_with.push(Arc::new(excluded));
        self
    }

    pub fn granularity(mut self, granularity: Granularity) -> Self {
        self.granularity = granularity;
        self
    }

    /// FSEvents batching interval; ignored by other backends. Default zero.
    pub fn latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Resume from a persisted position. Backends without replay, and cursors that cannot be
    /// honoured, start live and deliver `Overflow` first.
    pub fn replay_from(mut self, cursor: WatchCursor) -> Self {
        self.replay_from = Some(cursor);
        self
    }

    /// `ReadDirectoryChangesW` buffer for local volumes (default 262,144).
    pub fn windows_buffer_bytes(mut self, bytes: u32) -> Self {
        self.windows_buffer_bytes = bytes;
        self
    }

    /// Buffer retried once when the first arm fails on a network share (default 65,536).
    pub fn windows_network_buffer_bytes(mut self, bytes: u32) -> Self {
        self.windows_network_buffer_bytes = bytes;
        self
    }

    pub(crate) fn queue_capacity_get(&self) -> usize {
        self.queue_capacity.get()
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn granularity_get(&self) -> Granularity {
        self.granularity
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn latency_get(&self) -> Duration {
        self.latency
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn replay_cursor(&self) -> Option<WatchCursor> {
        self.replay_from
    }

    /// Rounded down to a multiple of 4, at least 4,096.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn windows_buffer(&self) -> u32 {
        normalize_buffer(self.windows_buffer_bytes)
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn windows_network_buffer(&self) -> u32 {
        normalize_buffer(self.windows_network_buffer_bytes)
    }
}

fn normalize_buffer(bytes: u32) -> u32 {
    (bytes & !3).max(MIN_WINDOWS_BUFFER)
}

/// Canonical form of an exclusion path: the path itself, or its canonical parent plus the final
/// component when it does not exist yet, or the path as given.
fn canonical_exclusion(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (
        path.parent().and_then(|p| p.canonicalize().ok()),
        path.file_name(),
    ) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// Path exclusions applied after translation and before queueing.
#[derive(Clone, Default)]
pub(crate) struct Exclusions {
    prefixes: Vec<PathBuf>,
    predicates: Vec<Predicate>,
}

impl Exclusions {
    fn from_options(options: &TypedOptions) -> Self {
        Self {
            prefixes: options
                .exclude_prefixes
                .iter()
                .map(|p| canonical_exclusion(p))
                .collect(),
            predicates: options.exclude_with.clone(),
        }
    }

    fn is_empty(&self) -> bool {
        self.prefixes.is_empty() && self.predicates.is_empty()
    }

    pub(crate) fn excludes(&self, path: &Path) -> bool {
        self.prefixes.iter().any(|prefix| path.starts_with(prefix))
            || self.predicates.iter().any(|excluded| excluded(path))
    }

    /// `None` drops the record. A rename with one excluded side degrades to `Delete(from)` or
    /// `Create(to)`; errors and overflow are never excluded.
    pub(crate) fn filter(&self, record: WatchEventRecord) -> Option<WatchEventRecord> {
        if self.is_empty() {
            return Some(record);
        }
        let WatchEventRecord { event, raw_kind } = record;
        let event = match event {
            WatchEvent::Create(p) => (!self.excludes(&p)).then_some(WatchEvent::Create(p))?,
            WatchEvent::Modify(p) => (!self.excludes(&p)).then_some(WatchEvent::Modify(p))?,
            WatchEvent::Delete(p) => (!self.excludes(&p)).then_some(WatchEvent::Delete(p))?,
            WatchEvent::Rename { from, to } => match (self.excludes(&from), self.excludes(&to)) {
                (false, false) => WatchEvent::Rename { from, to },
                (true, false) => WatchEvent::Create(to),
                (false, true) => WatchEvent::Delete(from),
                (true, true) => return None,
            },
            gap @ (WatchEvent::Error(_) | WatchEvent::Overflow) => gap,
        };
        Some(WatchEventRecord { event, raw_kind })
    }
}

/// Folds a record to `Modify(parent)` for directory granularity, keeping its raw kind.
/// A rename touches two directories, or one when they are the same.
pub(crate) fn fold_to_directory(record: WatchEventRecord) -> Vec<WatchEventRecord> {
    fn parent(path: PathBuf) -> PathBuf {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or(path)
    }
    let WatchEventRecord { event, raw_kind } = record;
    let modify = |p: PathBuf| WatchEventRecord {
        event: WatchEvent::Modify(parent(p)),
        raw_kind,
    };
    match event {
        WatchEvent::Create(p) | WatchEvent::Modify(p) | WatchEvent::Delete(p) => vec![modify(p)],
        WatchEvent::Rename { from, to } => {
            let (a, b) = (modify(from), modify(to));
            if a.event == b.event {
                vec![a]
            } else {
                vec![a, b]
            }
        }
        gap @ (WatchEvent::Error(_) | WatchEvent::Overflow) => vec![WatchEventRecord {
            event: gap,
            raw_kind,
        }],
    }
}

/// What a backend feeds: runs the `Translator` inline, applies exclusions and granularity, and
/// queues the result. Single-threaded by construction: each backend owns one sink.
pub(crate) struct BackendSink {
    translator: Translator,
    exclusions: Exclusions,
    fold_dirs: bool,
    coalesce_tail: bool,
    queue: Arc<BoundedQueue>,
    batch: Vec<Entry>,
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
impl BackendSink {
    fn new(queue: Arc<BoundedQueue>, options: &TypedOptions, fold_dirs: bool) -> Self {
        Self {
            translator: Translator::default(),
            exclusions: Exclusions::from_options(options),
            fold_dirs,
            coalesce_tail: options.granularity == Granularity::Directory,
            queue,
            batch: Vec::new(),
        }
    }

    /// Translates one backend result into the pending batch. `id` is the backend's resume id for
    /// this event; it rides on the last record the event produces.
    pub(crate) fn event(&mut self, result: notify::Result<Event>, id: Option<u64>) {
        let records = self.translator.translate_records(result, Instant::now());
        self.buffer(records, id);
    }

    /// Emits rename halves whose mate never arrived, if their deadline passed.
    pub(crate) fn expire(&mut self) {
        let records = self.translator.expire_records(Instant::now());
        self.buffer(records, None);
    }

    /// Degrades every pending rename half now (no timer is available to expire them later).
    pub(crate) fn flush(&mut self, id: Option<u64>) {
        let records = self.translator.flush_records();
        self.buffer(records, id);
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.translator.next_deadline()
    }

    fn buffer(&mut self, records: Vec<WatchEventRecord>, id: Option<u64>) {
        let mut kept: Vec<WatchEventRecord> = Vec::with_capacity(records.len());
        for record in records {
            let Some(record) = self.exclusions.filter(record) else {
                continue;
            };
            if self.fold_dirs {
                for folded in fold_to_directory(record) {
                    // Adjacent duplicates inside one event carry no extra information.
                    if kept.last() != Some(&folded) {
                        kept.push(folded);
                    }
                }
            } else {
                kept.push(record);
            }
        }
        let last = kept.len().saturating_sub(1);
        self.batch.extend(
            kept.into_iter()
                .enumerate()
                .map(|(i, record)| Entry::new(record, if i == last { id } else { None })),
        );
    }

    /// Pushes the pending batch atomically. False once the consumer has dropped its handle.
    pub(crate) fn commit(&mut self) -> bool {
        let batch = std::mem::take(&mut self.batch);
        self.queue.push_all(batch, self.coalesce_tail)
    }

    /// The backend stream ended: flush, commit and let the consumer drain, then disconnect.
    pub(crate) fn finish(&mut self) {
        self.flush(None);
        self.commit();
        self.queue.close();
    }

    pub(crate) fn queue(&self) -> &Arc<BoundedQueue> {
        &self.queue
    }
}

/// Everything `TypedWatcher` keeps alive for one `watch_with` stream.
pub(crate) struct Started {
    pub(crate) watcher: Option<RecommendedWatcher>,
    pub(crate) thread: Option<thread::JoinHandle<()>>,
    pub(crate) native: Option<Handle>,
    pub(crate) queue: Arc<BoundedQueue>,
    pub(crate) volume: Option<[u8; 16]>,
}

enum Resolved {
    Native,
    Notify,
}

fn resolve(backend: WatchBackend) -> Result<Resolved, WatchError> {
    let native_here = cfg!(target_os = "macos") || cfg!(windows);
    match backend {
        WatchBackend::Notify => Ok(Resolved::Notify),
        WatchBackend::Auto => Ok(
            if cfg!(target_os = "macos") || (cfg!(windows) && AUTO_IS_NATIVE_ON_WINDOWS) {
                Resolved::Native
            } else {
                Resolved::Notify
            },
        ),
        WatchBackend::Native if native_here => Ok(Resolved::Native),
        WatchBackend::Native => Err(WatchError::Backend {
            message: "no native watch backend on this platform; use WatchBackend::Notify".into(),
            paths: Vec::new(),
        }),
    }
}

pub(crate) fn start(path: &Path, options: TypedOptions) -> Result<Started, WatchError> {
    let root = path.canonicalize().map_err(|error| WatchError::Backend {
        message: error.to_string(),
        paths: vec![path.to_path_buf()],
    })?;
    let queue = Arc::new(BoundedQueue::new(options.queue_capacity_get()));
    match resolve(options.backend)? {
        Resolved::Native => {
            // macOS folds directories natively; the Windows reader reports files, so its sink folds.
            let fold_dirs = cfg!(windows) && options.granularity == Granularity::Directory;
            let sink = BackendSink::new(queue.clone(), &options, fold_dirs);
            match platform::start(&root, &options, sink) {
                Ok(handle) => {
                    let volume = handle.volume();
                    Ok(Started {
                        watcher: None,
                        thread: None,
                        native: Some(handle),
                        queue,
                        volume,
                    })
                }
                // A failed native open or arm leaves the queue untouched, so notify can take over.
                Err(error) if cfg!(windows) => {
                    tracing::debug!(%error, path = %root.display(), "native typed watch unavailable; using notify");
                    start_notify(root, &options, queue)
                }
                Err(error) => Err(error),
            }
        }
        Resolved::Notify => start_notify(root, &options, queue),
    }
}

/// notify has no replay: a requested cursor is answered with a leading `Overflow`.
fn start_notify(
    root: PathBuf,
    options: &TypedOptions,
    queue: Arc<BoundedQueue>,
) -> Result<Started, WatchError> {
    let (raw_tx, raw_rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |result| {
        let _ = raw_tx.send(result);
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    if options.replay_from.is_some() {
        queue.push_all(vec![Entry::new(unusable_cursor_overflow(), None)], false);
    }
    let sink = BackendSink::new(
        queue.clone(),
        options,
        options.granularity == Granularity::Directory,
    );
    let thread = thread::Builder::new()
        .name("rightkit-watch-typed".into())
        .spawn(move || run_notify(raw_rx, sink))
        .map_err(|error| WatchError::Backend {
            message: error.to_string(),
            paths: vec![root],
        })?;
    Ok(Started {
        watcher: Some(watcher),
        thread: Some(thread),
        native: None,
        queue,
        volume: None,
    })
}

/// The record a consumer gets first when its cursor cannot be honoured. No backend kind: nothing
/// in the backend produced it.
pub(crate) fn unusable_cursor_overflow() -> WatchEventRecord {
    WatchEventRecord {
        event: WatchEvent::Overflow,
        raw_kind: None,
    }
}

fn run_notify(rx: mpsc::Receiver<notify::Result<Event>>, mut sink: BackendSink) {
    loop {
        let result = match sink.next_deadline() {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let alive = match result {
            Ok(result) => {
                sink.event(result, None);
                sink.commit()
            }
            Err(RecvTimeoutError::Timeout) => {
                sink.expire();
                sink.commit()
            }
            Err(RecvTimeoutError::Disconnected) => {
                sink.finish();
                return;
            }
        };
        if !alive {
            sink.queue().close();
            return;
        }
    }
}

pub(crate) fn current_cursor(path: &Path) -> Option<WatchCursor> {
    platform::current_cursor(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;

    fn record(event: WatchEvent) -> WatchEventRecord {
        WatchEventRecord {
            event,
            raw_kind: Some(EventKind::Any),
        }
    }

    #[test]
    fn cursor_bytes_round_trip_and_reject_foreign_input() {
        let cursor = WatchCursor::new([7; 16], 0x0102_0304_0506_0708);
        let bytes = cursor.to_bytes();
        assert_eq!(bytes.len(), WatchCursor::ENCODED_LEN);
        assert_eq!(bytes[0], 1);
        assert_eq!(&bytes[17..], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(WatchCursor::from_bytes(&bytes), Some(cursor));
        assert_eq!(WatchCursor::from_bytes(&bytes[..24]), None);
        assert_eq!(WatchCursor::from_bytes(&[]), None);
        let mut other = bytes;
        other[0] = 2;
        assert_eq!(WatchCursor::from_bytes(&other), None);
    }

    #[test]
    fn exclusions_degrade_renames_and_never_hide_gaps() {
        let options = TypedOptions::new()
            .exclude_prefix("/watched/skip")
            .exclude_with(|p| p.extension().is_some_and(|e| e == "tmp"));
        let ex = Exclusions::from_options(&options);
        let rename = |from: &str, to: &str| {
            record(WatchEvent::Rename {
                from: from.into(),
                to: to.into(),
            })
        };
        assert_eq!(
            ex.filter(record(WatchEvent::Create("/watched/skip/a".into()))),
            None
        );
        assert_eq!(
            ex.filter(record(WatchEvent::Modify("/watched/x.tmp".into()))),
            None
        );
        assert!(ex
            .filter(record(WatchEvent::Modify("/watched/skipper".into())))
            .is_some());
        assert_eq!(
            ex.filter(rename("/watched/skip/a", "/watched/b"))
                .unwrap()
                .event,
            WatchEvent::Create("/watched/b".into())
        );
        assert_eq!(
            ex.filter(rename("/watched/a", "/watched/skip/b"))
                .unwrap()
                .event,
            WatchEvent::Delete("/watched/a".into())
        );
        assert_eq!(ex.filter(rename("/watched/skip/a", "/watched/q.tmp")), None);
        assert!(ex.filter(record(WatchEvent::Overflow)).is_some());
        let error = WatchEvent::Error(WatchError::Overflow);
        assert!(ex.filter(record(error)).is_some());
    }

    #[test]
    fn directory_fold_keeps_raw_kind_and_merges_same_parent_renames() {
        let folded = fold_to_directory(record(WatchEvent::Create("/a/b/c.txt".into())));
        assert_eq!(folded, vec![record(WatchEvent::Modify("/a/b".into()))]);
        let same = fold_to_directory(record(WatchEvent::Rename {
            from: "/a/b/x".into(),
            to: "/a/b/y".into(),
        }));
        assert_eq!(same.len(), 1);
        let cross = fold_to_directory(record(WatchEvent::Rename {
            from: "/a/b/x".into(),
            to: "/a/c/y".into(),
        }));
        assert_eq!(cross.len(), 2);
        assert_eq!(
            fold_to_directory(record(WatchEvent::Overflow)),
            vec![record(WatchEvent::Overflow)]
        );
    }

    #[test]
    fn windows_buffers_round_down_to_four_with_a_floor() {
        assert_eq!(normalize_buffer(10), 4096);
        assert_eq!(normalize_buffer(65_539), 65_536);
        assert_eq!(normalize_buffer(262_144), 262_144);
    }

    #[test]
    fn native_is_an_error_where_no_native_backend_exists() {
        let result = resolve(WatchBackend::Native);
        assert_eq!(result.is_ok(), cfg!(any(target_os = "macos", windows)));
    }
}
