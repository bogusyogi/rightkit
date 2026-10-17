//! Byte-oriented disk cache with a hard size cap.
//!
//! Improves on the MailRight `attach_cache` / `webimg` pattern it replaces:
//! eviction runs on every write (donor: every 20th), recency is updated on
//! reads (donor: write time only), recency survives restarts (file mtime),
//! writes are atomic (temp file + rename), and groups (an account, a URL
//! family) can be invalidated without a side index.
//!
//! Producers that write files (ffmpeg thumbnails, frames, waveforms) reserve a
//! temp path with [`DiskCache::reserve_path`] and commit it atomically; see
//! [`CacheReservation`]. [`DiskCache::open_read_only`] inspects a cache without
//! touching the filesystem at all.
//!
//! Layout: `<root>/<hh>/<keyhash>-<group>[.<ext>]` plus `<root>/.tmp/` for
//! in-flight writes. One writable `DiskCache` per directory per process: the
//! in-memory index is rebuilt on open, not kept coherent with other writers.
//!
//! The root must be exclusive to the cache. It holds `<hh>/` shards, `.tmp/` and an optional
//! `.pinned` list, and nothing else: [`DiskCache::open`] indexes the shard directories and sweeps
//! only temp files whose names the cache writes. Files the cache did not write are left alone, and
//! [`DiskCache::open_strict`] refuses to open a root that contains any. Keep other state in its own
//! directory.
//!
//! Entries can be pinned ([`DiskCache::put_pinned`], [`DiskCache::pin`]). Pinned entries are never
//! evicted by size pressure; their bytes count toward the cap, and a write that would leave no room
//! for them fails with [`PinnedCapacityError`]. Pins persist in `<root>/.pinned`.

use crate::key::{group_id, Key};
use crate::reservation::CacheReservation;
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Length of `<hash>-<group>` before any extension: 64 hex + `-` + 16 hex.
const BASE_LEN: usize = 64 + 1 + 16;
const MAX_EXT_LEN: usize = 8;
/// Default age after which a temp file in `<root>/.tmp/` is removed on open, even when its owner
/// looks alive. Set per cache with [`Config::stale_tmp_after`].
pub const DEFAULT_STALE_TMP_AGE: Duration = Duration::from_secs(60 * 60);
/// Sidecar suffix holding the pid that created a temp file. It contains `-`, which an extension
/// can never contain, so it cannot collide with a producer's output name.
const OWNER_SUFFIX: &str = ".owner-pid";
/// Root-level file listing the pinned entries' base names, one per line.
const PIN_FILE: &str = ".pinned";

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Total byte budget; the cache never stays above it after a write returns.
    pub max_bytes: u64,
    /// Entries larger than this are refused (webimg used 8 MB). `None` = only `max_bytes` applies.
    pub max_entry_bytes: Option<u64>,
    /// Temp files older than this are swept on open even when their owner is alive.
    /// Defaults to [`DEFAULT_STALE_TMP_AGE`] (1 hour).
    pub stale_tmp_after: Duration,
    /// Entries, temp files and the cache's directories are created owner-only: `0600` files and
    /// `0700` directories on Unix; on Windows a protected DACL for the current user. Default `false`,
    /// which keeps the platform's default permissions. Set with [`Config::owner_only`].
    pub owner_only: bool,
}

impl Config {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            max_entry_bytes: None,
            stale_tmp_after: DEFAULT_STALE_TMP_AGE,
            owner_only: false,
        }
    }

    /// Create entries and temp files owner-only (see [`Config::owner_only`]).
    pub fn owner_only(mut self) -> Self {
        self.owner_only = true;
        self
    }

    pub fn max_entry_bytes(mut self, n: u64) -> Self {
        self.max_entry_bytes = Some(n);
        self
    }

    pub fn stale_tmp_after(mut self, age: Duration) -> Self {
        self.stale_tmp_after = age;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub entries: usize,
    pub bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    /// Last-use time of the least recently used entry; `None` when empty.
    /// Uses the cache clock: wall time within a process, seeded from file mtimes on open.
    pub oldest: Option<SystemTime>,
    /// Last-use time of the most recently used entry; `None` when empty.
    pub newest: Option<SystemTime>,
}

/// Runtime figures of an open cache, computed on call by [`DiskCache::status`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiskCacheStatus {
    /// Entries, bytes, hits, misses and evictions (the cache's own backlog and churn figures).
    pub stats: Stats,
    /// The configured byte cap.
    pub max_bytes: u64,
    /// Resident memory, CPU time and I/O counters of the whole process (the cache runs in it and
    /// has no thread of its own), or `None` when the host cannot report them. See
    /// `rightkit_child_process::ProcessUsage`.
    pub process: Option<rightkit_child_process::ProcessUsage>,
}

/// What [`DiskCache::clear`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cleared {
    pub entries: usize,
    /// Bytes of the removed entries.
    pub bytes: u64,
}

/// What [`DiskCache::move_to`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MoveReport {
    /// Entries renamed (or copied then removed) into the new root.
    pub moved: usize,
    /// Entries whose destination file already existed: the destination was kept and the source
    /// removed.
    pub redundant: usize,
    /// One message per entry that could not be moved (`<base name>: <error>`). Those entries are
    /// no longer indexed; a file that still exists is left in the old root.
    pub failed: Vec<String>,
}

/// Files under the cache root that the cache did not write (see [`DiskCache::foreign_files`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ForeignFiles {
    pub count: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Stored,
    /// Larger than `max_entry_bytes` or `max_bytes`; nothing was written and existing data is untouched.
    TooLarge,
}

/// A write refused because pinned entries leave no room under `max_bytes`. Carried in an
/// `io::Error` of kind [`io::ErrorKind::StorageFull`]; read it with [`pinned_capacity_error`].
/// Nothing is evicted and nothing is written when this is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedCapacityError {
    /// Bytes held by pinned entries other than the one being written.
    pub pinned_bytes: u64,
    /// Bytes of the entry being written.
    pub entry_bytes: u64,
    /// The configured byte cap.
    pub max_bytes: u64,
}

impl fmt::Display for PinnedCapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pinned entries hold {} bytes; a {}-byte entry would exceed the {}-byte cap",
            self.pinned_bytes, self.entry_bytes, self.max_bytes
        )
    }
}

impl std::error::Error for PinnedCapacityError {}

/// The typed cause of a [`PinnedCapacityError`] write refusal, if `err` is one.
pub fn pinned_capacity_error(err: &io::Error) -> Option<&PinnedCapacityError> {
    err.get_ref()?.downcast_ref::<PinnedCapacityError>()
}

fn pinned_error(pinned_bytes: u64, entry_bytes: u64, max_bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::StorageFull,
        PinnedCapacityError {
            pinned_bytes,
            entry_bytes,
            max_bytes,
        },
    )
}

/// [`DiskCache::open_strict`] found files the cache did not write. Carried in an `io::Error` of
/// kind `InvalidData`; read it with [`foreign_content_error`]. Nothing was deleted or indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignContentError {
    /// Number of foreign files found under the root (outside shards it indexes and outside `.tmp/`
    /// names it writes).
    pub count: usize,
    /// One of them.
    pub first: PathBuf,
}

impl fmt::Display for ForeignContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cache root holds {} file(s) the cache did not write (first: {}); use a directory the cache owns",
            self.count,
            self.first.display()
        )
    }
}

impl std::error::Error for ForeignContentError {}

/// The typed cause of a strict-open refusal, if `err` is one.
pub fn foreign_content_error(err: &io::Error) -> Option<&ForeignContentError> {
    err.get_ref()?.downcast_ref::<ForeignContentError>()
}

fn foreign_error(count: usize, first: PathBuf) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        ForeignContentError { count, first },
    )
}

fn io_other(error: impl fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

/// Create a directory, owner-only when asked.
fn ensure_dir(dir: &Path, owner_only: bool) -> io::Result<()> {
    if owner_only {
        rightkit_fs::private::ensure_private_dir(dir).map_err(io_other)
    } else {
        fs::create_dir_all(dir)
    }
}

/// Create a new temp file. `owner_only` creates it `0600` from the start on Unix and applies the
/// owner-only DACL on Windows right after creation.
fn create_tmp_file(path: &Path, owner_only: bool) -> io::Result<File> {
    if !owner_only {
        return File::create(path);
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(not(unix))]
    rightkit_fs::private::restrict_owner_only(path).map_err(io_other)?;
    Ok(file)
}

/// Whether `name` is a temp file (or owner sidecar) this crate writes: `<pid>-<seq>` with an optional
/// extension, optionally followed by `.owner-pid`. The sweep and the strict open touch only these.
fn is_own_tmp_name(name: &str) -> bool {
    let base = name.strip_suffix(OWNER_SUFFIX).unwrap_or(name);
    let (stem, ext) = match base.split_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (base, None),
    };
    if !ext.is_none_or(valid_ext) {
        return false;
    }
    match stem.split_once('-') {
        Some((pid, seq)) => {
            let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
            digits(pid) && digits(seq)
        }
        None => false,
    }
}

/// Owner-pinned entry base names listed in `<root>/.pinned`. A missing file pins nothing.
///
/// Any other read error is returned: if the list cannot be read the pins are unknown, and
/// treating them as empty would make pinned entries evictable.
fn read_pins(root: &Path) -> io::Result<HashSet<String>> {
    let bytes = match rightkit_fs::read_replaced(&root.join(PIN_FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(e),
    };
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::trim)
        .filter(|line| matches!(parse_entry_name(line), Some((_, None))))
        .map(str::to_owned)
        .collect())
}

struct Meta {
    len: u64,
    /// Nanoseconds since the epoch of last use; strictly increasing within a process.
    used: u128,
    /// Extension of the stored file (`<base>.<ext>`); `None` for byte-written entries.
    ext: Option<String>,
    /// Exempt from size-pressure eviction. Persisted in `<root>/.pinned`.
    pinned: bool,
}

/// Bytes held by pinned entries.
fn pinned_total(st: &State) -> u64 {
    st.index.pinned_bytes()
}

/// The in-memory index: every stored entry by base name, plus the eviction order and the pinned
/// total. The methods that change an entry also change `recency` and `pinned_bytes`, so size
/// pressure reads the front of `recency` and never sorts or walks the entries.
#[derive(Default)]
struct Index {
    entries: HashMap<String, Meta>,
    /// `(last use, base name)` of every unpinned entry, least recently used first. This is the
    /// eviction order: entries that share a last-use time break ties by base name.
    recency: BTreeSet<(u128, String)>,
    /// Bytes held by pinned entries.
    pinned_bytes: u64,
    /// Whole-index walks ([`Index::iter`], [`Index::keys`], [`Index::values`]), counted by the unit
    /// test that asserts the write path walks none.
    #[cfg(test)]
    walks: Cell<u64>,
}

impl Index {
    fn walked(&self) {
        #[cfg(test)]
        {
            self.walks.set(self.walks.get() + 1);
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn get(&self, name: &str) -> Option<&Meta> {
        self.entries.get(name)
    }

    fn contains_key(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Every entry. A whole-index walk: not for the write path.
    fn values(&self) -> impl Iterator<Item = &Meta> {
        self.walked();
        self.entries.values()
    }

    /// Every entry. A whole-index walk: not for the write path.
    fn iter(&self) -> impl Iterator<Item = (&String, &Meta)> {
        self.walked();
        self.entries.iter()
    }

    /// Every base name. A whole-index walk: not for the write path.
    fn keys(&self) -> impl Iterator<Item = &String> {
        self.walked();
        self.entries.keys()
    }

    fn pinned_bytes(&self) -> u64 {
        self.pinned_bytes
    }

    /// Base name of the least recently used unpinned entry.
    fn least_recent(&self) -> Option<&str> {
        self.recency.first().map(|(_, name)| name.as_str())
    }

    /// Unpinned entries as `(last use, base name)`, least recently used first.
    fn recency_order(&self) -> impl Iterator<Item = &(u128, String)> {
        self.recency.iter()
    }

    /// Insert or replace `name`, returning the entry it replaced.
    fn insert(&mut self, name: String, meta: Meta) -> Option<Meta> {
        let prev = self.remove(&name);
        if meta.pinned {
            self.pinned_bytes += meta.len;
        } else {
            self.recency.insert((meta.used, name.clone()));
        }
        self.entries.insert(name, meta);
        prev
    }

    fn remove(&mut self, name: &str) -> Option<Meta> {
        let meta = self.entries.remove(name)?;
        if meta.pinned {
            self.pinned_bytes = self.pinned_bytes.saturating_sub(meta.len);
        } else {
            self.recency.remove(&(meta.used, name.to_owned()));
        }
        Some(meta)
    }

    /// Set the last-use time of `name`, moving it within the eviction order when it is unpinned.
    fn touch(&mut self, name: &str, used: u128) {
        let Some(meta) = self.entries.get_mut(name) else {
            return;
        };
        if !meta.pinned {
            self.recency.remove(&(meta.used, name.to_owned()));
            self.recency.insert((used, name.to_owned()));
        }
        meta.used = used;
    }

    /// Pin or unpin `name`. Returns `false` when it is absent.
    fn set_pinned(&mut self, name: &str, pinned: bool) -> bool {
        let Some(meta) = self.entries.get_mut(name) else {
            return false;
        };
        if meta.pinned == pinned {
            return true;
        }
        if pinned {
            self.recency.remove(&(meta.used, name.to_owned()));
            self.pinned_bytes += meta.len;
        } else {
            self.recency.insert((meta.used, name.to_owned()));
            self.pinned_bytes = self.pinned_bytes.saturating_sub(meta.len);
        }
        meta.pinned = pinned;
        true
    }
}

#[derive(Default)]
pub(crate) struct State {
    /// Keyed by base name `<hash>-<group>`; one slot per key whatever its extension.
    index: Index,
    bytes: u64,
    clock: u128,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl State {
    fn tick(&mut self) -> u128 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        self.clock = now.max(self.clock + 1);
        self.clock
    }
}

struct Inner {
    /// The root the handle was opened with (what [`DiskCache::root`] returns).
    root: PathBuf,
    /// Where the cache lives now; changed only by [`DiskCache::move_to`].
    live_root: RwLock<PathBuf>,
    /// The byte cap; changed by [`DiskCache::set_budget`].
    budget: AtomicU64,
    cfg: Config,
    /// `false` for [`DiskCache::open_read_only`]: no filesystem mutation of any kind.
    writable: bool,
    state: Mutex<State>,
}

/// Cheaply cloneable handle (shared index).
#[derive(Clone)]
pub struct DiskCache {
    inner: Arc<Inner>,
}

fn entry_name(key: &Key) -> String {
    format!("{}-{}", key.hash, key.group)
}

/// Split a stored file name into its base slot name and optional extension.
fn parse_entry_name(name: &str) -> Option<(&str, Option<&str>)> {
    let (base, ext) = match name.split_once('.') {
        Some((base, ext)) => (base, Some(ext)),
        None => (name, None),
    };
    let b = base.as_bytes();
    let base_ok = b.len() == BASE_LEN
        && b[64] == b'-'
        && base.bytes().all(|c| c == b'-' || c.is_ascii_hexdigit());
    let ext_ok = match ext {
        Some(ext) => valid_ext(ext),
        None => true,
    };
    (base_ok && ext_ok).then_some((base, ext))
}

/// Extensions are 1 to 8 ASCII alphanumerics, so they can never add a path separator or a dot.
pub(crate) fn valid_ext(ext: &str) -> bool {
    (1..=MAX_EXT_LEN).contains(&ext.len()) && ext.bytes().all(|c| c.is_ascii_alphanumeric())
}

pub(crate) fn unique_tmp_name(ext: Option<&str>) -> String {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    match ext {
        Some(ext) => format!("{}-{seq}.{ext}", std::process::id()),
        None => format!("{}-{seq}", std::process::id()),
    }
}

fn system_time(nanos: u128) -> SystemTime {
    let secs = u64::try_from(nanos / 1_000_000_000).unwrap_or(u64::MAX);
    let sub = (nanos % 1_000_000_000) as u32;
    UNIX_EPOCH
        .checked_add(Duration::new(secs, sub))
        .unwrap_or(UNIX_EPOCH)
}

/// `<root>/<hh>/<name>[.<ext>]`.
fn entry_path(root: &Path, name: &str, ext: Option<&str>) -> PathBuf {
    let file = match ext {
        Some(ext) => format!("{name}.{ext}"),
        None => name.to_owned(),
    };
    root.join(&name[..2]).join(file)
}

fn read_only_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "cache was opened read-only",
    )
}

fn owner_path(tmp: &Path) -> PathBuf {
    let mut name = tmp.as_os_str().to_owned();
    name.push(OWNER_SUFFIX);
    PathBuf::from(name)
}

/// Record this process as the owner of `tmp`. Best effort: a temp file without a sidecar is
/// judged by age alone.
fn record_owner(tmp: &Path, owner_only: bool) {
    let pid = std::process::id().to_string();
    if owner_only {
        let _ = rightkit_fs::private::write_private_file(&owner_path(tmp), pid.as_bytes());
    } else {
        let _ = fs::write(owner_path(tmp), pid);
    }
}

fn read_owner(tmp: &Path) -> Option<u32> {
    fs::read_to_string(owner_path(tmp))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Remove a temp file and its owner sidecar. Used on abort, failed commits and drops.
pub(crate) fn remove_tmp(tmp: &Path) {
    let _ = fs::remove_file(tmp);
    let _ = fs::remove_file(owner_path(tmp));
}

/// Move a finished temp file into place and drop its sidecar.
fn promote(tmp: &Path, dest: &Path) -> io::Result<()> {
    fs::rename(tmp, dest)?;
    let _ = fs::remove_file(owner_path(tmp));
    Ok(())
}

/// Remove crash leftovers from `<root>/.tmp/`: files whose owner pid is dead, and files older than
/// `stale_after`. Young files of live owners are kept, so opening the cache from a second
/// process never deletes another process's in-flight output.
fn sweep_tmp(tmp: &Path, stale_after: Duration) -> io::Result<()> {
    let now = SystemTime::now();
    let mut sidecars = Vec::new();
    for entry in fs::read_dir(tmp)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Only names this crate writes are ours. A file of any other name in `.tmp/` is left alone,
        // whatever its age or owner.
        if !is_own_tmp_name(&name) {
            continue;
        }
        if name.ends_with(OWNER_SUFFIX) {
            sidecars.push((path, name));
            continue;
        }
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let owner_dead = read_owner(&path).is_some_and(|pid| !rightkit_fs::process_alive(pid));
        let aged = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > stale_after);
        if owner_dead || aged {
            remove_tmp(&path);
        }
    }
    // Sidecars whose temp file is gone (removed above, or lost in a crash) are orphans.
    for (path, name) in sidecars {
        let target = name.trim_end_matches(OWNER_SUFFIX);
        if !tmp.join(target).exists() {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn touch(path: &Path) {
    let _ = OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(SystemTime::now()));
}

/// Paths and sizes of regular files under `root` that are not cache entries or cache temp files.
fn foreign_paths(root: &Path) -> io::Result<Vec<(PathBuf, u64)>> {
    let tmp = root.join(".tmp");
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if path != tmp {
                    dirs.push(path);
                }
                continue;
            }
            if dir.as_path() == root && entry.file_name().to_str() == Some(PIN_FILE) {
                continue;
            }
            if is_cache_entry(root, &path) {
                continue;
            }
            let len = fs::symlink_metadata(&path)?.len();
            found.push((path, len));
        }
    }
    Ok(found)
}

/// Files directly in `<root>/.tmp/` whose names the cache does not write.
fn foreign_tmp_entries(root: &Path) -> io::Result<Vec<(PathBuf, u64)>> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(root.join(".tmp")) else {
        return Ok(found);
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = entry.metadata()?;
        if meta.is_file() && !is_own_tmp_name(&name) {
            found.push((entry.path(), meta.len()));
        }
    }
    Ok(found)
}

/// Whether `path` is an indexed-style entry file directly inside a `<hh>` shard of `root`.
fn is_cache_entry(root: &Path, path: &Path) -> bool {
    let (Some(shard), Some(name)) = (path.parent().and_then(Path::file_name), path.file_name())
    else {
        return false;
    };
    let shard = shard.to_string_lossy();
    let name = name.to_string_lossy();
    if path.parent().and_then(Path::parent) != Some(root) || shard.len() != 2 {
        return false;
    }
    matches!(parse_entry_name(&name), Some((base, _)) if base.starts_with(shard.as_ref()))
}

impl DiskCache {
    /// Open (creating if needed) and index an existing cache directory, discarding crash leftovers
    /// and trimming to `cfg.max_bytes` (the cap may have shrunk since the last run).
    ///
    /// Fails, evicting nothing, if the pin list `<root>/.pinned` exists but cannot be read: with
    /// the pins unknown the trim could evict an entry the owner pinned. An absent list pins
    /// nothing.
    pub fn open(root: impl Into<PathBuf>, cfg: Config) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let tmp = root.join(".tmp");
        ensure_dir(&tmp, cfg.owner_only)?;
        // Best effort: a sweep failure must not stop the cache from opening.
        let _ = sweep_tmp(&tmp, cfg.stale_tmp_after);
        let cache = Self::scan(root, cfg, true)?;
        {
            let mut st = cache.lock();
            cache.evict_locked(&mut st, 0);
        }
        Ok(cache)
    }

    /// [`DiskCache::open`], but refuses a root that holds any file the cache did not write.
    ///
    /// Returns an `io::Error` carrying a [`ForeignContentError`] (see [`foreign_content_error`]) and
    /// touches nothing: no file is deleted, indexed or created. Use it when the directory might be
    /// shared with other state. A root that does not exist yet is created, as with `open`.
    pub fn open_strict(root: impl Into<PathBuf>, cfg: Config) -> io::Result<Self> {
        let root = root.into();
        if root.is_dir() {
            let mut found = foreign_paths(&root)?;
            found.extend(foreign_tmp_entries(&root)?);
            if let Some((first, _)) = found.first() {
                return Err(foreign_error(found.len(), first.clone()));
            }
        }
        Self::open(root, cfg)
    }

    /// Index an existing cache directory without any filesystem writes.
    ///
    /// Unlike [`DiskCache::open`], this does not create the root, does not remove `.tmp` leftovers,
    /// does not trim to the cap and does not refresh file mtimes. Reads, [`DiskCache::stats`] and
    /// [`DiskCache::contains`] work normally; every mutating call ([`DiskCache::put`],
    /// [`DiskCache::reserve_path`], [`DiskCache::remove`], [`DiskCache::clear`], ...) fails or
    /// does nothing. The root must already exist. The returned handle reports the `Config::new(u64::MAX)`
    /// budget because a read-only handle never evicts.
    pub fn open_read_only(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cache root does not exist",
            ));
        }
        Self::scan(root, Config::new(u64::MAX), false)
    }

    /// Build the in-memory index from files on disk. Never writes; `writable` only controls
    /// whether a duplicate slot's older file is deleted.
    fn scan(root: PathBuf, cfg: Config, writable: bool) -> io::Result<Self> {
        let mut found: Vec<(String, Option<String>, u64, u128)> = Vec::new();
        for shard in fs::read_dir(&root)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() || shard.file_name().len() != 2 {
                continue;
            }
            let shard_name = shard.file_name().to_string_lossy().into_owned();
            for f in fs::read_dir(shard.path())? {
                let f = f?;
                let name = f.file_name().to_string_lossy().into_owned();
                let Some((base, ext)) = parse_entry_name(&name) else {
                    continue;
                };
                let meta = f.metadata()?;
                if !meta.is_file() || !base.starts_with(&shard_name) {
                    continue;
                }
                let used = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                found.push((base.to_owned(), ext.map(str::to_owned), meta.len(), used));
            }
        }
        // Deterministic recency order even when mtimes tie.
        found.sort_by(|a, b| a.3.cmp(&b.3).then_with(|| a.0.cmp(&b.0)));
        let pins = read_pins(&root)?;

        let cache = Self {
            inner: Arc::new(Inner {
                live_root: RwLock::new(root.clone()),
                budget: AtomicU64::new(cfg.max_bytes),
                root,
                cfg,
                writable,
                state: Mutex::new(State::default()),
            }),
        };
        {
            let mut st = cache.lock();
            for (base, ext, len, used) in found {
                st.clock = st.clock.max(used);
                cache.index_insert(&mut st, &base, len, used, ext.as_deref());
            }
            for base in pins {
                st.index.set_pinned(&base, true);
            }
        }
        Ok(cache)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn path_for(&self, name: &str, ext: Option<&str>) -> PathBuf {
        entry_path(&self.base(), name, ext)
    }

    /// Record `base` in the index, replacing any previous slot entry for the same key.
    fn index_insert(&self, st: &mut State, base: &str, len: u64, used: u128, ext: Option<&str>) {
        // Replacing an entry keeps its pin: a rewrite never unpins.
        let pinned = st.index.get(base).is_some_and(|m| m.pinned);
        let prev = st.index.insert(
            base.to_owned(),
            Meta {
                len,
                used,
                ext: ext.map(str::to_owned),
                pinned,
            },
        );
        if let Some(prev) = prev {
            st.bytes = st.bytes.saturating_sub(prev.len);
            if self.inner.writable && prev.ext.as_deref() != ext {
                let _ = fs::remove_file(self.path_for(base, prev.ext.as_deref()));
            }
        }
        st.bytes += len;
    }

    /// The root this handle was opened with. After [`DiskCache::move_to`] this is still the
    /// original root; use [`DiskCache::location`] for the current one.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// The directory the cache lives in now: the opening root until [`DiskCache::move_to`]
    /// relocates it.
    pub fn location(&self) -> PathBuf {
        self.base()
    }

    /// The configuration, with `max_bytes` as last set by [`DiskCache::set_budget`].
    pub fn config(&self) -> Config {
        Config {
            max_bytes: self.max_bytes(),
            ..self.inner.cfg
        }
    }

    fn base(&self) -> PathBuf {
        self.inner
            .live_root
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn max_bytes(&self) -> u64 {
        self.inner.budget.load(Ordering::Relaxed)
    }

    /// `true` unless the handle came from [`DiskCache::open_read_only`].
    pub fn is_writable(&self) -> bool {
        self.inner.writable
    }

    /// Cache figures plus process resident memory and CPU, read now. No polling thread runs for this.
    pub fn status(&self) -> DiskCacheStatus {
        DiskCacheStatus {
            stats: self.stats(),
            max_bytes: self.config().max_bytes,
            process: rightkit_child_process::process_usage().ok(),
        }
    }

    pub fn stats(&self) -> Stats {
        let st = self.lock();
        Stats {
            entries: st.index.len(),
            bytes: st.bytes,
            hits: st.hits,
            misses: st.misses,
            evictions: st.evictions,
            oldest: st.index.values().map(|m| m.used).min().map(system_time),
            newest: st.index.values().map(|m| m.used).max().map(system_time),
        }
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.lock().index.contains_key(&entry_name(key))
    }

    /// Read an entry and mark it most-recently-used (in memory and on disk, so recency survives a restart).
    /// A read-only handle marks recency in memory only.
    pub fn get(&self, key: &Key) -> Option<Vec<u8>> {
        let name = entry_name(key);
        let path = {
            let mut st = self.lock();
            let ext = st.index.get(&name).map(|m| m.ext.clone());
            match ext {
                Some(ext) => self.path_for(&name, ext.as_deref()),
                None => {
                    st.misses += 1;
                    return None;
                }
            }
        };
        let read = if self.inner.writable {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .or_else(|_| File::open(&path))
                .and_then(|mut f| -> io::Result<Vec<u8>> {
                    let mut buf = Vec::new();
                    f.read_to_end(&mut buf)?;
                    let _ = f.set_modified(SystemTime::now());
                    Ok(buf)
                })
        } else {
            File::open(&path).and_then(|mut f| -> io::Result<Vec<u8>> {
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                Ok(buf)
            })
        };
        let mut st = self.lock();
        match read {
            Ok(bytes) => {
                let used = st.tick();
                st.index.touch(&name, used);
                st.hits += 1;
                Some(bytes)
            }
            Err(_) => {
                // Removed or unreadable behind our back: forget it.
                if let Some(m) = st.index.remove(&name) {
                    st.bytes = st.bytes.saturating_sub(m.len);
                }
                st.misses += 1;
                None
            }
        }
    }

    /// Path of a stored file entry (see [`DiskCache::reserve_path`]), marked most-recently-used.
    /// Returns `None` for a missing key or a file removed behind the cache's back.
    pub fn get_path(&self, key: &Key) -> Option<PathBuf> {
        let name = entry_name(key);
        let mut st = self.lock();
        let found = st.index.get(&name).map(|m| m.ext.clone());
        let Some(ext) = found else {
            st.misses += 1;
            return None;
        };
        let path = self.path_for(&name, ext.as_deref());
        if !path.is_file() {
            if let Some(m) = st.index.remove(&name) {
                st.bytes = st.bytes.saturating_sub(m.len);
            }
            st.misses += 1;
            return None;
        }
        let used = st.tick();
        st.index.touch(&name, used);
        st.hits += 1;
        drop(st);
        if self.inner.writable {
            touch(&path);
        }
        Some(path)
    }

    /// Store `bytes` atomically, then evict least-recently-used unpinned entries until the cap holds.
    /// Replacing an entry keeps its pin. Fails with [`PinnedCapacityError`] (kind `StorageFull`) when
    /// pinned entries leave no room for `bytes`; nothing is written or evicted then.
    pub fn put(&self, key: &Key, bytes: &[u8]) -> io::Result<PutOutcome> {
        self.put_with(key, bytes, false)
    }

    /// [`DiskCache::put`], and pin the entry: it is exempt from size-pressure eviction until
    /// [`DiskCache::unpin`]. The pin is persisted before this returns.
    pub fn put_pinned(&self, key: &Key, bytes: &[u8]) -> io::Result<PutOutcome> {
        self.put_with(key, bytes, true)
    }

    fn put_with(&self, key: &Key, bytes: &[u8], pin: bool) -> io::Result<PutOutcome> {
        if !self.inner.writable {
            return Err(read_only_error());
        }
        let len = bytes.len() as u64;
        if self.too_large(len) {
            return Ok(PutOutcome::TooLarge);
        }
        let name = entry_name(key);
        self.check_capacity(&name, len)?;
        let tmp = self.write_tmp(bytes)?;
        self.publish(&name, None, len, &tmp, pin)
    }

    /// Largest entry the cache accepts: the smaller of `max_bytes` and `max_entry_bytes`.
    fn entry_limit(&self) -> u64 {
        let cfg = self.config();
        cfg.max_entry_bytes
            .map_or(cfg.max_bytes, |m| m.min(cfg.max_bytes))
    }

    fn too_large(&self, len: u64) -> bool {
        len > self.entry_limit()
    }

    /// Refuse a write of `len` bytes for `name` when pinned entries other than `name` leave no room.
    /// Checked before any byte is written. A pin added concurrently after this check is not re-checked.
    fn check_capacity(&self, name: &str, len: u64) -> io::Result<()> {
        let max = self.max_bytes();
        let pinned = {
            let st = self.lock();
            // The entry being replaced does not count against its own write.
            let own = st.index.get(name).filter(|m| m.pinned).map_or(0, |m| m.len);
            st.index.pinned_bytes().saturating_sub(own)
        };
        if pinned.saturating_add(len) > max {
            return Err(pinned_error(pinned, len, max));
        }
        Ok(())
    }

    /// Move a finished temp file (`tmp`, holding `len` bytes) into place as `name`/`ext`, index it,
    /// and evict unpinned entries to the cap. On failure the temp file is removed.
    fn publish(
        &self,
        name: &str,
        ext: Option<&str>,
        len: u64,
        tmp: &Path,
        pin: bool,
    ) -> io::Result<PutOutcome> {
        let owner_only = self.inner.cfg.owner_only;
        let dest = self.path_for(name, ext);
        let prepared = (|| -> io::Result<()> {
            ensure_dir(dest.parent().expect("sharded path"), owner_only)?;
            if owner_only {
                rightkit_fs::private::restrict_owner_only(tmp).map_err(io_other)?;
            }
            Ok(())
        })()
        .and_then(|()| promote(tmp, &dest));
        if let Err(e) = prepared {
            remove_tmp(tmp);
            return Err(e);
        }
        let mut st = self.lock();
        let used = st.tick();
        self.index_insert(&mut st, name, len, used, ext);
        let persisted = if pin {
            st.index.set_pinned(name, true);
            self.persist_pins(&st)
        } else {
            Ok(())
        };
        self.evict_locked(&mut st, 0);
        persisted?;
        Ok(PutOutcome::Stored)
    }

    /// Start a streaming write of one entry, for values too large to hold in memory. Bytes go to a
    /// temp file in `<root>/.tmp/` through [`io::Write`]; [`PutWriter::commit`] publishes them
    /// atomically and accounts for them. Dropping the writer without committing removes the temp
    /// file and leaves the cache unchanged. Writing past the entry limit returns an error, and
    /// committing a writer that hit it returns [`PutOutcome::TooLarge`].
    pub fn put_writer(&self, key: &Key) -> io::Result<PutWriter> {
        if !self.inner.writable {
            return Err(read_only_error());
        }
        let owner_only = self.inner.cfg.owner_only;
        let tmp_dir = self.base().join(".tmp");
        ensure_dir(&tmp_dir, owner_only)?;
        let tmp = tmp_dir.join(unique_tmp_name(None));
        record_owner(&tmp, owner_only);
        let file = match create_tmp_file(&tmp, owner_only) {
            Ok(file) => file,
            Err(e) => {
                remove_tmp(&tmp);
                return Err(e);
            }
        };
        Ok(PutWriter {
            cache: self.clone(),
            name: entry_name(key),
            pin: false,
            tmp,
            file: Some(file),
            written: 0,
            limit: self.entry_limit(),
            exceeded: false,
            armed: true,
        })
    }

    /// Store a value read from `reader` without holding it in memory, as [`DiskCache::put`] does
    /// for a slice. The reader is copied in chunks into a [`PutWriter`]. Returns
    /// [`PutOutcome::TooLarge`] when it yields more than the entry limit.
    pub fn put_reader(&self, key: &Key, reader: &mut impl Read) -> io::Result<PutOutcome> {
        let mut writer = self.put_writer(key)?;
        match io::copy(reader, &mut writer) {
            Ok(_) => writer.commit(),
            Err(_) if writer.exceeded => Ok(PutOutcome::TooLarge),
            Err(e) => Err(e),
        }
    }

    /// Pin an existing entry, so size pressure never evicts it. Returns `Ok(false)` when the key is
    /// absent. Fails with [`PinnedCapacityError`] when pinned entries plus this one exceed the cap.
    pub fn pin(&self, key: &Key) -> io::Result<bool> {
        self.set_pinned(key, true)
    }

    /// Make a pinned entry evictable again. Returns `Ok(false)` when the key is absent.
    pub fn unpin(&self, key: &Key) -> io::Result<bool> {
        self.set_pinned(key, false)
    }

    fn set_pinned(&self, key: &Key, pinned: bool) -> io::Result<bool> {
        if !self.inner.writable {
            return Err(read_only_error());
        }
        let name = entry_name(key);
        let mut st = self.lock();
        let (already, len) = match st.index.get(&name) {
            Some(meta) => (meta.pinned, meta.len),
            None => return Ok(false),
        };
        if already == pinned {
            return Ok(true);
        }
        let max = self.max_bytes();
        if pinned {
            let others = pinned_total(&st);
            if others.saturating_add(len) > max {
                return Err(pinned_error(others, len, max));
            }
        }
        st.index.set_pinned(&name, pinned);
        if let Err(e) = self.persist_pins(&st) {
            st.index.set_pinned(&name, already);
            return Err(e);
        }
        Ok(true)
    }

    /// Bytes held by pinned entries. They count toward `max_bytes` and are never evicted by size
    /// pressure.
    pub fn pinned_bytes(&self) -> u64 {
        pinned_total(&self.lock())
    }

    /// Rewrite `<root>/.pinned` from the index: the pinned base names, or no file when none are pinned.
    fn persist_pins(&self, st: &State) -> io::Result<()> {
        self.persist_pins_at(st, &self.base())
    }

    fn persist_pins_at(&self, st: &State, root: &Path) -> io::Result<()> {
        let path = root.join(PIN_FILE);
        let mut names: Vec<&str> = st
            .index
            .iter()
            .filter(|(_, m)| m.pinned)
            .map(|(n, _)| n.as_str())
            .collect();
        if names.is_empty() {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        names.sort_unstable();
        let mut body = names.join("\n");
        body.push('\n');
        rightkit_fs::atomic_write(&path, body.as_bytes()).map_err(io_other)
    }

    /// Reserve a temp path for a producer (ffmpeg, an image encoder) to write a file for `key`.
    ///
    /// The path sits in `<root>/.tmp/` and ends in `.ext`, so tools that infer the output format
    /// from the extension work unchanged. Call [`CacheReservation::commit`] once the producer
    /// succeeds; [`CacheReservation::abort`], or simply dropping the reservation, removes the
    /// temp file and leaves the cache untouched. `ext` must be 1 to 8 ASCII alphanumerics.
    pub fn reserve_path(&self, key: &Key, ext: &str) -> io::Result<CacheReservation> {
        if !self.inner.writable {
            return Err(read_only_error());
        }
        if !valid_ext(ext) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache file extension must be 1 to 8 ASCII alphanumerics",
            ));
        }
        let tmp_dir = self.base().join(".tmp");
        ensure_dir(&tmp_dir, self.inner.cfg.owner_only)?;
        let tmp = tmp_dir.join(unique_tmp_name(Some(ext)));
        record_owner(&tmp, self.inner.cfg.owner_only);
        Ok(CacheReservation::new(
            self.clone(),
            entry_name(key),
            ext.to_owned(),
            tmp,
        ))
    }

    /// Move a producer's finished temp file into the cache under `name` (the base slot name).
    /// Used by [`CacheReservation::commit`]; `tmp` is always inside `<root>/.tmp/`.
    pub(crate) fn install_file(&self, name: &str, ext: &str, tmp: &Path) -> io::Result<PutOutcome> {
        if !self.inner.writable {
            return Err(read_only_error());
        }
        let meta = fs::metadata(tmp)?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "producer output is not a regular file",
            ));
        }
        let len = meta.len();
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "producer wrote an empty file",
            ));
        }
        if self.too_large(len) {
            return Ok(PutOutcome::TooLarge);
        }
        self.check_capacity(name, len)?;
        self.publish(name, Some(ext), len, tmp, false)
    }

    fn write_tmp(&self, bytes: &[u8]) -> io::Result<PathBuf> {
        let owner_only = self.inner.cfg.owner_only;
        let tmp = self.base().join(".tmp").join(unique_tmp_name(None));
        record_owner(&tmp, owner_only);
        let written = create_tmp_file(&tmp, owner_only).and_then(|mut f| f.write_all(bytes));
        if let Err(e) = written {
            remove_tmp(&tmp);
            return Err(e);
        }
        Ok(tmp)
    }

    /// Fetch-or-cache: on a miss call `fetch`, write the result through (best effort), return it.
    /// A fetch error leaves the cache untouched; a cache write failure never fails the caller.
    pub fn get_or_fetch<E>(
        &self,
        key: &Key,
        fetch: impl FnOnce() -> Result<Vec<u8>, E>,
    ) -> Result<Vec<u8>, E> {
        if let Some(bytes) = self.get(key) {
            return Ok(bytes);
        }
        let bytes = fetch()?;
        let _ = self.put(key, &bytes);
        Ok(bytes)
    }

    /// Remove one entry. Returns `false` if it was absent or the handle is read-only.
    pub fn remove(&self, key: &Key) -> bool {
        if !self.inner.writable {
            return false;
        }
        let name = entry_name(key);
        let mut st = self.lock();
        self.remove_entry(&mut st, &name).is_some()
    }

    /// Remove every entry stored under `Key::grouped(group_parts, ..)`; returns how many were removed.
    /// A read-only handle removes nothing and returns 0.
    pub fn clear_group(&self, group_parts: &[&str]) -> usize {
        if !self.inner.writable {
            return 0;
        }
        let suffix = format!("-{}", group_id(group_parts));
        let mut st = self.lock();
        let names: Vec<String> = st
            .index
            .keys()
            .filter(|n| n.ends_with(&suffix))
            .cloned()
            .collect();
        for n in &names {
            self.remove_entry(&mut st, n);
        }
        names.len()
    }

    /// Remove every entry and report how many entries and bytes were removed. A read-only handle
    /// removes nothing and reports zero. Files the cache did not write are untouched; see
    /// [`DiskCache::clear_foreign`].
    pub fn clear(&self) -> Cleared {
        if !self.inner.writable {
            return Cleared::default();
        }
        let mut st = self.lock();
        let names: Vec<String> = st.index.keys().cloned().collect();
        let mut bytes = 0u64;
        for n in &names {
            bytes += self.remove_entry(&mut st, n).unwrap_or(0);
        }
        st.bytes = 0;
        Cleared {
            entries: names.len(),
            bytes,
        }
    }

    /// The entry count from [`DiskCache::clear`], for callers that only need the number.
    pub fn clear_count(&self) -> usize {
        self.clear().entries
    }

    /// Regular files under the root that the cache did not write: anything outside the indexed
    /// `<hh>/<keyhash>-<group>[.<ext>]` entries and outside `<root>/.tmp/`. Directories are walked
    /// but not counted; symlinks are counted as files and never followed. Works on read-only
    /// handles.
    pub fn foreign_files(&self) -> io::Result<ForeignFiles> {
        let mut found = ForeignFiles::default();
        for (_, len) in foreign_paths(&self.base())? {
            found.count += 1;
            found.bytes += len;
        }
        Ok(found)
    }

    /// Delete the files [`DiskCache::foreign_files`] reports and return what was removed. Stops at
    /// the first failure and returns that error; files already removed stay removed. Leaves
    /// directories in place. A read-only handle fails with `PermissionDenied`.
    pub fn clear_foreign(&self) -> io::Result<ForeignFiles> {
        if !self.inner.writable {
            return Err(read_only_error());
        }
        let mut removed = ForeignFiles::default();
        for (path, len) in foreign_paths(&self.base())? {
            fs::remove_file(&path)?;
            removed.count += 1;
            removed.bytes += len;
        }
        Ok(removed)
    }

    /// Drop `name` from the index and delete its file. Returns the freed length if it was indexed.
    /// Callers have already checked `writable`.
    fn remove_entry(&self, st: &mut State, name: &str) -> Option<u64> {
        let m = st.index.remove(name)?;
        st.bytes = st.bytes.saturating_sub(m.len);
        let _ = fs::remove_file(self.path_for(name, m.ext.as_deref()));
        if m.pinned {
            let _ = self.persist_pins(st);
        }
        Some(m.len)
    }

    /// Evict LRU entries until `bytes + reserve <= max_bytes`. Each step takes the front of the
    /// index's eviction order, so the cost is the entries evicted, not the size of the index.
    fn evict_locked(&self, st: &mut State, reserve: u64) {
        let cap = self.max_bytes();
        while st.bytes + reserve > cap {
            let Some(name) = st.index.least_recent().map(str::to_owned) else {
                break;
            };
            if self.remove_entry(st, &name).is_some() {
                st.evictions += 1;
            }
        }
    }

    /// Evict one entry on behalf of a [`crate::CacheSet`] combined budget.
    /// Returns `None` for an absent key or a read-only handle.
    pub(crate) fn evict_base(&self, name: &str) -> Option<u64> {
        if !self.inner.writable {
            return None;
        }
        let mut st = self.lock();
        if st.index.get(name).is_some_and(|m| m.pinned) {
            return None;
        }
        let freed = self.remove_entry(&mut st, name)?;
        st.evictions += 1;
        Some(freed)
    }

    /// Change the byte cap without reopening, then evict least-recently-used unpinned entries
    /// until the cache fits. Returns what the shrink evicted (zero when the cap grew).
    ///
    /// Pinned entries are never evicted: when pinned bytes alone exceed the new cap the cache
    /// stays above it, and a later unpinned write fails with [`PinnedCapacityError`] until pins
    /// are released or the cap grows. The new cap applies to [`Config::max_entry_bytes`] checks
    /// and [`DiskCache::status`]; it is not persisted, so a reopen uses the `Config` given to
    /// `open`. A read-only handle fails with `PermissionDenied`.
    pub fn set_budget(&self, max_bytes: u64) -> io::Result<Cleared> {
        if !self.inner.writable {
            return Err(read_only_error());
        }
        let mut st = self.lock();
        self.inner.budget.store(max_bytes, Ordering::Relaxed);
        let (evictions, bytes) = (st.evictions, st.bytes);
        self.evict_locked(&mut st, 0);
        Ok(Cleared {
            entries: usize::try_from(st.evictions - evictions).unwrap_or(usize::MAX),
            bytes: bytes.saturating_sub(st.bytes),
        })
    }

    /// Relocate every entry to the root `to`, keeping keys, contents, recency (mtime), pins and
    /// the index, so the handle keeps working with no reopen and no rescan.
    ///
    /// Each entry is renamed, falling back to copy then remove when a rename is not possible
    /// (another volume); an entry never exists half-written at the destination. A destination
    /// file that already exists is kept and the source dropped (counted as `redundant`). The
    /// pin list is written to `to` before any entry moves, and the old root's pin list, empty
    /// shard directories and empty `.tmp` are removed afterwards (best effort); the old root
    /// directory itself and any foreign files in it are left alone. Per-entry failures are
    /// reported in [`MoveReport::failed`], those entries leave the cache, and the move carries on.
    ///
    /// Errors, with nothing moved: a read-only handle, `to` equal to the current root (that is an
    /// `Ok` no-op), or nested inside it or containing it (`InvalidInput`), or a `to` that cannot
    /// be created. The whole move holds the cache's lock; writes in flight when it starts (a
    /// [`PutWriter`] or reservation holding a temp file in the old root) can fail to commit, so
    /// move while idle. Hosts that move the cache between volumes get a copy of each entry, not
    /// one atomic switch.
    pub fn move_to(&self, to: impl AsRef<Path>) -> io::Result<MoveReport> {
        if !self.inner.writable {
            return Err(read_only_error());
        }
        let owner_only = self.inner.cfg.owner_only;
        let to = to.as_ref().to_path_buf();
        let mut st = self.lock();
        let mut live = self
            .inner
            .live_root
            .write()
            .unwrap_or_else(|p| p.into_inner());
        let from = live.clone();
        let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let (abs_to, abs_from) = (abs(&to), abs(&from));
        if abs_to != abs_from && (abs_to.starts_with(&abs_from) || abs_from.starts_with(&abs_to)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the new cache root must not be inside the current root or contain it",
            ));
        }
        ensure_dir(&to, owner_only)?;
        if fs::canonicalize(&to)? == fs::canonicalize(&from)? {
            return Ok(MoveReport::default());
        }
        ensure_dir(&to.join(".tmp"), owner_only)?;
        // Pins first: if this fails nothing has moved.
        self.persist_pins_at(&st, &to)?;

        let entries: Vec<(String, Option<String>)> = st
            .index
            .iter()
            .map(|(name, meta)| (name.clone(), meta.ext.clone()))
            .collect();
        let mut report = MoveReport::default();
        let mut dropped = Vec::new();
        for (name, ext) in entries {
            let src = entry_path(&from, &name, ext.as_deref());
            let dst = entry_path(&to, &name, ext.as_deref());
            match move_file(&src, &dst, &to, owner_only) {
                Ok(false) => report.moved += 1,
                Ok(true) => report.redundant += 1,
                Err(e) => {
                    report.failed.push(format!("{name}: {e}"));
                    dropped.push(name);
                }
            }
        }
        for name in &dropped {
            if let Some(meta) = st.index.remove(name) {
                st.bytes = st.bytes.saturating_sub(meta.len);
            }
        }
        *live = to.clone();
        if !dropped.is_empty() {
            let _ = self.persist_pins_at(&st, &to);
        }
        // Tidy the old root: only what the cache itself created.
        let _ = fs::remove_file(from.join(PIN_FILE));
        let _ = fs::remove_dir(from.join(".tmp"));
        if let Ok(shards) = fs::read_dir(&from) {
            for shard in shards.flatten() {
                if shard.file_name().len() == 2 {
                    let _ = fs::remove_dir(shard.path());
                }
            }
        }
        Ok(report)
    }

    /// Snapshot of `(last use, base name)` for every unpinned entry, least recently used first.
    pub(crate) fn recency_snapshot(&self) -> Vec<(u128, String)> {
        let st = self.lock();
        st.index.recency_order().cloned().collect()
    }
}

/// Move one entry file. `Ok(true)` when the destination already existed (the source is removed
/// and the destination kept), `Ok(false)` when the file was moved.
fn move_file(src: &Path, dst: &Path, to: &Path, owner_only: bool) -> io::Result<bool> {
    if dst.exists() {
        return match fs::remove_file(src) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(true),
        };
    }
    ensure_dir(dst.parent().expect("sharded path"), owner_only)?;
    if fs::rename(src, dst).is_ok() {
        return Ok(false);
    }
    // Rename failed (another volume, or a missing source): copy beside the destination under a
    // temp name, then rename into place so the destination is never half written.
    let tmp = to.join(".tmp").join(unique_tmp_name(None));
    let modified = fs::metadata(src).and_then(|m| m.modified()).ok();
    let copied = fs::copy(src, &tmp).map(|_| ()).and_then(|()| {
        if owner_only {
            rightkit_fs::private::restrict_owner_only(&tmp).map_err(io_other)?;
        }
        if let Some(modified) = modified {
            OpenOptions::new()
                .write(true)
                .open(&tmp)
                .and_then(|f| f.set_modified(modified))?;
        }
        fs::rename(&tmp, dst)
    });
    if let Err(e) = copied {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    // The entry is in place; a source that cannot be removed is only a leftover in the old root.
    let _ = fs::remove_file(src);
    Ok(false)
}

/// Streaming writer for one cache entry, from [`DiskCache::put_writer`]. See that method.
#[must_use = "a writer that is neither committed nor dropped on purpose leaves a temp file until it is swept"]
pub struct PutWriter {
    cache: DiskCache,
    name: String,
    pin: bool,
    tmp: PathBuf,
    file: Option<File>,
    written: u64,
    limit: u64,
    exceeded: bool,
    armed: bool,
}

impl PutWriter {
    /// Pin the entry when it is committed (see [`DiskCache::put_pinned`]).
    pub fn pinned(mut self) -> Self {
        self.pin = true;
        self
    }

    /// Bytes written so far.
    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    /// Publish the written bytes as the entry. Returns [`PutOutcome::TooLarge`] when the writer went
    /// past the entry limit (nothing is stored). Fails with [`PinnedCapacityError`] when pinned entries
    /// leave no room; the temp file is removed in every failure case.
    pub fn commit(mut self) -> io::Result<PutOutcome> {
        if self.exceeded {
            return Ok(PutOutcome::TooLarge);
        }
        // Close the file before the rename; `armed` stays set until the entry is published, so a
        // refused commit removes the temp file when `self` drops.
        self.file = None;
        self.cache.check_capacity(&self.name, self.written)?;
        let outcome = self
            .cache
            .publish(&self.name, None, self.written, &self.tmp, self.pin)?;
        self.armed = false;
        Ok(outcome)
    }
}

impl Write for PutWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written.saturating_add(buf.len() as u64) > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("cache entry exceeds the size limit"));
        }
        let Some(file) = self.file.as_mut() else {
            return Err(io::Error::other("cache writer is closed"));
        };
        let n = file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for PutWriter {
    fn drop(&mut self) {
        if self.armed {
            remove_tmp(&self.tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: usize) -> Key {
        let id = i.to_string();
        Key::new(&["entry", id.as_str()])
    }

    /// The maintained eviction order and pinned total must equal what a full scan of the entries
    /// gives. The old code computed both by sorting or summing the whole index on every write.
    fn assert_consistent(index: &Index) {
        let mut expected: Vec<(u128, String)> = index
            .entries
            .iter()
            .filter(|(_, m)| !m.pinned)
            .map(|(n, m)| (m.used, n.clone()))
            .collect();
        expected.sort();
        let actual: Vec<(u128, String)> = index.recency.iter().cloned().collect();
        assert_eq!(actual, expected, "eviction order drifted from a full sort");
        let pinned: u64 = index
            .entries
            .values()
            .filter(|m| m.pinned)
            .map(|m| m.len)
            .sum();
        assert_eq!(index.pinned_bytes, pinned, "pinned total drifted");
    }

    #[test]
    fn index_order_matches_a_full_sort_under_random_operations() {
        let mut index = Index::default();
        // Fixed-seed LCG: deterministic and dependency-free. Small `used` and name ranges force
        // many ties, so the base-name tie-break is exercised.
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |bound: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % bound
        };
        for _ in 0..5_000 {
            let name = format!("n{:02}", next(24));
            match next(4) {
                0 => {
                    let meta = Meta {
                        len: next(100) + 1,
                        used: u128::from(next(8)),
                        ext: None,
                        pinned: next(5) == 0,
                    };
                    index.insert(name, meta);
                }
                1 => {
                    index.remove(&name);
                }
                2 => index.touch(&name, u128::from(next(8))),
                _ => {
                    let pinned = next(2) == 0;
                    index.set_pinned(&name, pinned);
                }
            }
            assert_consistent(&index);
        }
    }

    #[test]
    fn over_cap_puts_walk_no_index_and_evict_in_write_order() {
        let root = tempfile::tempdir().unwrap();
        // Room for eight 8-byte entries; every later put evicts exactly one.
        let cache = DiskCache::open(root.path(), Config::new(64)).unwrap();
        for i in 0..8 {
            cache.put(&key(i), &[1; 8]).unwrap();
        }
        // `stats` itself walks the index, so read the counters before the measured window.
        let evicted_before = cache.stats().evictions;
        let walks_before = cache.lock().index.walks.get();
        const PUTS: usize = 1_000;
        for i in 8..8 + PUTS {
            cache.put(&key(i), &[1; 8]).unwrap();
        }
        let walks = cache.lock().index.walks.get() - walks_before;
        let stats = cache.stats();
        assert_eq!(walks, 0, "over-cap puts walked the whole index");
        assert_eq!(stats.evictions - evicted_before, PUTS as u64);
        assert_eq!((stats.entries, stats.bytes), (8, 64));
        for i in 0..PUTS {
            assert!(!cache.contains(&key(i)), "key {i} should be evicted");
        }
        for i in PUTS..8 + PUTS {
            assert!(cache.contains(&key(i)), "key {i} should be resident");
        }
    }
}
