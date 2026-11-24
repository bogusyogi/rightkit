use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, MutexGuard};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::IO::CancelSynchronousIo;

use rightkit_file_watch::{RetryPolicy, TypedOptions, TypedWatcher, WatchEvent};

use super::{
    Index, Root, RootTasks, STALE_AFTER, WalkEntry, build_until, excluded, lower_priority, norm,
    scope_with_settings, wait_for_root, walk_entry,
};
use crate::host::IndexScope;

const CHANGE_QUIET: Duration = Duration::from_secs(4);
const MAX_DIRTY_WAIT: Duration = Duration::from_secs(30);
const REBUILD_GAP: Duration = Duration::from_secs(120);
const REPAIR_GAP: Duration = Duration::from_secs(10);
const WATCH_POLL: Duration = Duration::from_secs(1);

/// The caller can stop using a watcher while Windows finishes cancelling its I/O.
struct RootWatch {
    root: Root,
    stop: Arc<AtomicBool>,
}

impl Drop for RootWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Request {
    folders: Vec<String>,
    settings: IndexScope,
}

struct State {
    request: Option<Request>,
    generation: u64,
    revision: u64,
    running: bool,
    index: Option<Index>,
    dirty: Option<(Instant, Instant)>,
    stale: bool,
    repair: bool,
}

/// Windows launcher file index whose scope follows the embedding app's settings.
pub struct LiveFileIndex {
    state: Mutex<State>,
    wake: Condvar,
    on_revision: fn(),
}

impl LiveFileIndex {
    /// `on_revision` runs after publishing an index and outside the state lock.
    /// Hosts can post a UI notification to refresh an open query.
    pub const fn new(on_revision: fn()) -> Self {
        Self {
            state: Mutex::new(State {
                request: None,
                generation: 0,
                revision: 0,
                running: false,
                index: None,
                dirty: None,
                stale: false,
                repair: false,
            }),
            wake: Condvar::new(),
            on_revision,
        }
    }

    /// Reconcile on open and settings changes, even when the panel is closed. `None` disables
    /// the index and releases the active watches, `Some(vec![])` indexes the default roots.
    /// Returns true if the requested scope or activation changed.
    pub fn configure(&'static self, folders: Option<Vec<String>>) -> bool {
        let request = folders.map(|folders| Request {
            folders,
            settings: crate::host::file_index_scope(),
        });
        let mut state = self.lock();
        let changed = state.request != request;
        if changed {
            state.request = request;
            state.generation = state.generation.wrapping_add(1);
            state.revision = state.revision.wrapping_add(1);
            state.index = None;
            state.dirty = None;
            state.stale = false;
            state.repair = false;
        } else if state
            .index
            .as_ref()
            .is_some_and(|index| index.inner.built.elapsed() >= STALE_AFTER)
        {
            state.stale = true;
        }
        if state.request.is_some() && !state.running {
            state.running = std::thread::Builder::new()
                .name("launcher-file-index".into())
                .spawn(move || self.run())
                .is_ok();
        }
        self.wake.notify_one();
        changed
    }

    /// Most recent results for the current generation, even while a refresh runs.
    pub fn cached(&self) -> (u64, Option<Index>) {
        let state = self.lock();
        (state.revision, state.index.clone())
    }

    /// Identifies the current index or scope, including changes that invalidate prior results.
    pub fn revision(&self) -> u64 {
        self.lock().revision
    }

    /// True while the current scope is waiting for its first result.
    pub fn loading(&self) -> bool {
        let state = self.lock();
        state.request.is_some() && state.running && state.index.is_none()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn run(&'static self) {
        lower_priority();
        loop {
            let (request, generation) = {
                let mut state = self.lock();
                let Some(request) = state.request.clone() else {
                    state.running = false;
                    return;
                };
                (request, state.generation)
            };
            self.run_scope(request, generation);
        }
    }

    fn run_scope(&'static self, request: Request, generation: u64) {
        let current = || self.lock().generation == generation;
        let scope_now = || scope_with_settings(&request.folders, request.settings.clone(), current);
        let mut scope = scope_now();
        if !current() {
            return;
        }
        let mut watches = Vec::new();
        let mut failed = self.reconcile_watches(&mut watches, &scope.roots, generation);
        let retry = RetryPolicy::default();
        let mut attempt = 0u32;
        let mut armed_at = Instant::now();
        let mut retry_at =
            (failed || scope.unavailable_roots).then(|| armed_at + retry.delay(attempt));
        let mut last_walk = None::<Instant>;
        let mut scan_unavailable = false;
        loop {
            let (walk, repair, retry_due) = {
                let mut state = self.lock();
                loop {
                    if state.generation != generation {
                        return;
                    }
                    let repair_at = state.repair.then(|| armed_at + REPAIR_GAP);
                    let now = Instant::now();
                    let walk_at = next_walk(&state, last_walk, now);
                    let walk = walk_at.is_some_and(|at| at <= now);
                    let repair = repair_at.is_some_and(|at| at <= now);
                    let retry_due = retry_at.is_some_and(|at| at <= now);
                    if walk || repair || retry_due {
                        if repair {
                            state.repair = false;
                        }
                        break (walk, repair, retry_due);
                    }
                    let deadline = [walk_at, repair_at, retry_at].into_iter().flatten().min();
                    state = match deadline {
                        Some(at) => {
                            self.wake
                                .wait_timeout(state, at.saturating_duration_since(now))
                                .unwrap_or_else(PoisonError::into_inner)
                                .0
                        }
                        None => self
                            .wake
                            .wait(state)
                            .unwrap_or_else(PoisonError::into_inner),
                    };
                }
            };
            if repair || retry_due || (walk && last_walk.is_some()) {
                let updated = scope_now();
                if !current() {
                    return;
                }
                let roots_changed = updated.roots != scope.roots;
                if roots_changed || repair || (retry_due && failed) {
                    let was_failed = failed;
                    failed = self.reconcile_watches(&mut watches, &updated.roots, generation);
                    armed_at = Instant::now();
                    if !walk && (!failed || !was_failed || roots_changed) {
                        // Scan after attaching a replacement: mutations during the gap were lost.
                        self.changed(generation, false);
                    }
                }
                if repair
                    || retry_due
                    || roots_changed
                    || updated.unavailable_roots != scope.unavailable_roots
                {
                    if retry_due && scan_unavailable && !walk {
                        self.changed(generation, false);
                    }
                    attempt = if failed || updated.unavailable_roots || scan_unavailable {
                        attempt.saturating_add(1)
                    } else {
                        0
                    };
                    retry_at = (failed || updated.unavailable_roots || scan_unavailable)
                        .then(|| Instant::now() + retry.delay(attempt));
                }
                scope = updated;
            }
            if walk {
                let mut state = self.lock();
                if state.generation != generation {
                    return;
                }
                let now = Instant::now();
                if next_walk(&state, last_walk, now).is_none_or(|deadline| deadline > now) {
                    continue;
                }
                state.dirty = None;
                state.stale = false;
                drop(state);
                last_walk = Some(now);
                let Some((index, unavailable)) = build_until(&scope, || !current(), true) else {
                    return;
                };
                if unavailable {
                    retry_at.get_or_insert_with(|| Instant::now() + retry.delay(attempt));
                } else if scan_unavailable && !failed && !scope.unavailable_roots {
                    attempt = 0;
                    retry_at = None;
                }
                scan_unavailable = unavailable;
                let mut state = self.lock();
                if state.generation != generation {
                    return;
                }
                state.index = Some(index);
                state.revision = state.revision.wrapping_add(1);
                drop(state);
                (self.on_revision)();
            }
        }
    }

    fn reconcile_watches(
        &'static self,
        watches: &mut Vec<RootWatch>,
        roots: &[Root],
        generation: u64,
    ) -> bool {
        watches.retain(|watch| !watch.stop.load(Ordering::Acquire) && roots.contains(&watch.root));
        let mut failed = false;
        for root in roots {
            if self.lock().generation != generation {
                break;
            }
            if watches.iter().any(|watch| &watch.root == root) {
                continue;
            }
            if let Some(watch) = self.arm(root, generation, roots.len()) {
                watches.push(watch);
            } else {
                failed = true;
            }
        }
        failed
    }

    fn arm(&'static self, root: &Root, generation: u64, root_count: usize) -> Option<RootWatch> {
        // Retiring watches and timed-out attachments keep their lease until native I/O returns.
        // The active scope can use one worker per root plus at most 32 spare slots.
        static WATCHES: RootTasks = RootTasks::new();
        let original = norm(&root.path);
        let task = WATCHES.reserve(&watch_path(&root.canonical), root_count.saturating_add(32))?;
        let canonical = watch_path(&root.canonical);
        let excludes = root.excludes.clone();
        let ignore = crate::host::ignore_file_name().to_lowercase();
        let options = TypedOptions::new()
            .exclude_with(move |path| !in_scope(path, &canonical, &original, &excludes, &ignore));
        let watch = RootWatch {
            root: root.clone(),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let stop = Arc::clone(&watch.stop);
        let path = root.path.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("launcher-file-notifications".into())
            .spawn(move || {
                let _task = task;
                let Ok(watcher) = TypedWatcher::watch_with(&path, options) else {
                    return;
                };
                if ready_tx.send(()).is_err() {
                    return;
                }
                while !stop.load(Ordering::Acquire) {
                    match watcher.recv_timeout(WATCH_POLL) {
                        Ok(WatchEvent::Error(_)) | Err(RecvTimeoutError::Disconnected) => {
                            if !stop.swap(true, Ordering::AcqRel) {
                                self.changed(generation, true);
                            }
                            return;
                        }
                        Ok(_) => {
                            if !stop.load(Ordering::Acquire) {
                                self.changed(generation, false);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .ok()?;
        wait_for_root(&ready_rx, || self.lock().generation == generation)?;
        Some(watch)
    }

    fn changed(&self, generation: u64, repair: bool) {
        let mut state = self.lock();
        if state.generation == generation {
            let now = Instant::now();
            let first = state.dirty.map_or(now, |(first, _)| first);
            state.dirty = Some((first, now));
            state.repair |= repair;
            self.wake.notify_one();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadResult {
    Complete,
    Skipped,
    Unavailable,
    Stalled,
}

enum DirectoryBatch {
    Rows(Vec<Option<WalkEntry>>),
    End(ReadResult),
}

type ReadRequest = (PathBuf, SyncSender<DirectoryBatch>);

/// A directory worker is reused for the duration of a root scan. The index
/// can move on from a stalled worker while the native I/O finishes.
pub(super) struct RootReader {
    requests: SyncSender<ReadRequest>,
    worker: JoinHandle<()>,
    stop: Arc<AtomicBool>,
    result: ReadResult,
}

impl Drop for RootReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if self.result == ReadResult::Stalled {
            // SAFETY: The JoinHandle keeps this worker's Windows thread
            // handle valid during the non-waiting cancellation request.
            let _ = unsafe { CancelSynchronousIo(HANDLE(self.worker.as_raw_handle())) };
        }
    }
}

impl RootReader {
    pub(super) fn new(root: &Root) -> Option<Self> {
        static FIXED_SCANS: RootTasks = RootTasks::new();
        static OTHER_SCANS: RootTasks = RootTasks::new();
        // Disconnected shares cannot use the fixed-drive worker capacity.
        let (pool, limit) = if root.fixed_local {
            (&FIXED_SCANS, 8)
        } else {
            (&OTHER_SCANS, 16)
        };
        let task = pool.reserve(&watch_path(&root.canonical), limit)?;
        let root = root.path.clone();
        let (tx, rx) = mpsc::sync_channel::<ReadRequest>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("launcher-directory-reader".into())
            .spawn(move || {
                let _task = task;
                lower_priority();
                while let Ok((dir, batches)) = rx.recv() {
                    let Some(result) = scan_directory(&dir, &root, &batches, &worker_stop) else {
                        return;
                    };
                    if batches.send(DirectoryBatch::End(result)).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self {
            requests: tx,
            worker,
            stop,
            result: ReadResult::Complete,
        })
    }

    pub(super) fn entries<'a>(
        &'a mut self,
        dir: PathBuf,
        current: &'a impl Fn() -> bool,
    ) -> impl Iterator<Item = Option<WalkEntry>> + 'a {
        let (batches, rx) = mpsc::sync_channel(1);
        let mut ended = self.requests.try_send((dir, batches)).is_err();
        self.result = ReadResult::Stalled;
        let mut pending = Vec::new().into_iter();
        std::iter::from_fn(move || {
            while !ended && current() {
                if let Some(row) = pending.next() {
                    return Some(row);
                }
                match wait_for_root(&rx, current) {
                    Some(DirectoryBatch::Rows(rows)) => pending = rows.into_iter(),
                    Some(DirectoryBatch::End(result)) => {
                        self.result = result;
                        ended = true;
                    }
                    None => ended = true,
                }
            }
            None
        })
    }

    pub(super) fn result(&self) -> ReadResult {
        self.result
    }
}

fn scan_directory(
    dir: &Path,
    root: &Path,
    batches: &SyncSender<DirectoryBatch>,
    stop: &AtomicBool,
) -> Option<ReadResult> {
    use std::io::ErrorKind;

    if stop.load(Ordering::Acquire) {
        return None;
    }
    let mut entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            // An inaccessible or concurrently deleted child is normal. A root
            // failure or a transport error leaves the root eligible for retry.
            let ordinary_child = dir != root
                && matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::PermissionDenied
                );
            return Some(if ordinary_child {
                ReadResult::Skipped
            } else {
                ReadResult::Unavailable
            });
        }
    };
    const ROWS_PER_BATCH: usize = 64;
    let mut rows = Vec::with_capacity(ROWS_PER_BATCH);
    let mut result = ReadResult::Complete;
    while !stop.load(Ordering::Acquire) {
        let Some(entry) = entries.next() else {
            break;
        };
        match entry {
            Ok(entry) => rows.push(walk_entry(entry)),
            Err(error) => {
                if !matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::PermissionDenied
                ) {
                    result = ReadResult::Unavailable;
                    break;
                }
            }
        }
        if rows.len() == ROWS_PER_BATCH {
            let batch = std::mem::replace(&mut rows, Vec::with_capacity(ROWS_PER_BATCH));
            batches.send(DirectoryBatch::Rows(batch)).ok()?;
        }
    }
    if !rows.is_empty() {
        batches.send(DirectoryBatch::Rows(rows)).ok()?;
    }
    Some(result)
}

fn next_walk(state: &State, last_walk: Option<Instant>, now: Instant) -> Option<Instant> {
    let Some(last) = last_walk else {
        return Some(now);
    };
    let paced = last + REBUILD_GAP;
    if state.stale {
        return Some(paced);
    }
    state
        .dirty
        .map(|(first, last)| (last + CHANGE_QUIET).min(first + MAX_DIRTY_WAIT).max(paced))
}

pub(super) fn watch_path(path: &Path) -> String {
    let path = norm(path);
    match path.strip_prefix("//?/unc/") {
        Some(unc) => format!("//{unc}"),
        None => path.strip_prefix("//?/").unwrap_or(&path).to_owned(),
    }
}

fn in_scope(
    path: &Path,
    canonical: &str,
    original: &str,
    excludes: &[Vec<String>],
    ignore: &str,
) -> bool {
    let path = watch_path(path);
    let canonical = canonical.trim_end_matches('/');
    if path.trim_end_matches('/') == canonical {
        return true;
    }
    let Some(relative) = path
        .strip_prefix(canonical)
        .and_then(|rest| rest.strip_prefix('/'))
    else {
        return false;
    };
    if relative == ignore {
        return true;
    }
    let mut parent = original.trim_end_matches('/').to_owned();
    let mut resolved_parent = canonical.to_owned();
    let mut parts = relative.split('/').peekable();
    while let Some(part) = parts.next() {
        if part.starts_with('.') {
            return false;
        }
        if parts.peek().is_some() {
            parent.push('/');
            parent.push_str(part);
            resolved_parent.push('/');
            resolved_parent.push_str(part);
            if excluded(&resolved_parent, excludes) || excluded(&parent, excludes) {
                return false;
            }
        }
    }
    true
}
