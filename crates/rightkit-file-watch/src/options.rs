//! Opt-in file-set & directory streams. Legacy constructors keep their defaults.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvError, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::sync::{normalize, GrantCheck, DEFAULT_DEBOUNCE, DEFAULT_MAX_WAIT};
use crate::typed::Translator;
use crate::{ChangeCoalescer, WatchError, WatchEvent};

/// Mutation categories accepted by [`EventFilter`]. Gaps always pass filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WatchEventKind {
    Create,
    Modify,
    Delete,
    Rename,
}

/// An immutable subset of typed mutation categories. Default accepts all four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter(BTreeSet<WatchEventKind>);

impl EventFilter {
    pub fn new(kinds: impl IntoIterator<Item = WatchEventKind>) -> Self {
        Self(kinds.into_iter().collect())
    }

    /// Errors & overflow cannot be hidden by a mutation filter.
    pub fn accepts(&self, event: &WatchEvent) -> bool {
        let kind = match event {
            WatchEvent::Create(_) => WatchEventKind::Create,
            WatchEvent::Modify(_) => WatchEventKind::Modify,
            WatchEvent::Delete(_) => WatchEventKind::Delete,
            WatchEvent::Rename { .. } => WatchEventKind::Rename,
            WatchEvent::Error(_) | WatchEvent::Overflow => return true,
        };
        self.0.contains(&kind)
    }
}

impl Default for EventFilter {
    fn default() -> Self {
        Self::new([
            WatchEventKind::Create,
            WatchEventKind::Modify,
            WatchEventKind::Delete,
            WatchEventKind::Rename,
        ])
    }
}

/// Typed hint & its backend provenance. Snapshot-only records/arm errors
/// have no raw kind. Parent-hint reconciliation keeps the triggering raw kind.
/// Coalesced records keep the last contributing backend kind;
/// it may differ from the net typed category. Paired renames keep the final half's kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEventRecord {
    pub event: WatchEvent,
    pub raw_kind: Option<EventKind>,
}

/// File-set & directory stream policy. Existing APIs & their defaults remain unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOptions {
    pub filter: EventFilter,
    pub quiet: Duration,
    /// None means trailing quiet only, with no forced deadline.
    pub max_wait: Option<Duration>,
    /// False returns the first failed parent-directory registration at startup.
    pub tolerate_arm_failures: bool,
    /// True discards pending events on input/consumer disconnect or handle drop.
    /// False flushes on input disconnect & warns on consumer disconnect.
    pub discard_on_disconnect: bool,
    /// Capture target existence initially & reconcile after batches/rescan signals.
    /// False skips both snapshot passes. Coalescing still resolves touched paths.
    pub reconcile: bool,
    /// False delivers every backend path hint immediately, without debounce,
    /// deduplication or rename pairing. Half-renames become Delete/Create.
    pub coalesce: bool,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            filter: EventFilter::default(),
            quiet: DEFAULT_DEBOUNCE,
            max_wait: Some(DEFAULT_MAX_WAIT),
            tolerate_arm_failures: false,
            discard_on_disconnect: false,
            reconcile: true,
            coalesce: true,
        }
    }
}

/// Unbounded typed batch receiver; dropping it stops its worker & OS watch.
/// Queue errors describe disconnect; backend errors arrive inside records.
pub struct FileSetReceiver {
    rx: mpsc::Receiver<Vec<WatchEventRecord>>,
    alive: Arc<AtomicBool>,
}

impl FileSetReceiver {
    pub fn recv(&self) -> Result<Vec<WatchEventRecord>, RecvError> {
        self.rx.recv()
    }

    pub fn try_recv(&self) -> Result<Vec<WatchEventRecord>, TryRecvError> {
        self.rx.try_recv()
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Vec<WatchEventRecord>, RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }
}

impl Drop for FileSetReceiver {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}

type Worker = (thread::JoinHandle<()>, Arc<AtomicBool>, FileSetReceiver);

pub(crate) fn start(
    paths: &[PathBuf],
    options: WatchOptions,
    grants: impl GrantCheck,
) -> Result<Worker, WatchError> {
    let base = if paths.iter().any(|path| !path.is_absolute()) {
        Some(
            std::env::current_dir().map_err(|error| WatchError::Backend {
                message: error.to_string(),
                paths: paths.to_vec(),
            })?,
        )
    } else {
        None
    };
    let display: BTreeMap<_, _> = paths
        .iter()
        .map(|path| {
            let key = match &base {
                Some(base) => normalize(&base.join(path)),
                None => normalize(path),
            };
            (key, path.clone())
        })
        .collect();
    let dirs: BTreeSet<_> = display
        .keys()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect();
    let (raw_tx, raw_rx) = mpsc::channel();
    let backend_tx = raw_tx.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        let _ = backend_tx.send(result);
    })?;
    for dir in dirs {
        if let Err(mut error) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            if error.paths.is_empty() {
                error.paths.push(dir);
            }
            if !options.tolerate_arm_failures {
                return Err(error.into());
            }
            let _ = raw_tx.send(Err(error));
        }
    }
    drop(raw_tx);
    let state = Delivery::new(display, options, grants);
    let stop = Arc::new(AtomicBool::new(false));
    let alive = Arc::new(AtomicBool::new(true));
    let (tx, rx) = mpsc::channel();
    let receiver = FileSetReceiver {
        rx,
        alive: alive.clone(),
    };
    let worker_stop = stop.clone();
    let worker = thread::Builder::new()
        .name("rightkit-watch-options".into())
        .spawn(move || {
            run(raw_rx, tx, state, &worker_stop, &alive);
            // Backend belongs to the worker so receiver disconnect disarms it.
            drop(watcher);
        })
        .map_err(|error| WatchError::Backend {
            message: error.to_string(),
            paths: paths.to_vec(),
        })?;
    Ok((worker, stop, receiver))
}

#[derive(Default)]
struct DebounceWindow {
    first: Option<Instant>,
    last: Option<Instant>,
}

impl DebounceWindow {
    fn observe(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    fn deadline(&self, options: &WatchOptions) -> Option<Instant> {
        let quiet = self.last? + options.quiet;
        Some(match options.max_wait {
            Some(cap) => quiet.min(self.first? + cap),
            None => quiet,
        })
    }
}

pub(crate) struct Delivery<G> {
    display: BTreeMap<PathBuf, PathBuf>,
    directory: Option<(PathBuf, bool)>,
    options: WatchOptions,
    grants: G,
    known_exists: Option<HashMap<PathBuf, bool>>,
    coalescer: ChangeCoalescer,
    provenance: BTreeMap<PathBuf, WatchEventRecord>,
    reconcile_kind: Option<EventKind>,
}

impl<G: GrantCheck> Delivery<G> {
    fn new(display: BTreeMap<PathBuf, PathBuf>, options: WatchOptions, grants: G) -> Self {
        // Lazy bool::then is intentional: disabled reconciliation does no initial stat pass.
        let known_exists = options.reconcile.then(|| {
            display
                .keys()
                .map(|path| (path.clone(), path.symlink_metadata().is_ok()))
                .collect()
        });
        Self {
            display,
            directory: None,
            options,
            grants,
            known_exists,
            coalescer: ChangeCoalescer::new(),
            provenance: BTreeMap::new(),
            reconcile_kind: None,
        }
    }

    pub(crate) fn directory(
        root: PathBuf,
        recursive: bool,
        options: WatchOptions,
        grants: G,
    ) -> notify::Result<Self> {
        let known_exists = if options.reconcile {
            Some(directory_snapshot(&root, recursive)?)
        } else {
            None
        };
        let mut state = Self::new(BTreeMap::new(), options, grants);
        state.directory = Some((root, recursive));
        state.known_exists = known_exists;
        Ok(state)
    }

    fn project(&self, record: WatchEventRecord) -> Vec<WatchEventRecord> {
        let raw_kind = record.raw_kind;
        let shown = |path: &PathBuf| {
            if let Some((root, recursive)) = &self.directory {
                return (path == root
                    || (*recursive && path.starts_with(root))
                    || path.parent() == Some(root.as_path()))
                .then(|| path.clone());
            }
            self.display
                .get(path)
                .filter(|_| self.grants.is_granted(path))
                .cloned()
        };
        let event = match record.event {
            WatchEvent::Create(path) => shown(&path).map(WatchEvent::Create),
            WatchEvent::Modify(path) => shown(&path).map(WatchEvent::Modify),
            WatchEvent::Delete(path) => shown(&path).map(WatchEvent::Delete),
            WatchEvent::Rename { from, to } => match (shown(&from), shown(&to)) {
                (Some(from), Some(to)) => Some(WatchEvent::Rename { from, to }),
                (Some(from), None) => Some(WatchEvent::Delete(from)),
                (None, Some(to)) if self.directory.is_some() => Some(WatchEvent::Create(to)),
                (None, Some(to)) => Some(WatchEvent::Modify(to)),
                (None, None) => None,
            },
            diagnostic => Some(diagnostic),
        };
        event
            .filter(|event| self.options.filter.accepts(event))
            .map(|event| vec![WatchEventRecord { event, raw_kind }])
            .unwrap_or_default()
    }

    fn push(&mut self, record: WatchEventRecord) {
        self.reconcile_kind = record.raw_kind;
        let _ = self
            .coalescer
            .push_watch_record(&record.event, record.raw_kind, None);
        let paths = match &record.event {
            WatchEvent::Create(path) | WatchEvent::Modify(path) | WatchEvent::Delete(path) => {
                vec![path.clone()]
            }
            WatchEvent::Rename { from, to } => vec![from.clone(), to.clone()],
            WatchEvent::Error(_) | WatchEvent::Overflow => Vec::new(),
        };
        for path in paths {
            // Preserve Create through later writes, while updating backend provenance.
            if let Some(old) = self.provenance.get_mut(&path) {
                if matches!(old.event, WatchEvent::Create(_))
                    && matches!(record.event, WatchEvent::Modify(_))
                {
                    old.raw_kind = record.raw_kind;
                    continue;
                }
            }
            self.provenance.insert(path, record.clone());
        }
    }

    fn reconcile(&mut self, records: &mut Vec<WatchEventRecord>) {
        let Some(known) = &mut self.known_exists else {
            return;
        };
        let reported: BTreeSet<_> = records
            .iter()
            .flat_map(|record| match &record.event {
                WatchEvent::Create(path) | WatchEvent::Modify(path) | WatchEvent::Delete(path) => {
                    vec![path.clone()]
                }
                WatchEvent::Rename { from, to } => vec![from.clone(), to.clone()],
                _ => Vec::new(),
            })
            .collect();
        if let Some((root, recursive)) = &self.directory {
            let current = match directory_snapshot(root, *recursive) {
                Ok(current) => current,
                Err(error) => {
                    records.push(WatchEventRecord {
                        event: WatchEvent::Error(error.into()),
                        raw_kind: None,
                    });
                    return;
                }
            };
            let paths: BTreeSet<_> = known.keys().chain(current.keys()).cloned().collect();
            for path in paths {
                let exists = current.contains_key(&path);
                if known.contains_key(&path) != exists && !reported.contains(&path) {
                    records.push(WatchEventRecord {
                        event: if exists {
                            WatchEvent::Create(path)
                        } else {
                            WatchEvent::Delete(path)
                        },
                        raw_kind: self.reconcile_kind,
                    });
                }
            }
            *known = current;
            return;
        }
        for path in self.display.keys() {
            let exists = path.symlink_metadata().is_ok();
            let previous = known.insert(path.clone(), exists).unwrap_or(exists);
            if previous != exists && !reported.contains(path) {
                records.push(WatchEventRecord {
                    event: if exists {
                        WatchEvent::Create(path.clone())
                    } else {
                        WatchEvent::Delete(path.clone())
                    },
                    raw_kind: self.reconcile_kind,
                });
            }
        }
    }

    fn flush(&mut self) -> Vec<WatchEventRecord> {
        let batch = self.coalescer.drain();
        let mut records = Vec::new();
        for path in batch.changed {
            let provenance = self.provenance.get(&path);
            let event =
                if provenance.is_some_and(|record| matches!(record.event, WatchEvent::Create(_))) {
                    WatchEvent::Create(path)
                } else {
                    WatchEvent::Modify(path)
                };
            records.push(WatchEventRecord {
                event,
                raw_kind: provenance.and_then(|record| record.raw_kind),
            });
        }
        for path in batch.removed {
            let raw_kind = self
                .provenance
                .get(&path)
                .and_then(|record| record.raw_kind);
            records.push(WatchEventRecord {
                event: WatchEvent::Delete(path),
                raw_kind,
            });
        }
        for rename in batch.renamed {
            let raw_kind = self
                .provenance
                .get(&rename.to)
                .and_then(|record| record.raw_kind);
            records.push(WatchEventRecord {
                event: WatchEvent::Rename {
                    from: rename.from,
                    to: rename.to,
                },
                raw_kind,
            });
        }
        self.provenance.clear();
        self.reconcile(&mut records);
        self.reconcile_kind = None;
        records
            .into_iter()
            .flat_map(|record| self.project(record))
            .collect()
    }
}

// Do not follow directory symlinks: recursion must stay inside the watched tree.
fn directory_snapshot(root: &Path, recursive: bool) -> notify::Result<HashMap<PathBuf, bool>> {
    fn visit(
        path: &Path,
        recursive: bool,
        paths: &mut HashMap<PathBuf, bool>,
    ) -> notify::Result<()> {
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(notify::Error::io(error).add_path(path.to_path_buf())),
        };
        paths.insert(path.to_path_buf(), true);
        if !metadata.is_dir() {
            return Ok(());
        }
        let entries = std::fs::read_dir(path)
            .map_err(|error| notify::Error::io(error).add_path(path.to_path_buf()))?;
        for entry in entries {
            let entry = entry.map_err(notify::Error::io)?;
            let child = entry.path();
            if recursive {
                visit(&child, true, paths)?;
            } else {
                paths.insert(child, true);
            }
        }
        Ok(())
    }
    let mut paths = HashMap::new();
    visit(root, recursive, &mut paths)?;
    Ok(paths)
}

fn raw_records(result: notify::Result<Event>) -> Vec<WatchEventRecord> {
    let event = match result {
        Ok(event) => event,
        Err(error) => {
            return vec![WatchEventRecord {
                event: WatchEvent::Error(error.into()),
                raw_kind: None,
            }]
        }
    };
    let raw_kind = event.kind;
    let record = |event| WatchEventRecord {
        event,
        raw_kind: Some(raw_kind),
    };
    let mut records = Vec::new();
    if event.need_rescan() {
        records.push(record(WatchEvent::Overflow));
    }
    if matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
    ) {
        if let [from, to] = event.paths.as_slice() {
            records.push(record(WatchEvent::Rename {
                from: from.clone(),
                to: to.clone(),
            }));
            return records;
        }
    }
    for path in event.paths {
        let typed = match event.kind {
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                WatchEvent::Create(path)
            }
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                WatchEvent::Delete(path)
            }
            EventKind::Modify(_) | EventKind::Any => WatchEvent::Modify(path),
            EventKind::Access(_) | EventKind::Other => continue,
        };
        records.push(record(typed));
    }
    records
}

const STOP_POLL: Duration = Duration::from_millis(50);

fn run<G: GrantCheck>(
    rx: mpsc::Receiver<notify::Result<Event>>,
    tx: mpsc::Sender<Vec<WatchEventRecord>>,
    delivery: Delivery<G>,
    stop: &AtomicBool,
    alive: &AtomicBool,
) {
    run_callback(
        rx,
        |records| tx.send(records).is_ok(),
        delivery,
        stop,
        alive,
    );
}

pub(crate) fn run_callback<G: GrantCheck>(
    rx: mpsc::Receiver<notify::Result<Event>>,
    mut emit: impl FnMut(Vec<WatchEventRecord>) -> bool,
    mut delivery: Delivery<G>,
    stop: &AtomicBool,
    alive: &AtomicBool,
) {
    let mut translator = Translator::default();
    let mut window = DebounceWindow::default();
    let discard = delivery.options.discard_on_disconnect;
    let mut send = |records: Vec<WatchEventRecord>| -> bool {
        if records.is_empty() {
            return true;
        }
        if emit(records) {
            return true;
        }
        if !discard {
            tracing::warn!("rightkit-file-watch consumer disconnected");
        }
        false
    };
    loop {
        if !alive.load(Ordering::Acquire) {
            if !discard {
                tracing::warn!("rightkit-file-watch consumer disconnected");
            }
            return;
        }
        if stop.load(Ordering::Acquire) {
            if !discard {
                for record in translator.flush_records() {
                    delivery.push(record);
                }
                let _ = send(delivery.flush());
            }
            return;
        }
        let now = Instant::now();
        let wait = [
            window.deadline(&delivery.options),
            translator.next_deadline(),
        ]
        .into_iter()
        .flatten()
        .map(|deadline| deadline.saturating_duration_since(now))
        .fold(STOP_POLL, Duration::min);
        let result = rx.recv_timeout(wait);
        let now = Instant::now();
        let received = result.is_ok();
        let records = match result {
            Ok(mut result) => {
                delivery.reconcile_kind = result.as_ref().ok().map(|event| event.kind);
                if let Ok(event) = &mut result {
                    event.paths = event.paths.iter().map(|path| normalize(path)).collect();
                }
                if delivery.options.coalesce {
                    translator.translate_records(result, now)
                } else {
                    raw_records(result)
                }
            }
            Err(RecvTimeoutError::Timeout) => translator.expire_records(now),
            Err(RecvTimeoutError::Disconnected) => {
                if !discard {
                    for record in translator.flush_records() {
                        delivery.push(record);
                    }
                    let _ = send(delivery.flush());
                }
                return;
            }
        };
        let mut observed = Vec::new();
        let mut gap = false;
        for record in records {
            if matches!(record.event, WatchEvent::Error(_) | WatchEvent::Overflow) {
                gap = true;
                if !send(delivery.project(record)) {
                    return;
                }
            } else if delivery.options.coalesce {
                delivery.push(record);
                window.observe(now);
            } else {
                delivery.reconcile_kind = record.raw_kind;
                observed.push(record.clone());
                if !send(delivery.project(record)) {
                    return;
                }
            }
        }
        if gap && delivery.options.coalesce && delivery.options.reconcile {
            // Rescan on gaps, but keep mutation delivery behind the quiet/cap policy.
            let mut reconciled = Vec::new();
            delivery.reconcile(&mut reconciled);
            for record in reconciled {
                if matches!(record.event, WatchEvent::Error(_) | WatchEvent::Overflow) {
                    if !send(delivery.project(record)) {
                        return;
                    }
                } else {
                    delivery.push(record);
                    window.observe(now);
                }
            }
        }
        if received && !delivery.options.coalesce && delivery.options.reconcile {
            let reported = observed.len();
            delivery.reconcile(&mut observed);
            delivery.reconcile_kind = None;
            let records = observed
                .into_iter()
                .skip(reported)
                .flat_map(|record| delivery.project(record))
                .collect();
            if !send(records) {
                return;
            }
        }
        if window
            .deadline(&delivery.options)
            .is_some_and(|deadline| deadline <= now)
        {
            if !send(delivery.flush()) {
                return;
            }
            window = DebounceWindow::default();
        }
    }
}
