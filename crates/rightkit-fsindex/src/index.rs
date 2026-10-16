//! The live index: open, query, change subscription.

use crate::backend::*;
use crate::config::*;
#[cfg(feature = "content")]
use crate::error::Error;
use crate::error::Result;
use crate::gate::{Decision, Rate, RelistGate};
use crate::limits::{RelistBudget, RescanGovernor};
use crate::query::Query;
use crate::store::{now_secs, Store};
use std::borrow::Cow;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ public types

/// Sizes of a folder (rollup) or a single entry. See README "Size semantics".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeInfo {
    /// Data + resource fork length, hard links counted once.
    pub logical: u64,
    /// ALLOCSIZE, hard links counted once, every APFS clone counted in full.
    pub allocated: u64,
    /// Clones and shared extents counted once per folder; see README for limits.
    pub allocated_unique: u64,
    /// Non-directory entries counted.
    pub files: u64,
    pub is_package: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub path: PathBuf,
    pub name: OsString,
    pub kind: EntryKind,
    pub is_package: bool,
    /// Hard link whose bytes are counted through another link of the same inode.
    pub is_secondary_link: bool,
    /// Folder deliberately not indexed (protected store, consent-gated, configured).
    pub excluded: bool,
    pub size: SizeInfo,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub item: Item,
    pub score: i32,
}

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub limit: usize,
    pub scope: Option<PathBuf>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            limit: 100,
            scope: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    AllocatedUnique,
    Allocated,
    Logical,
    Name,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Removed,
    Modified,
    /// A subtree was re-crawled (must-scan-subdirs); contents may all have changed.
    Rescanned,
}

#[derive(Clone, Debug)]
pub struct Change {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

#[derive(Clone, Debug)]
pub struct RollupUpdate {
    pub path: PathBuf,
    pub size: SizeInfo,
}

/// One coalesced delivery. Bounded by `Config::max_change_paths` / `max_change_rollups`.
#[derive(Clone, Debug, Default)]
pub struct ChangeBatch {
    pub seq: u64,
    pub changes: Vec<Change>,
    /// Total changes in the window, including ones not listed.
    pub total_changes: u64,
    pub changes_truncated: bool,
    /// Current rollups of the folders affected (changed folders and their ancestors), shallowest first.
    pub rollups: Vec<RollupUpdate>,
    pub rollups_truncated: bool,
    /// The whole index was rebuilt (dropped events); re-read everything shown.
    pub rescanned: bool,
    /// Batches this subscriber missed because its queue was full; re-read what is shown.
    pub missed_batches: u32,
}

#[derive(Clone, Debug)]
pub struct RootStatus {
    pub path: PathBuf,
    pub readable: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub backend: String,
    pub roots: Vec<RootStatus>,
    pub entries: usize,
    pub resumed_from_snapshot: bool,
    pub full_rescans: u32,
    pub last_event_id: u64,
    pub full_disk_access: bool,
    /// Directories skipped on purpose (protected stores, consent-gated folders, exclusions).
    pub skipped_dirs: Vec<PathBuf>,
    /// Directories that could not be read (first 1000).
    pub unreadable_dirs: Vec<PathBuf>,
    pub unreadable_count: usize,
    pub crawl_time: Duration,
    /// Folder re-lists run since open.
    pub relists_total: u64,
    /// Folder re-lists per second, averaged over the last minute (or since open, if sooner).
    pub relist_rate_per_s: f64,
    /// Folders currently backing off: changing so often that their re-list interval is above
    /// `Config::relist_min_interval`.
    pub backoff_folders: usize,
    /// Dropped-event reports absorbed into an already pending full rescan (not run on their own).
    pub rescans_coalesced: u64,
    /// Cooldown applied after the last full rescan, in milliseconds (0 before any). It doubles
    /// from 30 s to 10 min while drops keep coming and resets after a quiet period.
    pub rescan_backoff_ms: u64,
    /// Resident memory of the index: `mapped` is file-backed column and name pages currently
    /// in memory (measured with `mincore`; reclaimable, see `Index::shrink`); `heap` is the
    /// tracked anonymous allocations the index owns (estimate).
    pub resident_bytes: ResidentBytes,
    /// `resident_bytes.mapped + resident_bytes.heap` divided by `entries` (0 when empty).
    pub resident_per_entry: f64,
    /// Directory reads (listings) since open. Each is one `getattrlistbulk` pass.
    pub dir_reads_total: u64,
    /// Directory reads per second over the last minute (or since open, if sooner).
    pub dir_reads_per_s: f64,
    /// Process CPU time (user plus system) since process start, in seconds. Process-wide.
    pub cpu_seconds_total: f64,
    /// Process CPU percent since the previous `status()` call (since open, for the first call).
    /// Process-wide: it includes other work in the same process.
    pub cpu_percent: f64,
}

/// Resident bytes, split by kind. See `Status::resident_bytes`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResidentBytes {
    pub mapped: u64,
    pub heap: u64,
}

/// Process CPU time (user plus system) in seconds, or 0 if unavailable.
#[cfg(not(unix))]
fn cpu_seconds() -> f64 {
    0.0
}

/// Process CPU time (user plus system) in seconds, or 0 if unavailable.
#[cfg(unix)]
fn cpu_seconds() -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return 0.0;
    }
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    secs(ru.ru_utime) + secs(ru.ru_stime)
}

// ------------------------------------------------------------------ shared state

struct Sub {
    tx: SyncSender<ChangeBatch>,
    missed: u32,
}

struct Shared {
    store: RwLock<Store>,
    /// Per-folder re-list gate (see `gate.rs`).
    gate: Mutex<RelistGate>,
    /// Directory reads (one per listing), for `Status::dir_reads_per_s`.
    reads: Mutex<Rate>,
    /// Time and process CPU at the previous `status()` call.
    cpu_mark: Mutex<(Instant, f64)>,
    cfg: Config,
    roots: Vec<PathBuf>,
    backend: Arc<dyn Backend>,
    excludes: Excludes,
    subs: Mutex<Vec<Sub>>,
    last_event_id: AtomicU64,
    status: Mutex<Status>,
    fingerprint: u64,
    #[cfg(feature = "content")]
    content: Mutex<crate::content::ContentIndex>,
}

fn rd(s: &Shared) -> RwLockReadGuard<'_, Store> {
    s.store.read().unwrap_or_else(|e| e.into_inner())
}
fn wr(s: &Shared) -> RwLockWriteGuard<'_, Store> {
    s.store.write().unwrap_or_else(|e| e.into_inner())
}

#[cfg(unix)]
fn os(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(b.to_vec()))
}
#[cfg(not(unix))]
fn os(b: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b).into_owned())
}

#[cfg(unix)]
fn pb(p: &Path) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(p.as_os_str().as_bytes())
}
#[cfg(not(unix))]
fn pb(p: &Path) -> Cow<'_, [u8]> {
    Cow::Owned(p.to_string_lossy().into_owned().into_bytes())
}

fn name_os(b: Vec<u8>) -> OsString {
    os(&b).into_os_string()
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(dir.len() + 1 + name.len());
    p.extend_from_slice(dir);
    if dir != b"/" {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

fn parent_of(p: &[u8]) -> Option<&[u8]> {
    if p.len() <= 1 {
        return None;
    }
    match p.iter().rposition(|&b| b == b'/') {
        Some(0) => Some(b"/"),
        Some(i) => Some(&p[..i]),
        None => None,
    }
}

fn fnv(h: &mut u64, b: &[u8]) {
    for &x in b {
        *h ^= x as u64;
        *h = h.wrapping_mul(0x0100_0000_01b3);
    }
    *h ^= 0xff;
}

/// The effective exclude patterns: the built-in set unless `include_build_output`, then the
/// caller's patterns. Each pattern is split on `/` into non-empty segments.
fn exclude_pattern_list(cfg: &Config) -> Vec<Vec<Vec<u8>>> {
    let defaults: &[&str] = if cfg.include_build_output {
        &[]
    } else {
        DEFAULT_EXCLUDE_PATTERNS
    };
    defaults
        .iter()
        .copied()
        .map(str::to_string)
        .chain(cfg.exclude_patterns.iter().cloned())
        .map(|p| {
            p.split('/')
                .filter(|s| !s.is_empty())
                .map(|s| s.as_bytes().to_vec())
                .collect::<Vec<Vec<u8>>>()
        })
        .filter(|segs| !segs.is_empty())
        .collect()
}

/// Whether `path` is, or lies below, a directory the store holds as an excluded entry.
fn under_excluded_entry(store: &Store, path: &[u8]) -> bool {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if let Some(slot) = store.lookup(p) {
            return store.entry(slot).d.flags & FLAG_EXCLUDED != 0;
        }
        cur = parent_of(p);
    }
    false
}

/// Whether `path` is `dir` or below it, on a component boundary. `dir` may be `/`.
fn within_dir(path: &[u8], dir: &[u8]) -> bool {
    let d = trim_trailing_slash(dir);
    path.starts_with(&d) && (path.len() == d.len() || path[d.len()] == b'/')
}

fn trim_trailing_slash(p: &[u8]) -> Vec<u8> {
    let mut v = p.to_vec();
    while v.last() == Some(&b'/') {
        v.pop();
    }
    v
}

/// Returns free malloc pages to the system where the platform allows it (macOS zones).
#[cfg(target_os = "macos")]
fn release_allocator_memory() {
    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn release_allocator_memory() {}

fn kind_of(k: u8) -> EntryKind {
    match k {
        KIND_FILE => EntryKind::File,
        KIND_DIR => EntryKind::Dir,
        KIND_LINK => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

fn home_dirs(cfg: &Config) -> Vec<PathBuf> {
    if let Some(h) = &cfg.home_dirs {
        return h.clone();
    }
    let mut v = Vec::new();
    if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(h));
    }
    if let Ok(rd) = std::fs::read_dir("/Users") {
        for e in rd.flatten() {
            let n = e.file_name();
            if n != "Shared" && !n.to_string_lossy().starts_with('.') {
                v.push(e.path());
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

// ------------------------------------------------------------------ crawl into store

fn apply_listings(store: &mut Store, parent: u32, ls: &[Listing]) -> std::io::Result<()> {
    if ls.is_empty() {
        return Ok(());
    }
    if ls[0].error.is_some() {
        store.set_flag(parent, FLAG_UNREADABLE);
    }
    let mut stack = vec![(0usize, parent)];
    while let Some((li, slot)) = stack.pop() {
        let l = &ls[li];
        for e in &l.ents {
            let s = store.add_child(slot, l.name(e), e.data)?;
            if e.child != NONE {
                let ci = e.child as usize;
                if ls.get(ci).is_some_and(|c| c.error.is_some()) {
                    store.set_flag(s, FLAG_UNREADABLE);
                }
                stack.push((ci, s));
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ pending changes

#[derive(Default)]
struct Pending {
    changes: Vec<Change>,
    total: u64,
    truncated: bool,
    dirty: Vec<u32>,
    dirty_set: HashSet<u32>,
    rescanned: bool,
    first_at: Option<Instant>,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.total == 0 && !self.rescanned && self.dirty.is_empty()
    }
    fn touch(&mut self) {
        self.first_at.get_or_insert_with(Instant::now);
    }
    fn record(&mut self, kind: ChangeKind, path: Vec<u8>, max: usize) {
        self.touch();
        self.total += 1;
        if self.changes.len() < max {
            self.changes.push(Change {
                path: os(&path),
                kind,
            });
        } else {
            self.truncated = true;
        }
    }
    /// Mark `slot` (a folder) and all its ancestors dirty, stopping at one already marked.
    fn mark_dirty(&mut self, store: &Store, slot: u32) {
        self.touch();
        let mut x = slot;
        while x != NONE && self.dirty_set.insert(x) {
            self.dirty.push(x);
            x = store.entry(x).parent;
        }
    }
}

pub(crate) fn size_of(store: &Store, s: u32) -> SizeInfo {
    let e = store.entry(s);
    if e.d.kind == KIND_DIR {
        let r = store.rollup(s);
        SizeInfo {
            logical: r.logical,
            allocated: r.alloc,
            allocated_unique: r.unique,
            files: r.files,
            is_package: e.d.flags & FLAG_PACKAGE != 0,
        }
    } else {
        SizeInfo {
            logical: e.d.logical,
            allocated: e.d.alloc,
            allocated_unique: e.d.alloc,
            files: 1,
            is_package: false,
        }
    }
}

fn item_of(store: &Store, s: u32) -> Item {
    let e = store.entry(s);
    let path = store.path_of(s);
    let name = store.name_of(s);
    let name = if e.parent == NONE {
        path.clone()
    } else {
        name.to_vec()
    };
    Item {
        path: os(&path),
        name: name_os(name),
        kind: kind_of(e.d.kind),
        is_package: e.d.flags & FLAG_PACKAGE != 0,
        is_secondary_link: e.d.flags & FLAG_SECONDARY_LINK != 0,
        excluded: e.d.flags & FLAG_EXCLUDED != 0,
        size: size_of(store, s),
    }
}

// ------------------------------------------------------------------ the Index

pub struct Index {
    shared: Arc<Shared>,
    tx: Sender<WatchMsg>,
    worker: Option<std::thread::JoinHandle<()>>,
    watcher: Option<Box<dyn Watcher>>,
}

impl Index {
    pub fn open(cfg: Config) -> Result<Index> {
        Self::open_with_backend(cfg, default_backend())
    }

    /// Blocks until the first crawl (or reopen of the committed column store) finished; unreadable roots are reported in `status()`.
    pub fn open_with_backend(cfg: Config, backend: Arc<dyn Backend>) -> Result<Index> {
        Self::open_inner(cfg, backend, None)
    }

    /// Test and benchmark support: fill the columns with `entries` generated entries instead of crawling.
    #[doc(hidden)]
    pub fn open_synthetic(cfg: Config, entries: usize) -> Result<Index> {
        Self::open_inner(cfg, default_backend(), Some(entries))
    }

    fn open_inner(cfg: Config, backend: Arc<dyn Backend>, synth: Option<usize>) -> Result<Index> {
        backend.check_supported()?;
        let roots: Vec<PathBuf> = cfg.roots.iter().map(|r| canon(r)).collect();
        let fda = backend.has_full_disk_access();

        // Exclusions: configured, protected stores, consent-gated folders without FDA.
        let mut skipped: Vec<PathBuf> = Vec::new();
        let homes = home_dirs(&cfg);
        if !cfg.include_protected {
            for h in &homes {
                skipped.extend(PROTECTED_HOME_RELATIVE.iter().map(|r| h.join(r)));
            }
        }
        if !fda && !cfg.allow_consent_prompts {
            for h in &homes {
                skipped.extend(CONSENT_HOME_RELATIVE.iter().map(|r| h.join(r)));
            }
        }
        skipped.extend(cfg.exclude.iter().cloned());
        let skipped: Vec<PathBuf> = skipped.into_iter().map(|p| canon(&p)).collect();
        let mut excludes = Excludes {
            prefixes: skipped.iter().map(|p| pb(p).into_owned()).collect(),
            patterns: exclude_pattern_list(&cfg),
            roots: roots.iter().map(|r| trim_trailing_slash(&pb(r))).collect(),
            ..Excludes::default()
        };
        if !cfg.include_build_output {
            #[cfg(target_os = "macos")]
            excludes.prefixes.extend(
                DEFAULT_EXCLUDE_ROOTS_MACOS
                    .iter()
                    .map(|r| r.as_bytes().to_vec()),
            );
            #[cfg(windows)]
            excludes.ci_prefixes.extend(
                DEFAULT_EXCLUDE_ROOTS_WINDOWS
                    .iter()
                    .map(|r| r.as_bytes().to_vec()),
            );
            excludes.honor_cachedir_tag = true;
        }
        // The index's own state directory never feeds back into the index.
        let mut state_dir_in_root: Option<Vec<u8>> = None;
        if let Some(sd) = &cfg.state_dir {
            // Created now (best effort; the open below reports a real failure) so the
            // canonical path matches the one events will carry.
            let _ = std::fs::create_dir_all(sd);
            let sd = trim_trailing_slash(&pb(&canon(sd)));
            if !sd.is_empty() && roots.iter().any(|r| within_dir(&sd, &pb(r))) {
                excludes.prefixes.push(sd.clone());
                state_dir_in_root = Some(sd);
            }
        }
        excludes.prefixes.sort();
        excludes.prefixes.dedup();

        let mut pkg: Vec<Vec<u8>> = DEFAULT_PACKAGE_EXTENSIONS
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect();
        pkg.extend(
            cfg.extra_package_extensions
                .iter()
                .map(|s| s.trim_start_matches('.').to_ascii_lowercase().into_bytes()),
        );
        let mut fp = 0xcbf2_9ce4_8422_2325u64;
        for r in &roots {
            fnv(&mut fp, &pb(r));
        }
        for e in &excludes.prefixes {
            fnv(&mut fp, e);
        }
        for e in &excludes.ci_prefixes {
            fnv(&mut fp, &e.to_ascii_lowercase());
        }
        if excludes.honor_cachedir_tag {
            fnv(&mut fp, b"cachedir-tag");
        }
        for pattern in &excludes.patterns {
            for seg in pattern {
                fnv(&mut fp, seg);
            }
            fnv(&mut fp, b"/");
        }
        for p in &pkg {
            fnv(&mut fp, p);
        }

        let mut resumed = None;
        let mut opened = None;
        if let Some(sd) = &cfg.state_dir {
            std::fs::create_dir_all(sd)?;
            recover_swap(sd);
            if sd.join("index").exists() {
                if let Some((st, info)) = Store::open_existing(&sd.join("index"), pkg.clone(), fp)?
                {
                    if now_secs().saturating_sub(info.saved_at) <= cfg.snapshot_max_age.as_secs()
                        && info.event_id <= backend.current_event_id()
                    {
                        resumed = Some(info.event_id);
                        opened = Some(st);
                    }
                }
            }
        }
        let mut store = match opened {
            Some(s) => s,
            None => fresh_store(cfg.state_dir.as_deref(), "index", pkg, fp)?,
        };
        #[cfg(feature = "content")]
        {
            store.log_changes = cfg.content.enabled;
        }

        let (tx, rx) = channel();
        let start_id = backend.current_event_id();
        let watcher = if cfg.watch {
            Some(backend.watch(&roots, resumed, cfg.watch_latency, tx.clone())?)
        } else {
            None
        };

        let mut status = Status {
            backend: backend.name().to_string(),
            full_disk_access: fda,
            skipped_dirs: skipped.clone(),
            resumed_from_snapshot: resumed.is_some(),
            ..Status::default()
        };
        let t0 = Instant::now();
        if resumed.is_none() {
            if let Some(n) = synth {
                fill_synthetic(&mut store, &roots[0], n)?;
            } else {
                for r in &roots {
                    crawl_root(&*backend, &excludes, cfg.crawl_threads, &mut store, r)?;
                }
            }
            store.commit(start_id)?;
        }
        status.crawl_time = t0.elapsed();
        for r in &roots {
            status.roots.push(root_status(&*backend, &store, r));
        }
        store.take_logs();

        #[cfg(feature = "content")]
        let content = Mutex::new(crate::content::ContentIndex::new(&cfg));
        let opened_at = Instant::now();
        let gate = Mutex::new(RelistGate::new(
            cfg.relist_min_interval,
            cfg.relist_max_interval,
            opened_at,
        ));
        let shared = Arc::new(Shared {
            store: RwLock::new(store),
            gate,
            reads: Mutex::new(Rate::new(opened_at)),
            cpu_mark: Mutex::new((opened_at, cpu_seconds())),
            cfg,
            roots,
            backend,
            excludes,
            subs: Mutex::new(Vec::new()),
            last_event_id: AtomicU64::new(resumed.unwrap_or(start_id)),
            status: Mutex::new(status),
            fingerprint: fp,
            #[cfg(feature = "content")]
            content,
        });
        #[cfg(feature = "content")]
        shared.content_rebuild();

        let w = Worker {
            max_seen: shared.last_event_id.load(Ordering::Relaxed),
            shared: shared.clone(),
            pending: Pending::default(),
            last_commit: Instant::now(),
            last_compact: Instant::now(),
            last_trim: Instant::now(),
            rescans: RescanGovernor::new(
                shared.cfg.rescan_cooldown,
                crate::limits::RESCAN_COOLDOWN_MAX,
                crate::limits::RESCAN_QUIET_RESET,
            ),
            relists: RelistBudget::with_defaults(Instant::now()),
            state_dir: state_dir_in_root,
        };
        let worker = std::thread::Builder::new()
            .name("rightkit-fsindex".into())
            .spawn(move || w.run(rx))?;
        Ok(Index {
            shared,
            tx,
            worker: Some(worker),
            watcher,
        })
    }

    pub fn status(&self) -> Status {
        let mut st = self
            .shared
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let now = Instant::now();
        let gate_heap = {
            let mut gate = self.shared.gate.lock().unwrap_or_else(|e| e.into_inner());
            st.relists_total = gate.relists_total();
            st.relist_rate_per_s = gate.relists_per_s(now);
            st.backoff_folders = gate.backoff_folders();
            gate.heap_bytes()
        };
        {
            let mut reads = self.shared.reads.lock().unwrap_or_else(|e| e.into_inner());
            st.dir_reads_total = reads.total();
            st.dir_reads_per_s = reads.per_s(now);
        }
        let cpu_now = cpu_seconds();
        {
            let mut mark = self
                .shared
                .cpu_mark
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let elapsed = now.duration_since(mark.0).as_secs_f64();
            st.cpu_percent = if elapsed > 0.001 {
                (cpu_now - mark.1) / elapsed * 100.0
            } else {
                0.0
            };
            *mark = (now, cpu_now);
        }
        st.cpu_seconds_total = cpu_now;
        let store = rd(&self.shared);
        st.resident_bytes = ResidentBytes {
            mapped: store.mapped_resident_bytes() as u64,
            heap: (store.heap_bytes() + gate_heap) as u64,
        };
        st.entries = store.len();
        st.resident_per_entry = if st.entries == 0 {
            0.0
        } else {
            (st.resident_bytes.mapped + st.resident_bytes.heap) as f64 / st.entries as f64
        };
        st.last_event_id = self.shared.last_event_id.load(Ordering::Relaxed);
        st.roots = self
            .shared
            .roots
            .iter()
            .map(|r| root_status(&*self.shared.backend, &store, r))
            .collect();
        st.unreadable_dirs.clear();
        st.unreadable_count = 0;
        for s in store.live_slots() {
            let (kind, flags) = store.kind_flags(s);
            if kind == KIND_DIR && flags & FLAG_UNREADABLE != 0 {
                st.unreadable_count += 1;
                if st.unreadable_dirs.len() < 1000 {
                    st.unreadable_dirs.push(os(&store.path_of(s)));
                }
            }
        }
        st
    }

    pub fn search(&self, query: &str) -> Result<Vec<Hit>> {
        self.search_with(query, &SearchOptions::default())
    }

    pub fn search_with(&self, query: &str, opts: &SearchOptions) -> Result<Vec<Hit>> {
        let q = Query::parse(query)?;
        let store = rd(&self.shared);
        let limit = opts.limit.max(1);
        const UNKNOWN: i32 = i32::MIN;
        const MISS: i32 = i32::MIN + 1;
        let mut cache = vec![UNKNOWN; store.names.len()];
        let mut heap: BinaryHeap<std::cmp::Reverse<(i32, std::cmp::Reverse<u32>)>> =
            BinaryHeap::new();
        let mut visit = |s: u32| {
            let nid = store.name_id(s) as usize;
            if cache[nid] == UNKNOWN {
                cache[nid] = q.score_name(store.names.get(nid as u32)).unwrap_or(MISS);
            }
            let sc = cache[nid];
            if sc != MISS {
                heap.push(std::cmp::Reverse((sc, std::cmp::Reverse(s))));
                if heap.len() > limit {
                    heap.pop();
                }
            }
        };
        match &opts.scope {
            None => store.live_slots().for_each(&mut visit),
            Some(sc) => {
                let Some(root) = store
                    .lookup(&pb(&canon(sc)))
                    .or_else(|| store.lookup(&pb(sc)))
                else {
                    return Ok(Vec::new());
                };
                let mut stack = vec![root];
                while let Some(x) = stack.pop() {
                    if x != root {
                        visit(x);
                    }
                    stack.extend(store.children(x));
                }
            }
        }
        let mut v: Vec<(i32, u32)> = heap
            .into_iter()
            .map(|std::cmp::Reverse((sc, std::cmp::Reverse(s)))| (sc, s))
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let hits: Vec<Hit> = v
            .into_iter()
            .map(|(score, s)| Hit {
                item: item_of(&store, s),
                score,
            })
            .collect();
        if store.slots() > 500_000 {
            // A whole-index scan faults in tens of MB of clean pages; have the worker drop them (throttled).
            let _ = self.tx.send(WatchMsg::Trim);
        }
        Ok(hits)
    }

    fn lookup(&self, store: &Store, path: &Path) -> Option<u32> {
        store
            .lookup(&pb(path))
            .or_else(|| store.lookup(&pb(&canon(path))))
    }

    /// Sizes of a folder (rollup) or file. `None` if the path is not indexed.
    pub fn folder_size(&self, path: impl AsRef<Path>) -> Option<SizeInfo> {
        let store = rd(&self.shared);
        let s = self.lookup(&store, path.as_ref())?;
        Some(size_of(&store, s))
    }

    /// Children of a folder, largest `allocated_unique` first.
    pub fn children_sorted_by_size(&self, path: impl AsRef<Path>) -> Option<Vec<Item>> {
        self.children_sorted_by(path, SortKey::AllocatedUnique)
    }

    pub fn children_sorted_by(&self, path: impl AsRef<Path>, key: SortKey) -> Option<Vec<Item>> {
        let store = rd(&self.shared);
        let s = self.lookup(&store, path.as_ref())?;
        let mut v: Vec<(u32, SizeInfo)> = store
            .children(s)
            .into_iter()
            .map(|c| (c, size_of(&store, c)))
            .collect();
        let k = |x: &SizeInfo| match key {
            SortKey::AllocatedUnique => x.allocated_unique,
            SortKey::Allocated => x.allocated,
            SortKey::Logical => x.logical,
            SortKey::Name => 0,
        };
        v.sort_by(|a, b| {
            k(&b.1)
                .cmp(&k(&a.1))
                .then(b.1.allocated.cmp(&a.1.allocated))
                .then_with(|| store.name_of(a.0).cmp(store.name_of(b.0)))
        });
        Some(v.into_iter().map(|(c, _)| item_of(&store, c)).collect())
    }

    /// Subscribe to coalesced change batches (one per `Config::change_coalesce` window at most).
    /// The queue is bounded; when it is full, batches are dropped and the next one reports `missed_batches`.
    pub fn subscribe(&self) -> Receiver<ChangeBatch> {
        let (tx, rx) = sync_channel(16);
        self.shared
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Sub { tx, missed: 0 });
        rx
    }

    /// Deliver pending platform events, apply them, and emit any pending change batch. Blocks.
    pub fn flush(&self) {
        if let Some(w) = &self.watcher {
            w.flush();
        }
        let (ack_tx, ack_rx) = channel();
        if self.tx.send(WatchMsg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(30));
        }
    }

    /// Write the snapshot now (needs `Config::state_dir`).
    pub fn snapshot(&self) -> Result<()> {
        let mut store = wr(&self.shared);
        store.commit(self.shared.last_event_id.load(Ordering::Relaxed))?;
        Ok(())
    }

    /// Rebuild the column store now (drops freed slots and unreferenced names). Test hook.
    #[doc(hidden)]
    pub fn compact_now(&self) {
        let (ack_tx, ack_rx) = channel();
        if self.tx.send(WatchMsg::Compact(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(120));
        }
    }

    /// Drop clean mapped pages from the resident set now. Also done automatically after 30 s idle.
    pub fn trim(&self) {
        wr(&self.shared).trim();
    }

    /// Gives back what the index can: commits pending changes, drops the resident pages of its
    /// column and name mappings (they fault back in on use), and asks the allocator to return
    /// free heap pages. Queries stay correct; the next ones pay the page-fault cost.
    pub fn shrink(&self) {
        {
            let mut st = wr(&self.shared);
            if st.is_dirty() {
                let _ = st.commit(self.shared.last_event_id.load(Ordering::Relaxed));
            }
            st.trim();
        }
        release_allocator_memory();
    }

    #[doc(hidden)]
    pub fn aux_counts(&self) -> (usize, usize, usize, usize) {
        let st = rd(&self.shared);
        let a = st.aux_counts();
        (a.0, a.1, a.2, st.special_count())
    }

    /// Slots in the column store and how many are free (diagnostics).
    #[doc(hidden)]
    pub fn slot_counts(&self) -> (usize, usize) {
        let st = rd(&self.shared);
        (st.slots(), st.free_slots())
    }

    /// Content search (feature `content`; also needs `Config::content.enabled`).
    #[cfg(feature = "content")]
    pub fn content_search(&self, needle: &str, limit: usize) -> Result<Vec<Hit>> {
        if !self.shared.cfg.content.enabled {
            return Err(Error::ContentDisabled);
        }
        self.shared.content_search(needle, limit)
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        self.watcher.take();
        let _ = self.tx.send(WatchMsg::Stop);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
        let _ = self.snapshot();
    }
}

fn recover_swap(sd: &Path) {
    let (new, cur) = (sd.join("index.new"), sd.join("index"));
    if new.exists() {
        if !cur.exists() && new.join("header.bin").exists() {
            let _ = std::fs::rename(&new, &cur);
        } else {
            let _ = std::fs::remove_dir_all(&new);
        }
    }
}

fn fresh_store(
    state_dir: Option<&Path>,
    sub: &str,
    pkg: Vec<Vec<u8>>,
    fp: u64,
) -> std::io::Result<Store> {
    match state_dir {
        Some(sd) => {
            let d = sd.join(sub);
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d)?;
            Store::create(Some(&d), pkg, fp)
        }
        None => Store::create(None, pkg, fp),
    }
}

/// Stream a crawl of `root` straight into the store under `parent`, never holding more than the in-flight listings.
fn crawl_into(
    backend: &dyn Backend,
    root: &Path,
    excludes: &Excludes,
    threads: usize,
    store: &mut Store,
    parent: u32,
) -> std::io::Result<()> {
    let mut slots: HashMap<u32, u32> = HashMap::from([(0, parent)]);
    let mut err: Option<std::io::Error> = None;
    backend.crawl(root, excludes, threads, &mut |l| {
        if err.is_some() {
            return;
        }
        let Some(slot) = slots.remove(&l.id) else {
            return;
        };
        if l.error.is_some() {
            store.set_flag(slot, FLAG_UNREADABLE);
        }
        for e in &l.ents {
            match store.add_child(slot, l.name(e), e.data) {
                Ok(s) => {
                    if e.child != NONE {
                        slots.insert(e.child, s);
                    }
                }
                Err(e) => {
                    err = Some(e);
                    return;
                }
            }
        }
    });
    err.map_or(Ok(()), Err)
}

fn crawl_root(
    backend: &dyn Backend,
    excludes: &Excludes,
    threads: usize,
    store: &mut Store,
    root: &Path,
) -> Result<()> {
    let rb_c = pb(root);
    let rb: &[u8] = &rb_c;
    match backend.stat(root) {
        Ok(d) if d.kind == KIND_DIR => {
            let slot = store.add_root(rb, d)?;
            if excludes.matches(rb) {
                store.set_flag(slot, FLAG_EXCLUDED);
                return Ok(());
            }
            crawl_into(backend, root, excludes, threads, store, slot)?;
        }
        Ok(d) => {
            let slot = store.add_root(rb, d)?;
            store.set_flag(slot, FLAG_UNREADABLE);
        }
        Err(_) => {
            let slot = store.add_root(
                rb,
                EntryData {
                    kind: KIND_DIR,
                    ..EntryData::default()
                },
            )?;
            store.set_flag(slot, FLAG_UNREADABLE);
        }
    }
    Ok(())
}

/// Fill the store with `n` generated entries (no filesystem access): an 8-ary folder tree, files with
/// skewed 64-bit sizes, a shared name pool, and a few clones.
fn fill_synthetic(store: &mut Store, root: &Path, n: usize) -> Result<()> {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let r = store.add_root(
        &pb(root),
        EntryData {
            kind: KIND_DIR,
            ..EntryData::default()
        },
    )?;
    let ndirs = (n / 12).max(1);
    let mut dirs = vec![r];
    for i in 1..ndirs {
        let parent = dirs[(i - 1) / 8];
        let name = format!("dir{:x}", rnd() % (ndirs as u64 / 4 + 1));
        let d = EntryData {
            kind: KIND_DIR,
            ..EntryData::default()
        };
        dirs.push(store.add_child(parent, name.as_bytes(), d)?);
    }
    const EXT: [&str; 6] = ["dat", "txt", "png", "swift", "json", "log"];
    for k in 0..n.saturating_sub(ndirs) {
        let dir = dirs[(rnd() % dirs.len() as u64) as usize];
        let name = format!("file{:x}.{}", rnd() % (n as u64 / 4 + 1), EXT[k % 6]);
        let logical = rnd() % (1u64 << (rnd() % 26));
        let alloc = logical.div_ceil(4096) * 4096;
        let clone = k % 1000 == 0;
        let d = EntryData {
            kind: KIND_FILE,
            nlink: 1,
            mtime: 1_700_000_000 + (k % 100_000) as i64,
            dev: 1,
            fileid: k as u64 + 2,
            logical,
            alloc,
            private: if clone { alloc / 2 } else { alloc },
            cloneid: if clone { 1 + rnd() % 500 } else { 0 },
            ..EntryData::default()
        };
        store.add_child(dir, name.as_bytes(), d)?;
    }
    Ok(())
}

fn root_status(backend: &dyn Backend, store: &Store, r: &Path) -> RootStatus {
    let unreadable = store
        .lookup(&pb(r))
        .is_none_or(|s| store.entry(s).d.flags & FLAG_UNREADABLE != 0);
    let error = if unreadable {
        Some(match backend.stat(r) {
            Err(e) => e.to_string(),
            Ok(_) => "not readable (permission or Full Disk Access)".to_string(),
        })
    } else {
        None
    };
    RootStatus {
        path: r.to_path_buf(),
        readable: !unreadable,
        error,
    }
}

// ------------------------------------------------------------------ worker

struct Worker {
    shared: Arc<Shared>,
    /// Highest event id seen. Persisted only while no deferred re-list is outstanding, so a
    /// restart never skips a change that is still waiting for its window.
    max_seen: u64,
    pending: Pending,
    last_commit: Instant,
    last_compact: Instant,
    last_trim: Instant,
    /// Coalesces dropped-event full rescans behind a cooldown with exponential backoff.
    rescans: RescanGovernor,
    /// Global budget for subtree re-lists; the excess waits here, never dropped.
    relists: RelistBudget,
    /// Canonical state directory, when it lies under a root; events below it are skipped.
    state_dir: Option<Vec<u8>>,
}

impl Worker {
    fn run(mut self, rx: Receiver<WatchMsg>) {
        let coalesce = self.shared.cfg.change_coalesce;
        loop {
            let now = Instant::now();
            let mut wake = now + Duration::from_secs(30);
            if let Some(t) = self.pending.first_at {
                wake = wake.min(t + coalesce);
            }
            if let Some(d) = self
                .shared
                .gate
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .next_due()
            {
                wake = wake.min(d);
            }
            if let Some(d) = self.rescans.next_due() {
                wake = wake.min(d);
            }
            if let Some(d) = self.relists.next_due() {
                wake = wake.min(d);
            }
            let msg = match rx.recv_timeout(wake.saturating_duration_since(now)) {
                Ok(m) => Some(m),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    self.on_tick();
                    continue;
                }
                Err(_) => return,
            };
            let mut events: Vec<Event> = Vec::new();
            let mut acks: Vec<Sender<()>> = Vec::new();
            let mut compacts: Vec<Sender<()>> = Vec::new();
            let mut trim = false;
            let mut stop = false;
            let mut next = msg;
            let mut drained = 0;
            while let Some(m) = next.take() {
                match m {
                    WatchMsg::Events(mut e) => events.append(&mut e),
                    WatchMsg::Flush(a) => acks.push(a),
                    WatchMsg::Stop => stop = true,
                    WatchMsg::Compact(a) => compacts.push(a),
                    WatchMsg::Trim => trim = true,
                }
                drained += 1;
                if stop || drained >= 4096 {
                    break;
                }
                next = rx.try_recv().ok();
            }
            if !events.is_empty() {
                self.handle(events);
            }
            if trim && self.last_trim.elapsed() >= Duration::from_secs(2) {
                self.last_trim = Instant::now();
                wr(&self.shared).trim();
            }
            for a in compacts {
                let _ = self.shared.compact();
                let _ = a.send(());
            }
            if !acks.is_empty() {
                self.emit();
                for a in acks {
                    let _ = a.send(());
                }
            }
            if stop {
                return;
            }
        }
    }

    fn handle(&mut self, events: Vec<Event>) {
        let sh = self.shared.clone();
        let mut dirs: Vec<Vec<u8>> = Vec::new();
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        let mut subtrees: Vec<Vec<u8>> = Vec::new();
        let mut max_id = sh.last_event_id.load(Ordering::Relaxed);
        for ev in events {
            if ev.id != 0 {
                max_id = max_id.max(ev.id);
            }
            if ev.flags & (EV_USER_DROPPED | EV_KERNEL_DROPPED) != 0 {
                // Only marks a rescan wanted; it runs below when the cooldown allows.
                self.rescans.request();
                continue;
            }
            if self.under_state_dir(&ev.path) {
                continue;
            }
            if ev.flags == EV_HISTORY_DONE || ev.path.is_empty() {
                continue;
            }
            let mut p = ev.path;
            while p.len() > 1 && p.last() == Some(&b'/') {
                p.pop();
            }
            if ev.flags & (EV_MUST_SCAN_SUBDIRS | EV_ROOT_CHANGED | EV_MOUNT | EV_UNMOUNT) != 0 {
                subtrees.push(p);
            } else if seen.insert(p.clone()) {
                dirs.push(p);
            }
        }
        let now = Instant::now();
        if self.run_full_rescan_if_ready(now) {
            // A full rescan covers every subtree and folder in this batch.
        } else {
            for p in subtrees {
                self.relists.submit(p);
            }
            self.run_relists(now);
            for p in dirs {
                if self.gate_request(&p) == Decision::Run {
                    let mut done = HashSet::new();
                    self.sync_path(&p, &mut done);
                }
            }
        }
        self.max_seen = self.max_seen.max(max_id);
        if !self.gate_busy() {
            sh.last_event_id.store(self.max_seen, Ordering::Relaxed);
        }
        self.publish_limits();
        self.after_apply();
    }

    /// Whether `path` is the index's own state directory or below it.
    fn under_state_dir(&self, path: &[u8]) -> bool {
        self.state_dir
            .as_deref()
            .is_some_and(|sd| within_dir(path, sd))
    }

    /// Runs the coalesced full rescan if one is wanted and its cooldown has passed. Returns
    /// whether it ran. Subtree re-lists waiting for budget are covered by it and cleared.
    fn run_full_rescan_if_ready(&mut self, now: Instant) -> bool {
        if !self.rescans.try_begin(now) {
            return false;
        }
        let ok = self.full_rescan();
        self.rescans.finish(Instant::now(), ok);
        if ok {
            self.relists.clear_pending();
        }
        true
    }

    /// Re-lists the subtrees the global budget allows now; the rest stay pending.
    fn run_relists(&mut self, now: Instant) -> usize {
        let due = self.relists.drain(now);
        for p in &due {
            self.rescan_subtree(p);
        }
        due.len()
    }

    fn publish_limits(&self) {
        let mut st = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        st.rescans_coalesced = self.rescans.coalesced();
        st.rescan_backoff_ms = self.rescans.applied_ms();
    }

    /// Runs deferred folder re-lists that are due, then emits pending changes or commits.
    fn on_tick(&mut self) {
        let now = Instant::now();
        let mut applied = self.run_full_rescan_if_ready(now);
        if !applied {
            applied = self.run_relists(now) > 0;
        }
        self.publish_limits();
        let due = {
            let mut gate = self.shared.gate.lock().unwrap_or_else(|e| e.into_inner());
            gate.take_due(now)
        };
        if applied {
            if !self.gate_busy() {
                self.shared
                    .last_event_id
                    .store(self.max_seen, Ordering::Relaxed);
            }
            self.after_apply();
        }
        if !due.is_empty() {
            let mut done = HashSet::new();
            for p in &due {
                self.sync_path(p, &mut done);
            }
            if !self.gate_busy() {
                self.shared
                    .last_event_id
                    .store(self.max_seen, Ordering::Relaxed);
            }
            self.after_apply();
        }
        let coalesce = self.shared.cfg.change_coalesce;
        match self.pending.first_at {
            Some(t) if now.duration_since(t) >= coalesce => self.emit(),
            Some(_) => {}
            None => self.maybe_commit(),
        }
    }

    fn gate_request(&self, path: &[u8]) -> Decision {
        self.shared
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request(path, Instant::now())
    }

    /// Work is still waiting (folder re-lists, a coalesced rescan, or budgeted subtree
    /// re-lists), so the persisted event id must not advance past it.
    fn gate_busy(&self) -> bool {
        self.shared
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_deferred()
            || self.rescans.pending()
            || self.relists.pending() > 0
    }

    fn maybe_commit(&mut self) {
        if self.last_commit.elapsed() < Duration::from_secs(30) {
            return;
        }
        self.last_commit = Instant::now();
        let mut st = wr(&self.shared);
        if st.is_dirty() {
            let _ = st.commit(self.shared.last_event_id.load(Ordering::Relaxed));
        }
        st.trim();
    }

    fn after_apply(&mut self) {
        self.maybe_commit();
        let (slots, free) = {
            let st = rd(&self.shared);
            (st.slots(), st.free_slots())
        };
        if slots > 50_000
            && free * 4 > slots
            && self.last_compact.elapsed() > Duration::from_secs(600)
        {
            self.last_compact = Instant::now();
            let _ = self.shared.compact();
        }
        let (removed, touched) = wr(&self.shared).take_logs();
        #[cfg(feature = "content")]
        self.shared.content_apply(&removed, &touched);
        #[cfg(not(feature = "content"))]
        let _ = (removed, touched);
    }

    /// Rebuilds the whole index. Returns false when the rebuild failed (it is then retried
    /// after the cooldown).
    fn full_rescan(&mut self) -> bool {
        let sh = self.shared.clone();
        let Ok(fresh) = sh.build_fresh() else {
            return false;
        };
        sh.swap_store(fresh);
        sh.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .full_rescans += 1;
        self.pending.rescanned = true;
        self.pending.changes.clear();
        self.pending.dirty.clear();
        self.pending.dirty_set.clear();
        self.pending.touch();
        #[cfg(feature = "content")]
        sh.content_rebuild();
        true
    }

    fn rescan_subtree(&mut self, path: &[u8]) {
        let sh = self.shared.clone();
        if sh.excludes.matches(path) {
            return;
        }
        let slot = {
            let st = rd(&sh);
            if under_excluded_entry(&st, path) {
                return;
            }
            st.lookup(path)
        };
        let Some(slot) = slot else {
            if let Some(p) = parent_of(path) {
                let mut done = HashSet::new();
                self.sync_path(p, &mut done);
            }
            return;
        };
        let ls = sh.collect_listings(path);
        let mut store = wr(&sh);
        if store.lookup(path) != Some(slot) {
            return;
        }
        store.prune_children(slot, &HashSet::new());
        let _ = apply_listings(&mut store, slot, &ls);
        self.pending.record(
            ChangeKind::Rescanned,
            path.to_vec(),
            sh.cfg.max_change_paths,
        );
        self.pending.mark_dirty(&store, slot);
    }

    fn sync_path(&mut self, path: &[u8], done: &mut HashSet<Vec<u8>>) {
        let sh = self.shared.clone();
        if !done.insert(path.to_vec()) || sh.excludes.matches(path) {
            return;
        }
        // Directories excluded by a CACHEDIR.TAG are flagged in the store, not matched by
        // name: skip them and everything below.
        if under_excluded_entry(&rd(&sh), path) {
            return;
        }
        sh.reads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(Instant::now(), 1);
        let listing = sh.backend.list_dir(&os(path), &sh.excludes);
        let mut new_dirs: Vec<(u32, Vec<u8>)> = Vec::new();
        {
            let mut store = wr(&sh);
            let Some(slot) = store.lookup(path) else {
                let in_root = store.root_containing(path).is_some();
                drop(store);
                if in_root {
                    if let Some(p) = parent_of(path) {
                        self.sync_path(p, done);
                    }
                }
                return;
            };
            match listing {
                Ok(l) => {
                    let pmax = sh.cfg.max_change_paths;
                    new_dirs =
                        apply_dir_listing(&mut store, slot, path, &l, &mut self.pending, pmax);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) || e.raw_os_error() == Some(62) =>
                {
                    if store.entry(slot).parent == NONE {
                        store.set_flag(slot, FLAG_UNREADABLE);
                    } else {
                        let parent = store.entry(slot).parent;
                        self.pending.record(
                            ChangeKind::Removed,
                            path.to_vec(),
                            sh.cfg.max_change_paths,
                        );
                        store.remove_subtree(slot);
                        self.pending.mark_dirty(&store, parent);
                        let pp = parent_of(path).map(|p| p.to_vec());
                        drop(store);
                        if let Some(p) = pp {
                            self.sync_path(&p, done);
                        }
                        return;
                    }
                }
                Err(_) => {
                    store.set_flag(slot, FLAG_UNREADABLE);
                }
            }
        }
        for (slot, p) in new_dirs {
            let ls = sh.collect_listings(&p);
            let mut store = wr(&sh);
            if store.is_live(slot) && store.lookup(&p) == Some(slot) {
                let _ = apply_listings(&mut store, slot, &ls);
                self.pending.mark_dirty(&store, slot);
            }
        }
    }

    fn emit(&mut self) {
        if self.pending.is_empty() {
            self.pending = Pending::default();
            return;
        }
        let p = std::mem::take(&mut self.pending);
        let sh = &self.shared;
        let store = rd(sh);
        let mut dirty: Vec<(usize, u32)> = p
            .dirty
            .iter()
            .filter(|&&s| store.is_live(s))
            .map(|&s| (store.depth(s), s))
            .collect();
        dirty.sort();
        let max = sh.cfg.max_change_rollups;
        let rollups_truncated = dirty.len() > max;
        dirty.truncate(max);
        let rollups = dirty
            .into_iter()
            .map(|(_, s)| RollupUpdate {
                path: os(&store.path_of(s)),
                size: size_of(&store, s),
            })
            .collect();
        drop(store);
        let mut batch = ChangeBatch {
            seq: 0,
            changes: p.changes,
            total_changes: p.total,
            changes_truncated: p.truncated,
            rollups,
            rollups_truncated,
            rescanned: p.rescanned,
            missed_batches: 0,
        };
        static SEQ: AtomicU64 = AtomicU64::new(1);
        batch.seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut subs = sh.subs.lock().unwrap_or_else(|e| e.into_inner());
        subs.retain_mut(|s| {
            let mut b = batch.clone();
            b.missed_batches = s.missed;
            match s.tx.try_send(b) {
                Ok(()) => {
                    s.missed = 0;
                    true
                }
                Err(TrySendError::Full(_)) => {
                    s.missed += 1;
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }
}

fn changed(old: &EntryData, new: &EntryData) -> bool {
    old.kind != new.kind
        || old.logical != new.logical
        || old.alloc != new.alloc
        || old.private != new.private
        || old.cloneid != new.cloneid
        || old.nlink != new.nlink
        || old.fileid != new.fileid
        || old.mtime != new.mtime.clamp(0, u32::MAX as i64)
        || (old.flags ^ new.flags) & (FLAG_HIDDEN | FLAG_EXCLUDED | FLAG_MOUNT) != 0
}

/// Diff one directory listing against the store. Returns new subdirectories that still need a crawl.
fn apply_dir_listing(
    store: &mut Store,
    slot: u32,
    dirp: &[u8],
    l: &Listing,
    pend: &mut Pending,
    pmax: usize,
) -> Vec<(u32, Vec<u8>)> {
    let mut existing: HashMap<Vec<u8>, u32> = store
        .children(slot)
        .iter()
        .map(|&c| (store.name_of(c).to_vec(), c))
        .collect();
    let mut keep: HashSet<u32> = HashSet::with_capacity(existing.len());
    let mut new_dirs = Vec::new();
    let mut any = false;
    for e in &l.ents {
        let name = l.name(e);
        let d = e.data;
        let mut add = true;
        if let Some(s) = existing.remove(name) {
            let old = store.entry(s).d;
            if old.kind == d.kind {
                add = false;
                keep.insert(s);
                if changed(&old, &d) {
                    store.update(s, d);
                    if d.kind != KIND_DIR {
                        pend.record(ChangeKind::Modified, join(dirp, name), pmax);
                        any = true;
                    }
                }
            } else {
                pend.record(ChangeKind::Removed, join(dirp, name), pmax);
                store.remove_subtree(s);
                any = true;
            }
        }
        if add {
            let Ok(s) = store.add_child(slot, name, d) else {
                continue;
            };
            keep.insert(s);
            any = true;
            let p = join(dirp, name);
            if d.kind == KIND_DIR && d.flags & (FLAG_MOUNT | FLAG_EXCLUDED) == 0 {
                new_dirs.push((s, p.clone()));
            }
            pend.record(ChangeKind::Created, p, pmax);
        }
    }
    for name in existing.keys() {
        pend.record(ChangeKind::Removed, join(dirp, name), pmax);
        any = true;
    }
    if !existing.is_empty() {
        store.prune_children(slot, &keep);
    }
    if any {
        pend.mark_dirty(store, slot);
    }
    new_dirs
}

#[cfg(feature = "content")]
pub(crate) fn path_bytes(p: &Path) -> Vec<u8> {
    pb(p).into_owned()
}

#[cfg(feature = "content")]
impl Shared {
    fn content_collect(&self, slots: &[u32]) -> Vec<(u32, PathBuf)> {
        let store = rd(self);
        let c = self.content.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .iter()
            .copied()
            .filter(|&s| store.is_live(s))
            .filter(|&s| store.entry(s).d.kind == KIND_FILE)
            .filter_map(|s| {
                let p = store.path_of(s);
                c.eligible(&p, store.entry(s).d.logical)
                    .then(|| (s, os(&p)))
            })
            .collect()
    }

    pub(crate) fn content_rebuild(&self) {
        if !self.cfg.content.enabled {
            return;
        }
        self.content
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let all: Vec<u32> = rd(self).live_slots().collect();
        let todo = self.content_collect(&all);
        let mut c = self.content.lock().unwrap_or_else(|e| e.into_inner());
        for (s, p) in todo {
            c.index_file(s, &p);
        }
    }

    pub(crate) fn content_apply(&self, removed: &[u32], touched: &[u32]) {
        if !self.cfg.content.enabled {
            return;
        }
        {
            let mut c = self.content.lock().unwrap_or_else(|e| e.into_inner());
            for &s in removed {
                c.remove(s);
            }
        }
        let todo = self.content_collect(touched);
        let mut c = self.content.lock().unwrap_or_else(|e| e.into_inner());
        for (s, p) in todo {
            c.index_file(s, &p);
        }
    }

    pub(crate) fn content_search(&self, needle: &str, limit: usize) -> Result<Vec<Hit>> {
        let n = needle.to_ascii_lowercase().into_bytes();
        if n.len() < 3 {
            return Err(Error::InvalidQuery(
                "content needle needs at least 3 bytes".into(),
            ));
        }
        let cands = self
            .content
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .candidates(&n);
        let store = rd(self);
        let mut hits = Vec::new();
        for s in cands {
            if !store.is_live(s) {
                continue;
            }
            let path = os(&store.path_of(s));
            let count = crate::content::verify(&path, &n);
            if count > 0 {
                hits.push(Hit {
                    item: item_of(&store, s),
                    score: count as i32,
                });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.item.path.cmp(&b.item.path))
        });
        hits.truncate(limit.max(1));
        Ok(hits)
    }
}

impl Shared {
    fn swap_store(&self, fresh: Store) {
        let mut g = wr(self);
        match &self.cfg.state_dir {
            Some(sd) if fresh.is_file_backed() => {
                let old = std::mem::replace(&mut *g, fresh);
                drop(old);
                let cur = sd.join("index");
                let _ = std::fs::remove_dir_all(&cur);
                if std::fs::rename(sd.join("index.new"), &cur).is_ok() {
                    g.relocate(&cur);
                }
            }
            _ => *g = fresh,
        }
    }

    fn build_fresh(&self) -> Result<Store> {
        let pk = rd(self).package_exts_clone();
        let start = self.backend.current_event_id();
        let mut fresh = fresh_store(
            self.cfg.state_dir.as_deref(),
            "index.new",
            pk,
            self.fingerprint,
        )?;
        for r in &self.roots {
            let _ = crawl_root(
                &*self.backend,
                &self.excludes,
                self.cfg.crawl_threads,
                &mut fresh,
                r,
            );
        }
        fresh.log_changes = rd(self).log_changes;
        fresh.take_logs();
        fresh.commit(start)?;
        Ok(fresh)
    }

    /// Copy the live tree depth-first into a fresh store (contiguous slots, no garbage names), then swap.
    /// Only the worker thread writes, so the copy under the read lock is consistent.
    fn compact(&self) -> Result<()> {
        let pk = rd(self).package_exts_clone();
        let mut fresh = fresh_store(
            self.cfg.state_dir.as_deref(),
            "index.new",
            pk,
            self.fingerprint,
        )?;
        {
            let old = rd(self);
            for &r in &old.roots {
                let nr = fresh.add_root(old.name_of(r), old.entry(r).d)?;
                let mut stack = vec![(r, nr)];
                while let Some((o, n)) = stack.pop() {
                    for c in old.children(o).into_iter().rev() {
                        let e = old.entry(c);
                        let nc = fresh.add_child(n, old.name_of(c), e.d)?;
                        if e.d.kind == KIND_DIR {
                            stack.push((c, nc));
                        }
                    }
                }
            }
            fresh.log_changes = old.log_changes;
        }
        fresh.take_logs();
        fresh.commit(self.last_event_id.load(Ordering::Relaxed))?;
        self.swap_store(fresh);
        Ok(())
    }

    fn collect_listings(&self, path: &[u8]) -> Vec<Listing> {
        let mut v = Vec::new();
        self.backend.crawl(
            &os(path),
            &self.excludes,
            self.cfg.crawl_threads,
            &mut |l| v.push(l),
        );
        self.reads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(Instant::now(), v.len() as u64);
        v.sort_by_key(|l| l.id);
        v
    }
}
