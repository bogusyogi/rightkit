//! Thread-based debounced watchers (ViewRight donor, generalized).
//!
//! * [`FileSetWatcher`] watches the *parent directories* of a set of files and reports only the
//!   files asked for. Directory watching is what makes rename-over-original saves (write temp,
//!   rename over target) surface even though the target inode was replaced. Beyond the donor:
//!   targets that do not exist yet are watchable, deletions are reported, bursts are bounded by a
//!   max-wait so a continuous storm still flushes, and rename storms are coalesced.
//! * [`DirWatchSet`] is the explorer-tree primitive: capped, idempotent, oldest-evicted
//!   directory watches with one callback per debounced batch (optionally recursive).
//!
//! The donor emitted through a Tauri `AppHandle`; here emission is a plain callback so any app
//! (Tauri, CLI, daemon) wires its own transport.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::coalesce::{ChangeBatch, ChangeCoalescer};
use crate::{WatchEvent, WatchEventRecord, WatchOptions};

/// Trailing quiet window for document watches.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(100);
/// A continuous storm is flushed at least this often.
pub const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(2);
/// Quiet window for explorer directory watches.
pub const EXPLORER_DEBOUNCE: Duration = Duration::from_millis(200);
/// Maximum concurrently watched explorer folders; the oldest is evicted past the cap.
pub const EXPLORER_WATCH_CAP: usize = 32;

/// Answers "is this canonical path still authorized?". A change to a path whose grant was revoked
/// between arming and the event is dropped, never emitted.
pub trait GrantCheck: Send + Sync + 'static {
    fn is_granted(&self, canonical: &Path) -> bool;
}

impl<F: Fn(&Path) -> bool + Send + Sync + 'static> GrantCheck for F {
    fn is_granted(&self, canonical: &Path) -> bool {
        self(canonical)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    /// Created or modified; the file exists now.
    Changed,
    /// The file is gone.
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The path exactly as the caller passed it to [`FileSetWatcher::watch`].
    pub path: PathBuf,
    pub kind: FileChangeKind,
}

/// Canonical spelling that also works for paths that do not exist (yet or any more):
/// canonicalize the parent directory and re-attach the file name.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            std::fs::canonicalize(parent)
                .map(|p| p.join(name))
                .unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

fn run_debounce_loop(
    rx: mpsc::Receiver<Event>,
    quiet: Duration,
    max_wait: Duration,
    mut flush: impl FnMut(&mut ChangeCoalescer),
) {
    let mut coalescer = ChangeCoalescer::new();
    while let Ok(first) = rx.recv() {
        coalescer.push(&first);
        let started = Instant::now();
        loop {
            let remaining_cap = max_wait.saturating_sub(started.elapsed());
            let wait = quiet.min(remaining_cap);
            if wait.is_zero() {
                break;
            }
            match rx.recv_timeout(wait) {
                Ok(event) => coalescer.push(&event),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    flush(&mut coalescer);
                    return;
                }
            }
        }
        flush(&mut coalescer);
    }
}

fn mutating(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// Watches a set of files; dropping it stops the watch and joins the debounce thread.
pub struct FileSetWatcher {
    watcher: Option<RecommendedWatcher>,
    thread: Option<thread::JoinHandle<()>>,
    stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl FileSetWatcher {
    /// Arm a watch over `paths`. `on_change` runs on the debounce thread once per batch with the
    /// watched files that changed (deduplicated, sorted). Parent-directory events are reconciled
    /// against target stats so child deletion remains observable on FSEvents. Directories that
    /// cannot be watched are skipped so one bad path never disarms the rest; if none could be
    /// watched, that is an error.
    pub fn watch(
        paths: &[PathBuf],
        quiet: Duration,
        max_wait: Duration,
        grants: impl GrantCheck,
        on_change: impl Fn(Vec<FileChange>) + Send + 'static,
    ) -> notify::Result<Self> {
        let mut display: HashMap<PathBuf, PathBuf> = HashMap::new();
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for path in paths {
            let key = normalize(path);
            if let Some(parent) = key.parent() {
                dirs.insert(parent.to_path_buf());
            }
            display.insert(key, path.clone());
        }
        // Keep the last observed stat for every requested target. FSEvents can
        // report a parent-directory change instead of the removed child; the
        // debounce flush reconciles that parent event against this snapshot.
        let known_exists: HashMap<PathBuf, bool> = display
            .keys()
            .map(|path| (path.clone(), path.symlink_metadata().is_ok()))
            .collect();

        let (tx, rx) = mpsc::channel::<Event>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(mut event) = res {
                if mutating(&event) {
                    event.paths = event.paths.iter().map(|p| normalize(p)).collect();
                    let _ = tx.send(event);
                }
            }
        })?;
        let mut armed = 0usize;
        let mut last_error = None;
        for dir in &dirs {
            match watcher.watch(dir, RecursiveMode::NonRecursive) {
                Ok(()) => armed += 1,
                Err(error) => last_error = Some(error),
            }
        }
        if armed == 0 {
            if let Some(error) = last_error {
                return Err(error);
            }
        }

        let thread = thread::Builder::new()
            .name("rightkit-watch-files".into())
            .spawn(move || {
                let mut known_exists = known_exists;
                run_debounce_loop(rx, quiet, max_wait, |coalescer| {
                    let batch = coalescer.drain();
                    let mut out: Vec<FileChange> = Vec::new();
                    let mut reported = HashSet::new();
                    let mut push = |canonical: &PathBuf, kind: FileChangeKind| {
                        if let Some(shown) = display.get(canonical) {
                            if grants.is_granted(canonical) {
                                out.push(FileChange {
                                    path: shown.clone(),
                                    kind,
                                });
                            }
                        }
                    };
                    for path in &batch.changed {
                        reported.insert(path.clone());
                        push(path, FileChangeKind::Changed);
                    }
                    for path in &batch.removed {
                        reported.insert(path.clone());
                        push(path, FileChangeKind::Removed);
                    }
                    // Reconcile target state after every backend event. This
                    // closes FSEvents' parent-only remove gap while preserving
                    // the same grant gate and requested display path.
                    for canonical in display.keys() {
                        let now_exists = canonical.symlink_metadata().is_ok();
                        let was_exists = known_exists.get(canonical).copied().unwrap_or(now_exists);
                        if was_exists != now_exists && !reported.contains(canonical) {
                            reported.insert(canonical.clone());
                            push(
                                canonical,
                                if now_exists {
                                    FileChangeKind::Changed
                                } else {
                                    FileChangeKind::Removed
                                },
                            );
                        }
                        known_exists.insert(canonical.clone(), now_exists);
                    }
                    if !out.is_empty() {
                        out.sort_by(|a, b| a.path.cmp(&b.path));
                        on_change(out);
                    }
                });
            })
            .map_err(notify::Error::io)?;
        Ok(Self {
            watcher: Some(watcher),
            thread: Some(thread),
            stop: None,
        })
    }

    /// Opt-in typed file-set stream. Parent directories are watched; records use
    /// requested path spelling & grants are checked immediately before delivery.
    /// Keep this handle alive. Dropping its receiver stops the worker & OS watch.
    pub fn watch_with_options(
        paths: &[PathBuf],
        options: crate::WatchOptions,
        grants: impl GrantCheck,
    ) -> Result<(Self, crate::FileSetReceiver), crate::WatchError> {
        let (thread, stop, receiver) = crate::options::start(paths, options, grants)?;
        Ok((
            Self {
                watcher: None,
                thread: Some(thread),
                stop: Some(stop),
            },
            receiver,
        ))
    }

    /// Whether this watch's worker has stopped, including receiver disconnect.
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
    }

    /// Convenience with the donor defaults and no grant gating.
    pub fn watch_default(
        paths: &[PathBuf],
        on_change: impl Fn(Vec<FileChange>) + Send + 'static,
    ) -> notify::Result<Self> {
        Self::watch(
            paths,
            DEFAULT_DEBOUNCE,
            DEFAULT_MAX_WAIT,
            |_: &Path| true,
            on_change,
        )
    }
}

impl Drop for FileSetWatcher {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, std::sync::atomic::Ordering::Release);
        }
        // Dropping the watcher drops the event sender, which ends the debounce loop.
        self.watcher.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct DirWatch {
    /// None while a retrying watch waits for its folder: the worker thread owns the watcher.
    watcher: Option<RecommendedWatcher>,
    thread: thread::JoinHandle<()>,
    stop: Arc<AtomicBool>,
    /// True for a watch created by `*_when_present`; it is never pruned as a dead folder.
    retrying: bool,
}

#[derive(Default)]
struct DirMap {
    map: HashMap<PathBuf, DirWatch>,
    order: VecDeque<PathBuf>,
}

impl DirMap {
    fn remove(&mut self, key: &Path) {
        if let Some(watch) = self.map.remove(key) {
            watch.stop.store(true, Ordering::Release);
            drop(watch.watcher);
            let _ = watch.thread.join();
        }
        self.order.retain(|k| k != key);
    }
}

/// What one directory batch reports. `lost` means the backend could not report everything
/// (buffer overflow, rescan request, or an error): `changes` is then incomplete and the caller
/// must rescan the folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirBatch {
    pub changes: ChangeBatch,
    pub lost: bool,
}

/// Pure mapping from typed records to a [`DirBatch`]: mutations fold into the net lists (sorted,
/// duplicate-free); `Overflow` and `Error` set `lost`.
pub(crate) fn dir_batch(records: Vec<WatchEventRecord>) -> DirBatch {
    let mut batch = ChangeBatch::default();
    let mut lost = false;
    for record in records {
        match record.event {
            WatchEvent::Create(path) | WatchEvent::Modify(path) => batch.changed.push(path),
            WatchEvent::Delete(path) => batch.removed.push(path),
            WatchEvent::Rename { from, to } => {
                batch.changed.push(to.clone());
                batch.removed.push(from.clone());
                batch.renamed.push(crate::Rename { from, to });
            }
            WatchEvent::Error(_) | WatchEvent::Overflow => lost = true,
        }
    }
    batch.changed.sort();
    batch.changed.dedup();
    batch.removed.sort();
    batch.removed.dedup();
    DirBatch {
        changes: batch,
        lost,
    }
}

/// Backoff for a folder that does not exist yet: the wait before attempt `n` is
/// `initial * 2^n`, never more than `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetryPolicy {
    pub initial: Duration,
    pub max: Duration,
}

impl RetryPolicy {
    pub const fn new(initial: Duration, max: Duration) -> Self {
        Self { initial, max }
    }

    /// Wait before the `attempt`-th check (0-based). Pure; saturates instead of overflowing.
    pub fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
        self.initial.saturating_mul(factor).min(self.max)
    }
}

impl Default for RetryPolicy {
    /// 250 ms doubling to 30 s.
    fn default() -> Self {
        Self::new(Duration::from_millis(250), Duration::from_secs(30))
    }
}

type ArmedDirectory = (
    RecommendedWatcher,
    mpsc::Receiver<notify::Result<Event>>,
    crate::options::Delivery<fn(&Path) -> bool>,
);

fn allow_all(_: &Path) -> bool {
    true
}

fn arm_directory(
    key: &Path,
    recursive: bool,
    options: &WatchOptions,
) -> notify::Result<ArmedDirectory> {
    let (tx, rx) = mpsc::channel();
    let backend_tx = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let _ = backend_tx.send(res);
    })?;
    if let Err(mut error) = watcher.watch(
        key,
        if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        },
    ) {
        if !options.tolerate_arm_failures {
            return Err(error);
        }
        if error.paths.is_empty() {
            error.paths.push(key.to_path_buf());
        }
        let _ = tx.send(Err(error));
    }
    drop(tx);
    let delivery = crate::options::Delivery::directory(
        key.to_path_buf(),
        recursive,
        options.clone(),
        allow_all as fn(&Path) -> bool,
    )?;
    Ok((watcher, rx, delivery))
}

/// Sleep up to `total`, waking early when `stop` is set. Returns true if stopped.
fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) -> bool {
    const SLICE: Duration = Duration::from_millis(50);
    let deadline = Instant::now() + total;
    loop {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        thread::sleep(left.min(SLICE));
    }
}

fn retry_worker(
    key: PathBuf,
    recursive: bool,
    options: WatchOptions,
    policy: RetryPolicy,
    on_change: impl Fn(Vec<WatchEventRecord>) + Send + 'static,
    stop: &AtomicBool,
) {
    let mut attempt = 0u32;
    let armed = loop {
        if key.is_dir() {
            match arm_directory(&key, recursive, &options) {
                Ok(armed) => break armed,
                // The folder vanished again or is not yet readable: keep waiting.
                Err(error) => {
                    tracing::debug!(%error, path = %key.display(), "retrying directory watch")
                }
            }
        }
        if sleep_unless_stopped(policy.delay(attempt), stop) {
            return;
        }
        attempt = attempt.saturating_add(1);
    };
    let (_watcher, rx, delivery) = armed;
    // Changes between creation and arming are not reported: tell the caller to rescan.
    on_change(vec![WatchEventRecord {
        event: WatchEvent::Overflow,
        raw_kind: None,
    }]);
    crate::options::run_callback(
        rx,
        |records| {
            on_change(records);
            true
        },
        delivery,
        stop,
        &AtomicBool::new(true),
    );
}

/// Capped set of directory watches. Non-recursive unless explicitly opted in.
pub struct DirWatchSet {
    cap: usize,
    options: WatchOptions,
    recursive: bool,
    inner: Mutex<DirMap>,
}

impl Default for DirWatchSet {
    fn default() -> Self {
        Self::new(EXPLORER_WATCH_CAP, EXPLORER_DEBOUNCE)
    }
}

impl DirWatchSet {
    pub fn new(cap: usize, quiet: Duration) -> Self {
        Self::with_recursive(cap, quiet, false)
    }

    /// Creates a watch set with an explicit recursion mode. The cap counts roots.
    pub fn with_recursive(cap: usize, quiet: Duration, recursive: bool) -> Self {
        Self::with_options(
            cap,
            recursive,
            WatchOptions {
                quiet,
                max_wait: Some(quiet * 10),
                // Legacy directory watches resolve touched paths only.
                reconcile: false,
                ..WatchOptions::default()
            },
        )
    }

    /// Creates a watch set using the same delivery policy as file-set streams.
    /// `WatchOptions::default()` enables snapshot reconciliation; legacy constructors do not.
    pub fn with_options(cap: usize, recursive: bool, options: WatchOptions) -> Self {
        Self {
            cap: cap.max(1),
            options,
            recursive,
            inner: Mutex::new(DirMap::default()),
        }
    }

    pub fn len(&self) -> usize {
        self.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_watching(&self, dir: &Path) -> bool {
        self.lock().map.contains_key(&normalize(dir))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DirMap> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Watch `dir` (idempotent on its canonical path). Dead watches (folders deleted or
    /// unmounted) are pruned first, then the oldest are evicted until under the cap — over-cap is
    /// never an error. `on_change` runs once per debounced batch with the net [`ChangeBatch`].
    pub fn watch(
        &self,
        dir: &Path,
        on_change: impl Fn(ChangeBatch) + Send + 'static,
    ) -> notify::Result<()> {
        self.watch_records(dir, move |records| {
            let DirBatch { changes, lost } = dir_batch(records);
            if lost {
                tracing::warn!("directory watch gap: events may have been lost");
            }
            if !changes.is_empty() {
                on_change(changes);
            }
        })
    }

    /// Like [`watch`](Self::watch), but a lost report is delivered instead of only logged: the
    /// callback receives a [`DirBatch`] whose `lost` flag is set when the backend reported a
    /// buffer overflow, a rescan request or an error. A batch that carries only the flag is still
    /// delivered (with empty `changes`). The caller must rescan the folder when `lost` is set.
    pub fn watch_batches(
        &self,
        dir: &Path,
        on_change: impl Fn(DirBatch) + Send + 'static,
    ) -> notify::Result<()> {
        self.watch_records(dir, move |records| {
            let batch = dir_batch(records);
            if batch.lost || !batch.changes.is_empty() {
                on_change(batch);
            }
        })
    }

    /// [`watch_batches`](Self::watch_batches) for a folder that may not exist yet. If `dir` is
    /// missing, a background worker polls for it with the backoff in `retry` and arms the watch
    /// once it appears; nothing is returned as an error. On attach the callback first receives a
    /// batch with `lost` set, because changes made between creation and arming are not reported:
    /// rescan then. The worker is stopped by `unwatch`, eviction or dropping the set. A folder
    /// deleted after attaching is not re-armed; call this again.
    pub fn watch_batches_when_present(
        &self,
        dir: &Path,
        retry: RetryPolicy,
        on_change: impl Fn(DirBatch) + Send + 'static,
    ) -> notify::Result<()> {
        self.watch_records_when_present(dir, retry, move |records| {
            let batch = dir_batch(records);
            if batch.lost || !batch.changes.is_empty() {
                on_change(batch);
            }
        })
    }

    /// Typed-record form of [`watch_batches_when_present`](Self::watch_batches_when_present).
    /// On attach after a wait, the first callback carries one `WatchEvent::Overflow` record.
    pub fn watch_records_when_present(
        &self,
        dir: &Path,
        retry: RetryPolicy,
        on_change: impl Fn(Vec<WatchEventRecord>) + Send + 'static,
    ) -> notify::Result<()> {
        self.watch_records_inner(dir, Some(retry), on_change)
    }

    /// Watch a directory with typed batches, backend provenance & unfiltered gap diagnostics.
    /// With coalescing off, each backend hint is delivered immediately without deduplication.
    /// Tolerated arm failures emit Error records; they do not retry registration.
    /// Unwatch, eviction & set drop flush pending records unless discard is enabled.
    /// Callbacks run on the watch thread; they must not synchronously unwatch their own set.
    /// Eviction and `unwatch` join the watch thread before returning: the final flush (and any
    /// backend history replayed on arm) is delivered inside that call, and no callback for the
    /// directory runs after it returns.
    pub fn watch_records(
        &self,
        dir: &Path,
        on_change: impl Fn(Vec<WatchEventRecord>) + Send + 'static,
    ) -> notify::Result<()> {
        self.watch_records_inner(dir, None, on_change)
    }

    fn watch_records_inner(
        &self,
        dir: &Path,
        retry: Option<RetryPolicy>,
        on_change: impl Fn(Vec<WatchEventRecord>) + Send + 'static,
    ) -> notify::Result<()> {
        let key = normalize(dir);
        let mut guard = self.lock();
        if guard.map.contains_key(&key) {
            return Ok(());
        }
        let dead: Vec<PathBuf> = guard
            .map
            .iter()
            .filter(|(k, w)| !w.retrying && !k.is_dir())
            .map(|(k, _)| k.clone())
            .collect();
        for k in dead {
            guard.remove(&k);
        }
        while guard.map.len() >= self.cap {
            match guard.order.front().cloned() {
                Some(oldest) => guard.remove(&oldest),
                None => break,
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let watch = if let (Some(policy), false) = (retry, key.is_dir()) {
            let worker_key = key.clone();
            let recursive = self.recursive;
            let options = self.options.clone();
            let thread = thread::Builder::new()
                .name("rightkit-watch-dir-retry".into())
                .spawn(move || {
                    retry_worker(
                        worker_key,
                        recursive,
                        options,
                        policy,
                        on_change,
                        &worker_stop,
                    );
                })
                .map_err(notify::Error::io)?;
            DirWatch {
                watcher: None,
                thread,
                stop,
                retrying: true,
            }
        } else {
            let (watcher, rx, delivery) = arm_directory(&key, self.recursive, &self.options)?;
            let thread = thread::Builder::new()
                .name("rightkit-watch-dir".into())
                .spawn(move || {
                    crate::options::run_callback(
                        rx,
                        |records| {
                            on_change(records);
                            true
                        },
                        delivery,
                        &worker_stop,
                        &AtomicBool::new(true),
                    );
                })
                .map_err(notify::Error::io)?;
            DirWatch {
                watcher: Some(watcher),
                thread,
                stop,
                retrying: false,
            }
        };
        guard.map.insert(key.clone(), watch);
        guard.order.push_back(key);
        Ok(())
    }

    /// Stop watching `dir`. Missing entries are ignored.
    pub fn unwatch(&self, dir: &Path) {
        let mut guard = self.lock();
        guard.remove(&normalize(dir));
        guard.remove(dir);
    }
}

impl Drop for DirWatchSet {
    fn drop(&mut self) {
        let keys: Vec<PathBuf> = self.lock().map.keys().cloned().collect();
        let mut guard = self.lock();
        for key in keys {
            guard.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_recursion_is_opt_in() {
        assert!(!DirWatchSet::default().recursive);
        assert!(!DirWatchSet::new(1, DEFAULT_DEBOUNCE).recursive);
        assert!(DirWatchSet::with_recursive(1, DEFAULT_DEBOUNCE, true).recursive);
    }

    #[test]
    fn directory_legacy_policy_keeps_quiet_times_ten_and_no_snapshot() {
        let quiet = Duration::from_millis(37);
        for set in [
            DirWatchSet::new(0, quiet),
            DirWatchSet::with_recursive(2, quiet, true),
        ] {
            assert_eq!(set.options.quiet, quiet);
            assert_eq!(set.options.max_wait, Some(quiet * 10));
            assert!(set.options.coalesce);
            assert!(!set.options.reconcile);
            assert!(!set.options.tolerate_arm_failures);
            assert!(!set.options.discard_on_disconnect);
            assert!(set
                .options
                .filter
                .accepts(&WatchEvent::Create("file".into())));
            assert!(set.cap >= 1);
        }
    }

    #[test]
    fn directory_options_are_kept_without_overriding_caller_policy() {
        let options = WatchOptions {
            max_wait: None,
            coalesce: false,
            tolerate_arm_failures: true,
            discard_on_disconnect: true,
            filter: crate::EventFilter::new([crate::WatchEventKind::Delete]),
            ..WatchOptions::default()
        };
        let set = DirWatchSet::with_options(3, true, options.clone());
        assert_eq!(set.options, options);
        assert!(set.recursive);
        assert_eq!(set.cap, 3);
    }

    fn rec(event: WatchEvent) -> WatchEventRecord {
        WatchEventRecord {
            event,
            raw_kind: None,
        }
    }

    #[test]
    fn overflow_and_error_records_map_to_lost_batch() {
        let only_gap = dir_batch(vec![rec(WatchEvent::Overflow)]);
        assert!(only_gap.lost);
        assert!(only_gap.changes.is_empty());

        let error = dir_batch(vec![rec(WatchEvent::Error(
            notify::Error::generic("gap").into(),
        ))]);
        assert!(error.lost);

        let clean = dir_batch(vec![rec(WatchEvent::Create("a".into()))]);
        assert!(!clean.lost);
        assert_eq!(clean.changes.changed, vec![PathBuf::from("a")]);
    }

    #[test]
    fn mutations_and_gap_share_one_sorted_deduplicated_batch() {
        let out = dir_batch(vec![
            rec(WatchEvent::Modify("b".into())),
            rec(WatchEvent::Create("a".into())),
            rec(WatchEvent::Modify("b".into())),
            rec(WatchEvent::Delete("z".into())),
            rec(WatchEvent::Overflow),
            rec(WatchEvent::Rename {
                from: "old".into(),
                to: "new".into(),
            }),
        ]);
        assert!(out.lost);
        assert_eq!(
            out.changes.changed,
            vec![PathBuf::from("a"), "b".into(), "new".into()]
        );
        assert_eq!(out.changes.removed, vec![PathBuf::from("old"), "z".into()]);
        assert_eq!(out.changes.renamed.len(), 1);
    }

    #[test]
    fn retry_delay_doubles_and_saturates_at_max() {
        let p = RetryPolicy::new(Duration::from_millis(100), Duration::from_millis(1000));
        let delays: Vec<u128> = (0..6).map(|n| p.delay(n).as_millis()).collect();
        assert_eq!(delays, vec![100, 200, 400, 800, 1000, 1000]);
        assert_eq!(p.delay(200), Duration::from_millis(1000));
        assert_eq!(p.delay(u32::MAX), Duration::from_millis(1000));
        assert_eq!(RetryPolicy::default().delay(0), Duration::from_millis(250));
    }
}
