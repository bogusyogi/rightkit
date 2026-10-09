//! The live index: open, query, change subscription.

use crate::backend::*;
use crate::config::*;
#[cfg(feature = "content")]
use crate::error::Error;
use crate::error::Result;
use crate::names::NameStore;
use crate::query::Query;
use crate::snapshot;
use crate::store::Store;
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
}

// ------------------------------------------------------------------ shared state

struct Sub {
    tx: SyncSender<ChangeBatch>,
    missed: u32,
}

struct Shared {
    store: RwLock<Store>,
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

    /// Blocks until the first crawl (or snapshot load) finished; unreadable roots are reported in `status()`.
    pub fn open_with_backend(cfg: Config, backend: Arc<dyn Backend>) -> Result<Index> {
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
        };
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
        for p in &pkg {
            fnv(&mut fp, p);
        }

        let names = NameStore::new(cfg.state_dir.as_ref().map(|d| d.join("names")))?;
        let mut store = Store::new(names, pkg);

        // Snapshot.
        let snap_path = cfg.state_dir.as_ref().map(|d| d.join("index.snapshot"));
        let mut resumed = None;
        if let Some(sp) = &snap_path {
            if sp.exists() {
                match snapshot::load(&mut store, sp, fp) {
                    Ok(l)
                        if snapshot::now_secs().saturating_sub(l.saved_at)
                            <= cfg.snapshot_max_age.as_secs()
                            && l.event_id <= backend.current_event_id() =>
                    {
                        resumed = Some(l.event_id)
                    }
                    _ => {
                        // Unusable: start from an empty store.
                        let names =
                            NameStore::new(cfg.state_dir.as_ref().map(|d| d.join("names")))?;
                        let pk = store_pkg(&store);
                        store = Store::new(names, pk);
                    }
                }
            }
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
            for r in &roots {
                crawl_root(&*backend, &excludes, cfg.crawl_threads, &mut store, r)?;
            }
        }
        status.crawl_time = t0.elapsed();
        for r in &roots {
            status.roots.push(root_status(&*backend, &store, r));
        }
        store.take_logs();

        #[cfg(feature = "content")]
        let content = Mutex::new(crate::content::ContentIndex::new(&cfg));
        let shared = Arc::new(Shared {
            store: RwLock::new(store),
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
            shared: shared.clone(),
            pending: Pending::default(),
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
        let store = rd(&self.shared);
        st.entries = store.len();
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
            let e = store.entry(s);
            if e.d.kind == KIND_DIR && e.d.flags & FLAG_UNREADABLE != 0 {
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
            let nid = store.entry(s).name as usize;
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
                    stack.extend_from_slice(store.children(x));
                }
            }
        }
        let mut v: Vec<(i32, u32)> = heap
            .into_iter()
            .map(|std::cmp::Reverse((sc, std::cmp::Reverse(s)))| (sc, s))
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        Ok(v.into_iter()
            .map(|(score, s)| Hit {
                item: item_of(&store, s),
                score,
            })
            .collect())
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
            .iter()
            .map(|&c| (c, size_of(&store, c)))
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
        let Some(d) = &self.shared.cfg.state_dir else {
            return Ok(());
        };
        std::fs::create_dir_all(d)?;
        let store = rd(&self.shared);
        snapshot::save(
            &store,
            &d.join("index.snapshot"),
            self.shared.fingerprint,
            self.shared.last_event_id.load(Ordering::Relaxed),
        )
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

fn store_pkg(_s: &Store) -> Vec<Vec<u8>> {
    _s.package_exts_clone()
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
            let ls = backend.crawl(root, excludes, threads);
            apply_listings(store, slot, &ls)?;
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
    pending: Pending,
}

impl Worker {
    fn run(mut self, rx: Receiver<WatchMsg>) {
        let coalesce = self.shared.cfg.change_coalesce;
        loop {
            let msg = match self.pending.first_at {
                Some(t) => {
                    let due = t + coalesce;
                    match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(m) => Some(m),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            self.emit();
                            continue;
                        }
                        Err(_) => return,
                    }
                }
                None => match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => return,
                },
            };
            let mut events: Vec<Event> = Vec::new();
            let mut acks: Vec<Sender<()>> = Vec::new();
            let mut stop = false;
            let mut next = msg;
            let mut drained = 0;
            while let Some(m) = next.take() {
                match m {
                    WatchMsg::Events(mut e) => events.append(&mut e),
                    WatchMsg::Flush(a) => acks.push(a),
                    WatchMsg::Stop => stop = true,
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
        let mut full = false;
        let mut max_id = sh.last_event_id.load(Ordering::Relaxed);
        for ev in events {
            if ev.id != 0 {
                max_id = max_id.max(ev.id);
            }
            if ev.flags & (EV_USER_DROPPED | EV_KERNEL_DROPPED) != 0 {
                full = true;
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
        if full {
            self.full_rescan();
        } else {
            for p in subtrees {
                self.rescan_subtree(&p);
            }
            for p in dirs {
                let mut done = HashSet::new();
                self.sync_path(&p, &mut done);
            }
        }
        sh.last_event_id.store(max_id, Ordering::Relaxed);
        self.after_apply();
    }

    fn after_apply(&mut self) {
        let (removed, touched) = wr(&self.shared).take_logs();
        #[cfg(feature = "content")]
        self.shared.content_apply(&removed, &touched);
        #[cfg(not(feature = "content"))]
        let _ = (removed, touched);
    }

    fn full_rescan(&mut self) {
        let sh = self.shared.clone();
        let names = match NameStore::new(sh.cfg.state_dir.as_ref().map(|d| d.join("names"))) {
            Ok(n) => n,
            Err(_) => NameStore::new(None).expect("anonymous name store"),
        };
        let pk = rd(&sh).package_exts_clone();
        let mut fresh = Store::new(names, pk);
        for r in &sh.roots {
            let _ = crawl_root(
                &*sh.backend,
                &sh.excludes,
                sh.cfg.crawl_threads,
                &mut fresh,
                r,
            );
        }
        fresh.take_logs();
        *wr(&sh) = fresh;
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
    }

    fn rescan_subtree(&mut self, path: &[u8]) {
        let sh = self.shared.clone();
        if sh.excludes.matches(path) {
            return;
        }
        let slot = rd(&sh).lookup(path);
        let Some(slot) = slot else {
            if let Some(p) = parent_of(path) {
                let mut done = HashSet::new();
                self.sync_path(p, &mut done);
            }
            return;
        };
        let ls = sh
            .backend
            .crawl(&os(path), &sh.excludes, sh.cfg.crawl_threads);
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
            let ls = sh
                .backend
                .crawl(&os(&p), &sh.excludes, sh.cfg.crawl_threads);
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
        || old.mtime != new.mtime
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
