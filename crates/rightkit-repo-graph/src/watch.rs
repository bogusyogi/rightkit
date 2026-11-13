//! Native, polling-first Blueprint watcher and filesystem reconciler.
//!
//! Events are hints. A bounded deterministic snapshot is truth, so a dropped
//! or coalesced callback cannot make an index appear current indefinitely.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, Metadata};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_file_watch::{FileWatcher, TypedWatcher, WatchEvent as BackendWatchEvent};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::api::CancellationToken;
use crate::contracts::BarrierResult;
use crate::freshness::{
    content_digest, stable_read_with_limit, StableReadError, MAX_SOURCE_FILE_BYTES,
};
use crate::graph::RepoIgnore;

pub const DEFAULT_MAX_FILES: usize = 100_000;
pub const DEFAULT_MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
/// Retained for wire/config compatibility. Native events are drained as soon
/// as supervision runs; known paths never wait for this interval.
pub const DEFAULT_DEBOUNCE_MS: u64 = 1_000;
pub const DEBOUNCE_MS: u64 = DEFAULT_DEBOUNCE_MS;
pub const MAX_DEBOUNCE_MS: u64 = 5_000;

#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    pub root: PathBuf,
    pub exclusions: BTreeSet<String>,
    pub max_files: usize,
    pub max_bytes: u64,
    pub max_file_bytes: u64,
    pub max_events: usize,
    pub debounce_ms: u64,
    /// Minimum wall-clock interval between full snapshot reconciliations when
    /// the native event queue is quiet. Zero runs a snapshot on every poll.
    /// The snapshot reads and digests every source file, so a supervised
    /// resident must not run it on every supervisor tick.
    pub snapshot_interval_ms: u64,
    /// Backstop interval used instead of `snapshot_interval_ms` while native
    /// events are healthy (no gap). Zero or smaller keeps `snapshot_interval_ms`.
    pub healthy_snapshot_interval_ms: u64,
}

impl SnapshotConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            exclusions: BTreeSet::new(),
            max_files: DEFAULT_MAX_FILES,
            max_bytes: DEFAULT_MAX_SNAPSHOT_BYTES,
            max_file_bytes: MAX_SOURCE_FILE_BYTES,
            max_events: 256,
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            snapshot_interval_ms: 0,
            healthy_snapshot_interval_ms: 0,
        }
    }

    pub fn exclude(mut self, path: impl AsRef<Path>) -> Self {
        if let Some(path) = normalized_relative(path.as_ref()) {
            self.exclusions.insert(path);
        }
        self
    }

    pub fn max_events(mut self, max_events: usize) -> Self {
        self.max_events = max_events.max(1);
        self
    }

    pub fn debounce_ms(mut self, debounce_ms: u64) -> Self {
        self.debounce_ms = debounce_ms.min(MAX_DEBOUNCE_MS);
        self
    }

    pub fn debounce(self, debounce: Duration) -> Self {
        self.debounce_ms(debounce.as_millis().min(u64::MAX as u128) as u64)
    }

    pub fn snapshot_interval_ms(mut self, snapshot_interval_ms: u64) -> Self {
        self.snapshot_interval_ms = snapshot_interval_ms;
        self
    }

    pub fn healthy_snapshot_interval_ms(mut self, interval_ms: u64) -> Self {
        self.healthy_snapshot_interval_ms = interval_ms;
        self
    }
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("snapshot cancelled")]
    Cancelled,
    #[error("snapshot root is unavailable: {0}")]
    Root(#[from] std::io::Error),
    #[error("snapshot escaped repository root: {0}")]
    Escaped(PathBuf),
    #[error("snapshot limit exceeded: {kind} ({actual}, limit {limit})")]
    Limit {
        kind: &'static str,
        actual: u64,
        limit: u64,
    },
    #[error("stable read failed for {path}: {source}")]
    Read {
        path: PathBuf,
        source: StableReadError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified_ns: Option<u128>,
    pub digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub entries: Vec<SnapshotEntry>,
    pub fingerprint: String,
}

fn canonical_root(root: &Path) -> Result<PathBuf, SnapshotError> {
    root.canonicalize().map_err(SnapshotError::Root)
}

fn normalized_relative(path: &Path) -> Option<String> {
    let mut result = String::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if !result.is_empty() {
                    result.push('/');
                }
                result.push_str(&value.to_string_lossy().replace('\\', "/"));
            }
            Component::CurDir => {}
            Component::ParentDir => return None,
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!result.is_empty()).then_some(result)
}

fn excluded(relative: &str, config: &SnapshotConfig) -> bool {
    config
        .exclusions
        .iter()
        .any(|prefix| relative == prefix || relative.starts_with(&format!("{prefix}/")))
}

/// Build/output folders the host app file index also skips; the shared policy already covers
/// target, node_modules, .git, build, dist and __pycache__, so only the remainder is added.
fn dir_skipped(ignore: &RepoIgnore, relative: &str, name: &str) -> bool {
    matches!(name, ".gradle" | ".venv") || ignore.dir_ignored(relative, name)
}

fn confined(root: &Path, path: &Path) -> Result<bool, SnapshotError> {
    let canonical = match path.canonicalize() {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(SnapshotError::Root(error)),
    };
    Ok(canonical == root || canonical.starts_with(root))
}

fn metadata_modified_ns(metadata: &Metadata) -> Option<u128> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()
        .map(|v| v.as_nanos())
}

#[allow(clippy::too_many_arguments)]
fn walk(
    root: &Path,
    current: &Path,
    config: &SnapshotConfig,
    ignore: &RepoIgnore,
    output: &mut Vec<SnapshotEntry>,
    bytes: &mut u64,
    digest: bool,
    reuse: Option<&Reuse>,
    cancellation: &CancellationToken,
) -> Result<(), SnapshotError> {
    if cancellation.is_cancelled() {
        return Err(SnapshotError::Cancelled);
    }
    let mut children = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|left| left.file_name());
    for child in children {
        if cancellation.is_cancelled() {
            return Err(SnapshotError::Cancelled);
        }
        let path = child.path();
        let relative = path.strip_prefix(root).ok().and_then(normalized_relative);
        let Some(relative) = relative else {
            continue;
        };
        if excluded(&relative, config) {
            continue;
        }
        if !confined(root, &path)? {
            continue;
        }
        let file_type = child.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let metadata = child.metadata()?;
        if file_type.is_dir() {
            if dir_skipped(ignore, &relative, &child.file_name().to_string_lossy()) {
                continue;
            }
            if output.len() as u64 >= config.max_files as u64 {
                return Err(SnapshotError::Limit {
                    kind: "entries",
                    actual: output.len() as u64 + 1,
                    limit: config.max_files as u64,
                });
            }
            output.push(SnapshotEntry {
                path: relative.clone(),
                kind: EntryKind::Directory,
                size: 0,
                modified_ns: metadata_modified_ns(&metadata),
                digest: None,
                reason: None,
            });
            walk(
                root,
                &path,
                config,
                ignore,
                output,
                bytes,
                digest,
                reuse,
                cancellation,
            )?;
        } else if file_type.is_file() {
            if output.len() as u64 >= config.max_files as u64 {
                return Err(SnapshotError::Limit {
                    kind: "entries",
                    actual: output.len() as u64 + 1,
                    limit: config.max_files as u64,
                });
            }
            if ignore.file_ignored(&relative, &child.file_name().to_string_lossy()) {
                continue;
            }
            if metadata.len() > config.max_file_bytes {
                output.push(SnapshotEntry {
                    path: relative,
                    kind: EntryKind::File,
                    size: metadata.len(),
                    modified_ns: metadata_modified_ns(&metadata),
                    digest: None,
                    reason: Some(format!(
                        "unsupported:file_bytes:{}>limit:{}",
                        metadata.len(),
                        config.max_file_bytes
                    )),
                });
                continue;
            }
            *bytes = bytes.saturating_add(metadata.len());
            if *bytes > config.max_bytes {
                return Err(SnapshotError::Limit {
                    kind: "snapshot_bytes",
                    actual: *bytes,
                    limit: config.max_bytes,
                });
            }
            if !digest {
                // Size-only pass (#435): the limits above are checked from metadata alone, so
                // an over-limit tree is refused before any file is read or hashed.
                output.push(SnapshotEntry {
                    path: relative,
                    kind: EntryKind::File,
                    size: metadata.len(),
                    modified_ns: None,
                    digest: None,
                    reason: None,
                });
                continue;
            }
            // Reuse the previous digest only when size and mtime are unchanged and the mtime
            // was already older than the racy window when the previous digest was read (git's
            // racy rule: compare against the previous read, not the current snapshot).
            let cached =
                reuse
                    .zip(metadata_modified_ns(&metadata))
                    .and_then(|(reuse, modified)| {
                        let prev = reuse.entries.get(relative.as_str())?;
                        (modified.saturating_add(REUSE_RACY_WINDOW_NS) < reuse.prev_started_ns
                            && prev.kind == EntryKind::File
                            && prev.reason.is_none()
                            && prev.size == metadata.len()
                            && prev.modified_ns == Some(modified))
                        .then(|| prev.digest.clone())
                        .flatten()
                        .map(|digest| (modified, digest))
                    });
            if let Some((modified, digest)) = cached {
                output.push(SnapshotEntry {
                    path: relative,
                    kind: EntryKind::File,
                    size: metadata.len(),
                    modified_ns: Some(modified),
                    digest: Some(digest),
                    reason: None,
                });
                continue;
            }
            let read = stable_read_with_limit(&path, config.max_file_bytes).map_err(|source| {
                SnapshotError::Read {
                    path: relative.clone().into(),
                    source,
                }
            })?;
            if cancellation.is_cancelled() {
                return Err(SnapshotError::Cancelled);
            }
            output.push(SnapshotEntry {
                path: relative,
                kind: EntryKind::File,
                size: read.bytes.len() as u64,
                modified_ns: metadata_modified_ns(&metadata),
                digest: Some(read.content_digest),
                reason: None,
            });
        }
    }
    Ok(())
}

const REUSE_RACY_WINDOW_NS: u128 = 2_000_000_000;

struct Reuse<'a> {
    entries: BTreeMap<&'a str, &'a SnapshotEntry>,
    /// Wall-clock start (ns since epoch) of the snapshot that produced `entries`; 0 = unknown.
    prev_started_ns: u128,
}

/// The snapshot's limits (entries, total bytes) checked from directory listings and file
/// metadata alone: no file is read or hashed. The resident's build refuses a root with this
/// before building when the caller asks for it, with the same limits the watcher applies.
pub fn check_limits(
    config: &SnapshotConfig,
    cancellation: &CancellationToken,
) -> Result<(), SnapshotError> {
    let root = canonical_root(&config.root)?;
    let ignore = RepoIgnore::for_root(&root);
    walk(
        &root,
        &root,
        config,
        &ignore,
        &mut Vec::new(),
        &mut 0,
        false,
        None,
        cancellation,
    )
}

pub fn snapshot(config: &SnapshotConfig) -> Result<Snapshot, SnapshotError> {
    snapshot_with_cancellation(config, &CancellationToken::new())
}

pub fn snapshot_with_cancellation(
    config: &SnapshotConfig,
    cancellation: &CancellationToken,
) -> Result<Snapshot, SnapshotError> {
    snapshot_reusing(config, cancellation, None, 0)
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn snapshot_reusing(
    config: &SnapshotConfig,
    cancellation: &CancellationToken,
    previous: Option<&Snapshot>,
    previous_started_ns: u128,
) -> Result<Snapshot, SnapshotError> {
    let reuse = previous.map(|snapshot| Reuse {
        entries: snapshot
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect(),
        prev_started_ns: previous_started_ns,
    });
    let root = canonical_root(&config.root)?;
    let ignore = RepoIgnore::for_root(&root);
    // Metadata-only pre-walk (#435): an over-limit tree used to be read and digested up to the
    // byte limit before it was refused, on every retry. This pass costs directory listings only.
    walk(
        &root,
        &root,
        config,
        &ignore,
        &mut Vec::new(),
        &mut 0,
        false,
        None,
        cancellation,
    )?;
    let mut entries = Vec::new();
    let mut bytes = 0;
    walk(
        &root,
        &root,
        config,
        &ignore,
        &mut entries,
        &mut bytes,
        true,
        reuse.as_ref(),
        cancellation,
    )?;
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut canonical = Vec::new();
    for entry in &entries {
        canonical.extend_from_slice(entry.path.as_bytes());
        canonical.push(0);
        canonical.extend_from_slice(
            format!(
                "{:?}:{}:{}:{}:{}\n",
                entry.kind,
                entry.size,
                entry.modified_ns.unwrap_or(0),
                entry.digest.as_deref().unwrap_or(""),
                entry.reason.as_deref().unwrap_or("")
            )
            .as_bytes(),
        );
    }
    Ok(Snapshot {
        entries,
        fingerprint: content_digest(&canonical),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Create,
    Modify,
    Delete,
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchEvent {
    pub kind: EventKind,
    pub path: String,
    pub rename_to: Option<String>,
    pub source_clock: u64,
}

#[derive(Default)]
struct NativeEventQueue {
    events: Vec<WatchEvent>,
    keys: BTreeSet<(EventKind, String, Option<String>)>,
    next_source_clock: u64,
    overflow: bool,
    overflow_count: usize,
}

impl NativeEventQueue {
    fn enqueue(&mut self, mut event: WatchEvent, max_events: usize) {
        if event.rename_to.is_none() {
            if let Some(existing) = self
                .events
                .iter_mut()
                .find(|existing| existing.path == event.path && existing.rename_to.is_none())
            {
                let old_key = (existing.kind, existing.path.clone(), None);
                let merged_kind = match (existing.kind, event.kind) {
                    (EventKind::Create, EventKind::Modify)
                    | (EventKind::Create, EventKind::Create) => EventKind::Create,
                    (_, EventKind::Create) => EventKind::Create,
                    (_, EventKind::Delete) => EventKind::Delete,
                    _ => EventKind::Modify,
                };
                existing.kind = merged_kind;
                self.keys.remove(&old_key);
                self.keys.insert((merged_kind, existing.path.clone(), None));
                return;
            }
        }
        let key = (event.kind, event.path.clone(), event.rename_to.clone());
        if !self.keys.insert(key) {
            return;
        }
        if self.events.len() >= max_events {
            self.overflow = true;
            self.overflow_count = self.overflow_count.saturating_add(1);
            return;
        }
        self.next_source_clock = self.next_source_clock.saturating_add(1);
        event.source_clock = self.next_source_clock;
        self.events.push(event);
    }
}

fn normalize_watch_path(root: &Path, path: &Path) -> Option<String> {
    let relative = if path.is_absolute() {
        path.strip_prefix(root).ok()?
    } else {
        path
    };
    normalized_relative(relative)
}

fn enqueue_backend_event(
    queue: &mut NativeEventQueue,
    root: &Path,
    max_events: usize,
    event: BackendWatchEvent,
) {
    // Backend errors and rescan notices mean events were lost; the snapshot
    // reconciliation path must run before freshness is reported.
    let event = match event {
        BackendWatchEvent::Error(_) | BackendWatchEvent::Overflow => {
            queue.overflow = true;
            queue.overflow_count = queue.overflow_count.saturating_add(1);
            return;
        }
        event => event,
    };
    let mut enqueue = |kind: EventKind, path: &Path, rename_to: Option<String>| {
        if let Some(path) = normalize_watch_path(root, path) {
            queue.enqueue(
                WatchEvent {
                    kind,
                    path,
                    rename_to,
                    source_clock: 0,
                },
                max_events,
            );
        }
    };
    match event {
        BackendWatchEvent::Create(path) => enqueue(EventKind::Create, &path, None),
        BackendWatchEvent::Delete(path) => enqueue(EventKind::Delete, &path, None),
        BackendWatchEvent::Modify(path) => enqueue(EventKind::Modify, &path, None),
        BackendWatchEvent::Rename { from, to } => {
            enqueue(EventKind::Rename, &from, normalize_watch_path(root, &to));
        }
        BackendWatchEvent::Error(_) | BackendWatchEvent::Overflow => {}
    }
}

/// BPT-020/021 (selective invalidation): this reconciler is what makes a
/// watcher publish *selective* rather than a full-generation rebuild -- it
/// diffs the previous and current snapshot per path and emits exactly the
/// Create/Modify/Delete events for entries whose digest/kind/size actually
/// changed. What this module does NOT own: turning those per-path events
/// into a dependency-DAG invalidation over source/provider/config/schema/
/// generation parents (declared elsewhere in Blueprint's build/derivation
/// path) -- that projection consumes these events but is out of this
/// module's scope.
pub fn reconcile_snapshots(
    previous: &Snapshot,
    current: &Snapshot,
    source_clock: u64,
) -> Vec<WatchEvent> {
    // Directory metadata is useful for deterministic snapshot identity, but a
    // rebuild callback is only meaningful for source files.
    let old = previous
        .entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let new = current
        .entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut events = Vec::new();
    for path in old
        .keys()
        .chain(new.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        match (old.get(path), new.get(path)) {
            (None, Some(_)) => events.push(WatchEvent {
                kind: EventKind::Create,
                path: path.to_owned(),
                rename_to: None,
                source_clock: source_clock + events.len() as u64 + 1,
            }),
            (Some(_), None) => events.push(WatchEvent {
                kind: EventKind::Delete,
                path: path.to_owned(),
                rename_to: None,
                source_clock: source_clock + events.len() as u64 + 1,
            }),
            (Some(before), Some(after)) if before != after => events.push(WatchEvent {
                kind: EventKind::Modify,
                path: path.to_owned(),
                rename_to: None,
                source_clock: source_clock + events.len() as u64 + 1,
            }),
            _ => {}
        }
    }
    events
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    EventOverflow,
    SnapshotFailed,
    ReconciliationFailed,
    CallbackFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventGap {
    pub reason: GapReason,
    pub source_clock: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierPoll {
    Waiting,
    Complete(BarrierResult),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Barrier {
    pub target_source_clock: u64,
    pub deadline: Option<Duration>,
}

pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> Duration;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SteadyClock {
    started: Option<Instant>,
}

impl SteadyClock {
    pub fn new() -> Self {
        Self {
            started: Some(Instant::now()),
        }
    }
}
impl MonotonicClock for SteadyClock {
    fn now(&self) -> Duration {
        self.started
            .map(|start| start.elapsed())
            .unwrap_or_default()
    }
}

pub trait LifecycleSink: Send + Sync {
    fn observe(&self, observation: LifecycleObservation);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleObservation {
    pub sequence: u64,
    pub kind: String,
    pub source_clock: u64,
    pub applied_clock: u64,
    pub detail: Option<String>,
}

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("watcher is shut down")]
    Shutdown,
    #[error("snapshot failed: {0}")]
    Snapshot(#[from] SnapshotError),
    #[error("rebuild callback failed: {0}")]
    Callback(String),
    #[error("watcher event batch exceeded limit: {actual} (limit {limit})")]
    EventOverflow { actual: usize, limit: usize },
    #[error("native watcher unavailable: {0}")]
    Native(String),
}

pub struct NativeWatcher {
    config: SnapshotConfig,
    previous: Snapshot,
    /// Wall-clock start of the walk that produced `previous` (git racy-timestamp reference).
    previous_started_ns: u128,
    source_clock: u64,
    applied_clock: u64,
    gap: Option<EventGap>,
    closed: bool,
    sequence: u64,
    clock: Arc<dyn MonotonicClock>,
    sink: Option<Arc<dyn LifecycleSink>>,
    ignore: RepoIgnore,
    native_queue: Arc<Mutex<NativeEventQueue>>,
    last_snapshot_at: Option<Instant>,
    last_git_head_probe_at: Option<Instant>,
    git_head_marker: Option<GitMarker>,
    /// Failed-snapshot backoff (#435): consecutive failures, when the next attempt is allowed,
    /// and the recorded reason, so a failing tree is not re-walked on every poll.
    snapshot_failures: u32,
    snapshot_retry_at: Option<Instant>,
    last_snapshot_failure: Option<String>,
    previous_git_head: Option<String>,
    _native_watcher: TypedWatcher,
}

/// Cheap mtime fingerprint of the files a HEAD change touches: HEAD, packed-refs, refs/heads
/// and each immediate subdirectory of refs/heads (nested branches such as feature/x).
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitMarker(
    [Option<std::time::SystemTime>; 3],
    Vec<Option<std::time::SystemTime>>,
);

/// Resolve `<root>/.git`: a directory, or a linked-worktree file `gitdir: <path>`.
fn resolve_git_dir(root: &Path) -> PathBuf {
    let dot_git = root.join(".git");
    if dot_git.is_file() {
        if let Ok(text) = fs::read_to_string(&dot_git) {
            if let Some(target) = text.lines().find_map(|l| l.trim().strip_prefix("gitdir:")) {
                let target = Path::new(target.trim());
                return if target.is_absolute() {
                    target.to_path_buf()
                } else {
                    root.join(target)
                };
            }
        }
    }
    dot_git
}

fn git_marker(root: &Path) -> Option<GitMarker> {
    let git = resolve_git_dir(root);
    let mtime = |p: PathBuf| fs::metadata(p).and_then(|m| m.modified()).ok();
    // Branch refs and packed-refs live in the common dir of a linked worktree; HEAD is per-worktree.
    let common = fs::read_to_string(git.join("commondir"))
        .ok()
        .map(|c| {
            let c = PathBuf::from(c.trim());
            if c.is_absolute() {
                c
            } else {
                git.join(c)
            }
        })
        .unwrap_or_else(|| git.clone());
    let heads = common.join("refs").join("heads");
    let marker = [
        mtime(git.join("HEAD")),
        mtime(heads.clone()),
        mtime(common.join("packed-refs")),
    ];
    let mut nested: Vec<(std::ffi::OsString, Option<std::time::SystemTime>)> = fs::read_dir(&heads)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| (e.file_name(), e.metadata().and_then(|m| m.modified()).ok()))
                .collect()
        })
        .unwrap_or_default();
    nested.sort_by(|a, b| a.0.cmp(&b.0));
    let nested = nested.into_iter().map(|(_, t)| t).collect();
    marker[0].map(|_| GitMarker(marker, nested))
}

impl NativeWatcher {
    /// Backstop interval: the long healthy interval while native events have
    /// no gap, the base interval while degraded/overflowed.
    fn backstop_interval(&self) -> Duration {
        let base = self.config.snapshot_interval_ms;
        let ms = if self.gap.is_none() {
            base.max(self.config.healthy_snapshot_interval_ms)
        } else {
            base
        };
        Duration::from_millis(ms)
    }

    pub fn start(config: SnapshotConfig) -> Result<Self, WatchError> {
        Self::start_with_cancellation(config, &CancellationToken::new())
    }

    pub fn start_with_cancellation(
        config: SnapshotConfig,
        cancellation: &CancellationToken,
    ) -> Result<Self, WatchError> {
        let root = canonical_root(&config.root)?;
        let mut config = config;
        config.root = root.clone();
        let initial_started_ns = now_ns();
        let initial = snapshot_with_cancellation(&config, cancellation)?;
        let queue = Arc::new(Mutex::new(NativeEventQueue::default()));
        let native_watcher = FileWatcher::watch_typed(&root)
            .map_err(|error| WatchError::Native(error.to_string()))?;
        let previous_git_head =
            crate::git_source_observation::git_base_commit(&root.to_string_lossy());
        let mut watcher = Self {
            config,
            previous: initial,
            previous_started_ns: initial_started_ns,
            source_clock: 0,
            applied_clock: 0,
            gap: None,
            closed: false,
            sequence: 0,
            clock: Arc::new(SteadyClock::new()),
            sink: None,
            ignore: RepoIgnore::for_root(&root),
            native_queue: queue,
            last_snapshot_at: Some(Instant::now()),
            last_git_head_probe_at: None,
            git_head_marker: git_marker(&root),
            snapshot_failures: 0,
            snapshot_retry_at: None,
            last_snapshot_failure: None,
            previous_git_head,
            _native_watcher: native_watcher,
        };
        watcher.emit("started", None);
        Ok(watcher)
    }

    pub fn with_clock(mut self, clock: Arc<dyn MonotonicClock>) -> Self {
        self.clock = clock;
        self
    }
    pub fn with_sink(mut self, sink: Arc<dyn LifecycleSink>) -> Self {
        self.sink = Some(sink);
        self
    }
    pub fn new(config: SnapshotConfig) -> Result<Self, WatchError> {
        Self::start(config)
    }
    pub fn source_clock(&self) -> u64 {
        self.source_clock
    }
    pub fn applied_clock(&self) -> u64 {
        self.applied_clock
    }
    pub fn gap(&self) -> Option<&EventGap> {
        self.gap.as_ref()
    }
    /// Reason of the last failed snapshot while backing off (#435).
    pub fn last_snapshot_failure(&self) -> Option<&str> {
        self.last_snapshot_failure.as_deref()
    }
    pub fn is_shutdown(&self) -> bool {
        self.closed
    }
    pub fn root(&self) -> &Path {
        &self.config.root
    }
    pub fn debounce(&self) -> Duration {
        Duration::from_millis(self.config.debounce_ms)
    }

    /// Drain native events immediately. The debounce setting remains part of
    /// config compatibility, but native known-path events do not sleep.
    pub fn poll_debounced<F>(
        &mut self,
        schedule_rebuild: F,
        cancellation: &CancellationToken,
    ) -> Result<Vec<WatchEvent>, WatchError>
    where
        F: FnMut(&WatchEvent) -> Result<(), String>,
    {
        self.poll_with_cancellation(schedule_rebuild, cancellation)
    }

    /// Polling is the reconciliation truth. A callback is only a rebuild hint;
    /// it cannot approve Phase 2 or mutate semantic policy.
    pub fn poll<F>(&mut self, schedule_rebuild: F) -> Result<Vec<WatchEvent>, WatchError>
    where
        F: FnMut(&WatchEvent) -> Result<(), String>,
    {
        self.poll_with_cancellation(schedule_rebuild, &CancellationToken::new())
    }

    fn native_event_is_source(&self, event: &WatchEvent) -> bool {
        let path_is_source = |path: &str| {
            let mut relative_parent = String::new();
            let components = Path::new(path)
                .components()
                .filter_map(|component| match component {
                    Component::Normal(value) => value.to_str(),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for component in components.iter().take(components.len().saturating_sub(1)) {
                if !relative_parent.is_empty() {
                    relative_parent.push('/');
                }
                relative_parent.push_str(component);
                if dir_skipped(&self.ignore, &relative_parent, component) {
                    return false;
                }
            }
            let name = Path::new(path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if self.ignore.file_ignored(path, name) {
                return false;
            }
            match fs::symlink_metadata(self.config.root.join(path)) {
                Ok(metadata) => metadata.is_file(),
                Err(_) => self
                    .previous
                    .entries
                    .iter()
                    .any(|entry| entry.path == path && entry.kind == EntryKind::File),
            }
        };
        path_is_source(&event.path) || event.rename_to.as_deref().is_some_and(path_is_source)
    }

    fn take_native_events(&mut self) -> Result<(bool, Vec<WatchEvent>), WatchError> {
        let Ok(mut queue) = self.native_queue.lock() else {
            self.gap = Some(EventGap {
                reason: GapReason::EventOverflow,
                source_clock: self.source_clock,
            });
            return Err(WatchError::EventOverflow {
                actual: self.config.max_events.saturating_add(1),
                limit: self.config.max_events,
            });
        };
        // The backend stream is unbounded; drain it into the bounded queue so
        // overflow is judged at the same limit as before.
        while let Ok(event) = self._native_watcher.try_recv() {
            enqueue_backend_event(&mut queue, &self.config.root, self.config.max_events, event);
        }
        if queue.overflow {
            let actual = queue
                .events
                .len()
                .saturating_add(queue.overflow_count)
                .max(self.config.max_events.saturating_add(1));
            queue.events.clear();
            queue.keys.clear();
            queue.overflow = false;
            queue.overflow_count = 0;
            drop(queue);
            self.gap = Some(EventGap {
                reason: GapReason::EventOverflow,
                source_clock: self.source_clock,
            });
            self.emit(
                "event_overflow",
                Some(format!("{actual}>{}", self.config.max_events)),
            );
            return Err(WatchError::EventOverflow {
                actual,
                limit: self.config.max_events,
            });
        }
        let events = std::mem::take(&mut queue.events);
        queue.keys.clear();
        drop(queue);
        let observed = !events.is_empty();
        let events = events
            .into_iter()
            .filter(|event| self.native_event_is_source(event))
            .collect();
        Ok((observed, events))
    }

    fn apply_known_event(&mut self, event: &WatchEvent) {
        let remove = |entries: &mut Vec<SnapshotEntry>, path: &str| {
            entries
                .retain(|entry| entry.path != path && !entry.path.starts_with(&format!("{path}/")));
        };
        remove(&mut self.previous.entries, &event.path);
        if let Some(rename_to) = &event.rename_to {
            remove(&mut self.previous.entries, rename_to);
        }
        if matches!(event.kind, EventKind::Delete) {
            return;
        }
        let path = event.rename_to.as_deref().unwrap_or(&event.path);
        let full_path = self.config.root.join(path);
        let Ok(metadata) = fs::symlink_metadata(&full_path) else {
            return;
        };
        let name = Path::new(path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let entry = if metadata.is_dir() {
            if dir_skipped(&self.ignore, path, name) {
                return;
            }
            SnapshotEntry {
                path: path.to_owned(),
                kind: EntryKind::Directory,
                size: 0,
                modified_ns: metadata_modified_ns(&metadata),
                digest: None,
                reason: None,
            }
        } else if metadata.is_file() {
            if self.ignore.file_ignored(path, name) {
                return;
            }
            if metadata.len() > self.config.max_file_bytes {
                SnapshotEntry {
                    path: path.to_owned(),
                    kind: EntryKind::File,
                    size: metadata.len(),
                    modified_ns: metadata_modified_ns(&metadata),
                    digest: None,
                    reason: Some(format!(
                        "unsupported:file_bytes:{}>limit:{}",
                        metadata.len(),
                        self.config.max_file_bytes
                    )),
                }
            } else {
                let Ok(read) = stable_read_with_limit(&full_path, self.config.max_file_bytes)
                else {
                    return;
                };
                SnapshotEntry {
                    path: path.to_owned(),
                    kind: EntryKind::File,
                    size: read.bytes.len() as u64,
                    modified_ns: metadata_modified_ns(&metadata),
                    digest: Some(read.content_digest),
                    reason: None,
                }
            }
        } else {
            return;
        };
        self.previous.entries.push(entry);
        self.previous
            .entries
            .sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    }

    fn publish_native_events<F>(
        &mut self,
        events: Vec<WatchEvent>,
        mut schedule_rebuild: F,
    ) -> Result<Vec<WatchEvent>, WatchError>
    where
        F: FnMut(&WatchEvent) -> Result<(), String>,
    {
        if events.len() > self.config.max_events {
            self.gap = Some(EventGap {
                reason: GapReason::EventOverflow,
                source_clock: self.source_clock,
            });
            return Err(WatchError::EventOverflow {
                actual: events.len(),
                limit: self.config.max_events,
            });
        }
        for event in &events {
            self.source_clock = self.source_clock.max(event.source_clock);
            if let Err(error) = schedule_rebuild(event) {
                self.gap = Some(EventGap {
                    reason: GapReason::CallbackFailed,
                    source_clock: self.source_clock,
                });
                self.emit("callback_failed", Some(error.clone()));
                return Err(WatchError::Callback(error));
            }
            self.emit("rebuild_scheduled", Some(event.path.clone()));
        }
        for event in &events {
            self.apply_known_event(event);
        }
        self.applied_clock = self.source_clock;
        if self.gap.is_none() {
            self.emit("caught_up", None);
        }
        Ok(events)
    }

    pub fn poll_with_cancellation<F>(
        &mut self,
        schedule_rebuild: F,
        cancellation: &CancellationToken,
    ) -> Result<Vec<WatchEvent>, WatchError>
    where
        F: FnMut(&WatchEvent) -> Result<(), String>,
    {
        self.poll_inner(schedule_rebuild, cancellation, false)
    }

    fn poll_inner<F>(
        &mut self,
        mut schedule_rebuild: F,
        cancellation: &CancellationToken,
        force_snapshot: bool,
    ) -> Result<Vec<WatchEvent>, WatchError>
    where
        F: FnMut(&WatchEvent) -> Result<(), String>,
    {
        if self.closed {
            return Err(WatchError::Shutdown);
        }
        if cancellation.is_cancelled() {
            self.emit("poll_cancelled", None);
            return Err(WatchError::Snapshot(SnapshotError::Cancelled));
        }
        let (native_events_observed, native_events) = self.take_native_events()?;
        let now = Instant::now();
        let backstop = self.backstop_interval();
        let marker = git_marker(&self.config.root);
        let marker_changed = marker != self.git_head_marker;
        self.git_head_marker = marker;
        let probe_due = marker_changed
            || self
                .last_git_head_probe_at
                .is_none_or(|at| now.duration_since(at) >= backstop);
        let should_probe_git_head = probe_due;
        let current_git_head = if should_probe_git_head {
            self.last_git_head_probe_at = Some(now);
            crate::git_source_observation::git_base_commit_bounded(
                &self.config.root.to_string_lossy(),
                Duration::from_millis(100),
            )
        } else {
            self.previous_git_head.clone()
        };
        let git_head_changed =
            current_git_head.is_some() && current_git_head != self.previous_git_head;
        let mut native_events = native_events;
        if git_head_changed {
            native_events.push(WatchEvent {
                kind: EventKind::Modify,
                path: ".git/HEAD".into(),
                rename_to: None,
                source_clock: self
                    .source_clock
                    .saturating_add(native_events.len() as u64 + 1),
            });
        }
        // Notifications for ignored/generated paths are still observations,
        // but they do not identify a source event. Keep the snapshot drift
        // backstop active when filtering removed every queued event; otherwise
        // an unrelated `.agent`/directory notification can suppress the
        // reconciliation that discovers a missed source edit.
        let snapshot_due = force_snapshot
            || self
                .last_snapshot_at
                .is_none_or(|at| at.elapsed() >= backstop);
        if (native_events_observed || git_head_changed)
            && !native_events.is_empty()
            && (!snapshot_due || git_head_changed)
        {
            let result = self.publish_native_events(native_events, schedule_rebuild);
            if result.is_ok() && git_head_changed {
                self.previous_git_head = current_git_head;
            }
            return result;
        }
        // A full snapshot reads and digests every source file; on a quiet
        // native queue it is the drift backstop, not the per-tick path.
        // Supervised residents configure a real interval so each supervisor
        // pass stays cheap; an explicit reconcile still forces one.
        if !force_snapshot
            && self
                .last_snapshot_at
                .is_some_and(|at| at.elapsed() < backstop)
        {
            return Ok(Vec::new());
        }
        if !force_snapshot && self.snapshot_retry_at.is_some_and(|at| Instant::now() < at) {
            // Backing off after a failed snapshot (#435); the gap stays set so freshness
            // remains reported as stale, but the tree is not walked again yet.
            return Ok(Vec::new());
        }
        let started_ns = now_ns();
        let current = match snapshot_reusing(
            &self.config,
            cancellation,
            Some(&self.previous),
            self.previous_started_ns,
        ) {
            Ok(current) => {
                self.last_snapshot_at = Some(Instant::now());
                self.snapshot_failures = 0;
                self.snapshot_retry_at = None;
                self.last_snapshot_failure = None;
                current
            }
            Err(SnapshotError::Cancelled) => {
                self.emit("poll_cancelled", None);
                return Err(WatchError::Snapshot(SnapshotError::Cancelled));
            }
            Err(error) => {
                self.gap = Some(EventGap {
                    reason: GapReason::SnapshotFailed,
                    source_clock: self.source_clock,
                });
                // Exponential backoff: 30 s doubling to a 30 min cap; reason is recorded.
                self.snapshot_failures = self.snapshot_failures.saturating_add(1);
                let delay_s = 30u64
                    .saturating_mul(1u64 << (self.snapshot_failures - 1).min(6))
                    .min(1800);
                self.snapshot_retry_at = Some(Instant::now() + Duration::from_secs(delay_s));
                let reason = error.to_string();
                self.last_snapshot_failure = Some(reason.clone());
                self.emit(
                    "snapshot_failed",
                    Some(format!(
                        "{reason}; failures={}; next_retry_s={delay_s}",
                        self.snapshot_failures
                    )),
                );
                return Err(WatchError::Snapshot(error));
            }
        };
        let events = reconcile_snapshots(&self.previous, &current, self.source_clock);
        if events.len() > self.config.max_events {
            self.gap = Some(EventGap {
                reason: GapReason::EventOverflow,
                source_clock: self.source_clock,
            });
            self.emit(
                "event_overflow",
                Some(format!("{}>{}", events.len(), self.config.max_events)),
            );
            return Err(WatchError::EventOverflow {
                actual: events.len(),
                limit: self.config.max_events,
            });
        }
        self.source_clock = self.source_clock.saturating_add(events.len() as u64);
        for event in &events {
            if let Err(error) = schedule_rebuild(event) {
                self.gap = Some(EventGap {
                    reason: GapReason::CallbackFailed,
                    source_clock: self.source_clock,
                });
                self.emit("callback_failed", Some(error.clone()));
                return Err(WatchError::Callback(error));
            }
            self.emit("rebuild_scheduled", Some(event.path.clone()));
        }
        // Invariant: a watcher publish becomes queryable before it is
        // reported fresh. `self.previous`/`self.applied_clock` (the state a
        // query reads) are committed here, strictly before the
        // "gap_cleared"/"caught_up" freshness observations are emitted below
        // -- never the other way around, so nothing can observe a "fresh"
        // signal against state that has not actually been published yet.
        self.previous = current;
        self.previous_started_ns = started_ns;
        self.applied_clock = self.source_clock;
        if let Ok(mut queue) = self.native_queue.lock() {
            queue.next_source_clock = queue.next_source_clock.max(self.source_clock);
        }
        if self
            .gap
            .as_ref()
            .is_some_and(|gap| gap.reason != GapReason::CallbackFailed)
        {
            self.gap = None;
            self.emit("gap_cleared", None);
        }
        if self.gap.is_none() {
            self.emit("caught_up", None);
        }
        Ok(events)
    }

    pub fn report_event_gap(&mut self, reason: GapReason) -> Result<(), WatchError> {
        if self.closed {
            return Err(WatchError::Shutdown);
        }
        self.gap = Some(EventGap {
            reason,
            source_clock: self.source_clock,
        });
        self.emit("event_gap", Some(format!("{reason:?}")));
        Ok(())
    }

    /// Reconcile immediately without requiring a resident holder. Forces the
    /// snapshot path so a caller-visible freshness barrier is never gated on
    /// the supervised snapshot interval.
    pub fn reconcile_now<F>(&mut self, schedule_rebuild: F) -> Result<Vec<WatchEvent>, WatchError>
    where
        F: FnMut(&WatchEvent) -> Result<(), String>,
    {
        self.poll_inner(schedule_rebuild, &CancellationToken::new(), true)
    }

    pub fn barrier(&self, barrier: Barrier) -> BarrierPoll {
        if self.gap.is_some() {
            return BarrierPoll::Complete(BarrierResult::GapBlocked);
        }
        if self.applied_clock >= barrier.target_source_clock {
            return BarrierPoll::Complete(BarrierResult::CaughtUp);
        }
        if barrier
            .deadline
            .map(|deadline| self.clock.now() >= deadline)
            .unwrap_or(false)
        {
            return BarrierPoll::Complete(BarrierResult::Timeout);
        }
        BarrierPoll::Waiting
    }

    pub fn clear_gap_after_reconcile(&mut self) {
        self.gap = None;
        self.emit("gap_cleared", None);
    }

    pub fn shutdown(&mut self) -> Result<(), WatchError> {
        if self.closed {
            return Ok(());
        }
        self.emit("shutdown_requested", None);
        self.closed = true;
        self.emit("shutdown_complete", None);
        Ok(())
    }

    fn emit(&mut self, kind: &str, detail: Option<String>) {
        self.sequence = self.sequence.saturating_add(1);
        if let Some(sink) = &self.sink {
            sink.observe(LifecycleObservation {
                sequence: self.sequence,
                kind: kind.to_owned(),
                source_clock: self.source_clock,
                applied_clock: self.applied_clock,
                detail,
            });
        }
    }
}
