//! Several cache roots (or namespaces under one root) managed as one set.
//!
//! Each member keeps its own budget, index and LRU, enforced on every write. A set adds two
//! things on top: aggregated stats across members, and an optional combined budget that
//! [`CacheSet::enforce_budget`] applies by evicting the globally least recently used entries.

use crate::disk::{Config, DiskCache, Stats};
use std::io;
use std::path::Path;
use std::time::SystemTime;

/// Namespace names are 1 to 32 characters of `a-z`, `0-9`, `-` or `_`, which keeps them safe as
/// directory names under a shared root.
pub fn valid_namespace(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn invalid_namespace() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "namespace must be 1 to 32 characters of a-z, 0-9, '-' or '_'",
    )
}

/// Named cache members with aggregated accounting and an optional combined byte budget.
#[derive(Clone, Default)]
pub struct CacheSet {
    members: Vec<(String, DiskCache)>,
    budget: Option<u64>,
}

/// Aggregated view over a [`CacheSet`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetStats {
    pub entries: usize,
    pub bytes: u64,
    /// Least recently used entry across all members; `None` when every member is empty.
    pub oldest: Option<SystemTime>,
    /// Most recently used entry across all members; `None` when every member is empty.
    pub newest: Option<SystemTime>,
    /// Per-member stats in insertion order.
    pub members: Vec<(String, Stats)>,
}

impl CacheSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Combined byte budget across all members, applied by [`CacheSet::enforce_budget`].
    pub fn with_budget(mut self, bytes: u64) -> Self {
        self.budget = Some(bytes);
        self
    }

    /// Add a member under `name`. Names must be unique, and each member must use its own directory.
    pub fn add(&mut self, name: &str, cache: DiskCache) -> io::Result<()> {
        if !valid_namespace(name) {
            return Err(invalid_namespace());
        }
        if self.members.iter().any(|(n, _)| n == name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "namespace already present in this set",
            ));
        }
        if self
            .members
            .iter()
            .any(|(_, c)| c.location() == cache.location())
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "directory already belongs to this set",
            ));
        }
        self.members.push((name.to_owned(), cache));
        Ok(())
    }

    /// Open namespaces that share one root. Each namespace is `<root>/<name>` with its own budget,
    /// so thumbnails, frames and waveforms can live under one root without evicting each other.
    /// Names are validated before anything is created on disk.
    pub fn open_namespaces(
        root: impl AsRef<Path>,
        namespaces: &[(&str, Config)],
    ) -> io::Result<Self> {
        let mut set = Self::new();
        for (name, cfg) in namespaces {
            if !valid_namespace(name) {
                return Err(invalid_namespace());
            }
            let cache = DiskCache::open(root.as_ref().join(name), *cfg)?;
            set.add(name, cache)?;
        }
        Ok(set)
    }

    /// Member by name.
    pub fn get(&self, name: &str) -> Option<&DiskCache> {
        self.members.iter().find(|(n, _)| n == name).map(|(_, c)| c)
    }

    /// Aggregated stats; per-member stats are included in `members`.
    pub fn stats(&self) -> SetStats {
        let mut out = SetStats::default();
        for (name, cache) in &self.members {
            let s = cache.stats();
            out.entries += s.entries;
            out.bytes += s.bytes;
            out.oldest = min_time(out.oldest, s.oldest);
            out.newest = max_time(out.newest, s.newest);
            out.members.push((name.clone(), s));
        }
        out
    }

    /// Apply the combined budget: while the members' total bytes exceed it, evict the least
    /// recently used entry across every member. Returns how many entries were evicted.
    ///
    /// Member caps still hold on every write; this adds the cross-member limit. With no budget
    /// set, or no excess, it evicts nothing. Under concurrent writers to the same members the
    /// result is best effort: the total is re-checked against each eviction's freed bytes.
    pub fn enforce_budget(&self) -> usize {
        let Some(budget) = self.budget else {
            return 0;
        };
        let mut total: u64 = self.members.iter().map(|(_, c)| c.stats().bytes).sum();
        if total <= budget {
            return 0;
        }
        let mut candidates: Vec<(u128, usize, String)> = Vec::new();
        for (i, (_, cache)) in self.members.iter().enumerate() {
            for (used, base) in cache.recency_snapshot() {
                candidates.push((used, i, base));
            }
        }
        candidates.sort();
        let mut evicted = 0;
        for (_, i, base) in candidates {
            if total <= budget {
                break;
            }
            if let Some(freed) = self.members[i].1.evict_base(&base) {
                total = total.saturating_sub(freed);
                evicted += 1;
            }
        }
        evicted
    }
}

fn min_time(a: Option<SystemTime>, b: Option<SystemTime>) -> Option<SystemTime> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

fn max_time(a: Option<SystemTime>, b: Option<SystemTime>) -> Option<SystemTime> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}
