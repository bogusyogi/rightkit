//! Byte-oriented disk cache with a hard size cap.
//!
//! Improves on the MailRight `attach_cache` / `webimg` pattern it replaces:
//! eviction runs on every write (donor: every 20th), recency is updated on
//! reads (donor: write time only), recency survives restarts (file mtime),
//! writes are atomic (temp file + rename), and groups (an account, a URL
//! family) can be invalidated without a side index.
//!
//! Layout: `<root>/<hh>/<keyhash>-<group>` plus `<root>/.tmp/` for in-flight
//! writes. One `DiskCache` per directory per process: the in-memory index is
//! rebuilt on [`DiskCache::open`], not kept coherent with other writers.

use crate::key::{group_id, Key};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Total byte budget; the cache never stays above it after a write returns.
    pub max_bytes: u64,
    /// Entries larger than this are refused (webimg used 8 MB). `None` = only `max_bytes` applies.
    pub max_entry_bytes: Option<u64>,
}

impl Config {
    pub fn new(max_bytes: u64) -> Self {
        Self { max_bytes, max_entry_bytes: None }
    }

    pub fn max_entry_bytes(mut self, n: u64) -> Self {
        self.max_entry_bytes = Some(n);
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Stored,
    /// Larger than `max_entry_bytes` or `max_bytes`; nothing was written and existing data is untouched.
    TooLarge,
}

struct Meta {
    len: u64,
    /// Nanoseconds since the epoch of last use; strictly increasing within a process.
    used: u128,
}

#[derive(Default)]
struct State {
    index: HashMap<String, Meta>,
    bytes: u64,
    clock: u128,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl State {
    fn tick(&mut self) -> u128 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        self.clock = now.max(self.clock + 1);
        self.clock
    }
}

struct Inner {
    root: PathBuf,
    cfg: Config,
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

fn valid_entry_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 64 + 1 + 16 && b[64] == b'-' && name.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

impl DiskCache {
    /// Open (creating if needed) and index an existing cache directory, discarding crash leftovers
    /// and trimming to `cfg.max_bytes` (the cap may have shrunk since the last run).
    pub fn open(root: impl Into<PathBuf>, cfg: Config) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let tmp = root.join(".tmp");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp)?;

        let mut state = State::default();
        let mut found: Vec<(String, u64, u128)> = Vec::new();
        for shard in fs::read_dir(&root)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() || shard.file_name().len() != 2 {
                continue;
            }
            for f in fs::read_dir(shard.path())? {
                let f = f?;
                let name = f.file_name().to_string_lossy().into_owned();
                let meta = f.metadata()?;
                if !meta.is_file() || !valid_entry_name(&name) || !name.starts_with(&shard.file_name().to_string_lossy().into_owned()) {
                    continue;
                }
                let used = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                found.push((name, meta.len(), used));
            }
        }
        // Deterministic recency order even when mtimes tie.
        found.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        for (name, len, used) in found {
            state.clock = state.clock.max(used);
            state.bytes += len;
            state.index.insert(name, Meta { len, used });
        }
        let cache = Self { inner: Arc::new(Inner { root, cfg, state: Mutex::new(state) }) };
        {
            let mut st = cache.lock();
            cache.evict_locked(&mut st, 0);
        }
        Ok(cache)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.inner.root.join(&name[..2]).join(name)
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn config(&self) -> Config {
        self.inner.cfg
    }

    pub fn stats(&self) -> Stats {
        let st = self.lock();
        Stats { entries: st.index.len(), bytes: st.bytes, hits: st.hits, misses: st.misses, evictions: st.evictions }
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.lock().index.contains_key(&entry_name(key))
    }

    /// Read an entry and mark it most-recently-used (in memory and on disk, so recency survives a restart).
    pub fn get(&self, key: &Key) -> Option<Vec<u8>> {
        let name = entry_name(key);
        {
            let mut st = self.lock();
            if !st.index.contains_key(&name) {
                st.misses += 1;
                return None;
            }
        }
        let path = self.path_for(&name);
        let read = OpenOptions::new().read(true).write(true).open(&path).or_else(|_| File::open(&path)).and_then(|mut f| {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            let _ = f.set_modified(SystemTime::now());
            Ok(buf)
        });
        let mut st = self.lock();
        match read {
            Ok(bytes) => {
                let used = st.tick();
                if let Some(m) = st.index.get_mut(&name) {
                    m.used = used;
                }
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

    /// Store `bytes` atomically, then evict least-recently-used entries until the cap holds.
    pub fn put(&self, key: &Key, bytes: &[u8]) -> io::Result<PutOutcome> {
        let len = bytes.len() as u64;
        let cfg = self.inner.cfg;
        if len > cfg.max_bytes || cfg.max_entry_bytes.is_some_and(|m| len > m) {
            return Ok(PutOutcome::TooLarge);
        }
        let name = entry_name(key);
        let dest = self.path_for(&name);
        let tmp = self.write_tmp(bytes)?;
        fs::create_dir_all(dest.parent().expect("sharded path"))?;
        if let Err(e) = fs::rename(&tmp, &dest) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        let mut st = self.lock();
        let used = st.tick();
        if let Some(old) = st.index.insert(name.clone(), Meta { len, used }) {
            st.bytes = st.bytes.saturating_sub(old.len);
        }
        st.bytes += len;
        self.evict_locked(&mut st, 0);
        Ok(PutOutcome::Stored)
    }

    fn write_tmp(&self, bytes: &[u8]) -> io::Result<PathBuf> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = self.inner.root.join(".tmp").join(format!("{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        let mut f = File::create(&tmp)?;
        if let Err(e) = f.write_all(bytes) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(tmp)
    }

    /// Fetch-or-cache: on a miss call `fetch`, write the result through (best effort), return it.
    /// A fetch error leaves the cache untouched; a cache write failure never fails the caller.
    pub fn get_or_fetch<E>(&self, key: &Key, fetch: impl FnOnce() -> Result<Vec<u8>, E>) -> Result<Vec<u8>, E> {
        if let Some(bytes) = self.get(key) {
            return Ok(bytes);
        }
        let bytes = fetch()?;
        let _ = self.put(key, &bytes);
        Ok(bytes)
    }

    pub fn remove(&self, key: &Key) -> bool {
        let name = entry_name(key);
        let mut st = self.lock();
        match st.index.remove(&name) {
            Some(m) => {
                st.bytes = st.bytes.saturating_sub(m.len);
                let _ = fs::remove_file(self.path_for(&name));
                true
            }
            None => false,
        }
    }

    /// Remove every entry stored under `Key::grouped(group_parts, ..)`; returns how many were removed.
    pub fn clear_group(&self, group_parts: &[&str]) -> usize {
        let suffix = format!("-{}", group_id(group_parts));
        let mut st = self.lock();
        let names: Vec<String> = st.index.keys().filter(|n| n.ends_with(&suffix)).cloned().collect();
        for n in &names {
            if let Some(m) = st.index.remove(n) {
                st.bytes = st.bytes.saturating_sub(m.len);
            }
            let _ = fs::remove_file(self.path_for(n));
        }
        names.len()
    }

    pub fn clear(&self) -> usize {
        let mut st = self.lock();
        let names: Vec<String> = st.index.keys().cloned().collect();
        for n in &names {
            let _ = fs::remove_file(self.path_for(n));
        }
        st.index.clear();
        st.bytes = 0;
        names.len()
    }

    /// Evict LRU entries until `bytes + reserve <= max_bytes`.
    fn evict_locked(&self, st: &mut State, reserve: u64) {
        let cap = self.inner.cfg.max_bytes;
        if st.bytes + reserve <= cap {
            return;
        }
        let mut order: Vec<(u128, String)> = st.index.iter().map(|(n, m)| (m.used, n.clone())).collect();
        order.sort();
        for (_, name) in order {
            if st.bytes + reserve <= cap {
                break;
            }
            if let Some(m) = st.index.remove(&name) {
                st.bytes = st.bytes.saturating_sub(m.len);
                st.evictions += 1;
                let _ = fs::remove_file(self.path_for(&name));
            }
        }
    }
}
