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
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::coalesce::{ChangeBatch, ChangeCoalescer};

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
    watcher: RecommendedWatcher,
    thread: thread::JoinHandle<()>,
}

#[derive(Default)]
struct DirMap {
    map: HashMap<PathBuf, DirWatch>,
    order: VecDeque<PathBuf>,
}

impl DirMap {
    fn remove(&mut self, key: &Path) {
        if let Some(watch) = self.map.remove(key) {
            drop(watch.watcher);
            let _ = watch.thread.join();
        }
        self.order.retain(|k| k != key);
    }
}

/// Capped set of directory watches. Non-recursive unless explicitly opted in.
pub struct DirWatchSet {
    cap: usize,
    quiet: Duration,
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
        Self {
            cap: cap.max(1),
            quiet,
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
        let key = normalize(dir);
        let mut guard = self.lock();
        if guard.map.contains_key(&key) {
            return Ok(());
        }
        let dead: Vec<PathBuf> = guard.map.keys().filter(|k| !k.is_dir()).cloned().collect();
        for k in dead {
            guard.remove(&k);
        }
        while guard.map.len() >= self.cap {
            match guard.order.front().cloned() {
                Some(oldest) => guard.remove(&oldest),
                None => break,
            }
        }
        let (tx, rx) = mpsc::channel::<Event>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(mut event) = res {
                if mutating(&event) {
                    event.paths = event.paths.iter().map(|p| normalize(p)).collect();
                    let _ = tx.send(event);
                }
            }
        })?;
        watcher.watch(
            &key,
            if self.recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            },
        )?;
        let quiet = self.quiet;
        let thread = thread::Builder::new()
            .name("rightkit-watch-dir".into())
            .spawn(move || {
                run_debounce_loop(rx, quiet, quiet * 10, |coalescer| {
                    let batch = coalescer.drain();
                    if !batch.is_empty() {
                        on_change(batch);
                    }
                });
            })
            .map_err(notify::Error::io)?;
        guard.map.insert(key.clone(), DirWatch { watcher, thread });
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
}
