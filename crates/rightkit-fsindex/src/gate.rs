//! Per-folder re-list gate. A folder that keeps changing is re-listed at most once per
//! interval; the interval doubles while changes keep arriving (up to a ceiling) and returns
//! to the floor once a folder is quiet. Changes that arrive inside the window are deferred
//! to its end, never dropped.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Window over which the re-list rate is reported.
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// How often quiet folder states are dropped from the map.
const PRUNE_EVERY: Duration = Duration::from_secs(60);
/// A folder state is dropped once the folder has not been re-listed for this many of its own
/// intervals (and never sooner than `PRUNE_EVERY`). An idle state with nothing deferred
/// behaves exactly like no state once its interval has elapsed (the next change runs at once
/// and the interval restarts at the floor), so dropping it loses nothing; 4 only adds margin
/// for the coarse prune cadence and keeps a recently busy folder visible in
/// `backoff_folders` for a few windows. The horizon follows the interval, so a folder at the
/// ceiling is held longer than one at the floor, but none is held forever.
const HORIZON_INTERVALS: u32 = 4;
/// Most folder states the gate keeps (about 200 B each with a typical path, so ~25 MB).
/// At the cap, settled states are evicted first; if none are, a new folder runs untracked
/// (re-listed at once, no back-off) rather than growing the map.
pub const DEFAULT_MAX_ENTRIES: usize = 131_072;

struct FolderState {
    last: Instant,
    interval: Duration,
    /// Requests that arrived inside the current window.
    hits: u32,
}

/// What the caller should do with a folder change.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Re-list now. The gate has already recorded the run.
    Run,
    /// Deferred until the window ends; `take_due` returns the folder then.
    Deferred,
}

/// Events counted over the last minute, with a running total. Used for directory reads.
pub struct Rate {
    events: VecDeque<(Instant, u64)>,
    total: u64,
    started: Instant,
}

impl Rate {
    pub fn new(now: Instant) -> Rate {
        Rate {
            events: VecDeque::new(),
            total: 0,
            started: now,
        }
    }

    pub fn record(&mut self, now: Instant, n: u64) {
        if n == 0 {
            return;
        }
        self.total += n;
        self.events.push_back((now, n));
        self.trim(now);
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// Events per second over the last minute (or since start, if sooner).
    pub fn per_s(&mut self, now: Instant) -> f64 {
        self.trim(now);
        let count: u64 = self.events.iter().map(|&(_, n)| n).sum();
        let span = now
            .duration_since(self.started)
            .min(RATE_WINDOW)
            .max(Duration::from_secs(1));
        count as f64 / span.as_secs_f64()
    }

    fn trim(&mut self, now: Instant) {
        while let Some(&(t, _)) = self.events.front() {
            if now.duration_since(t) > RATE_WINDOW {
                self.events.pop_front();
            } else {
                break;
            }
        }
    }
}

pub struct RelistGate {
    floor: Duration,
    ceiling: Duration,
    started: Instant,
    folders: HashMap<Vec<u8>, FolderState>,
    /// Folder path -> time at which its deferred re-list is due.
    deferred: HashMap<Vec<u8>, Instant>,
    /// Times of recent re-lists, oldest first, pruned to `RATE_WINDOW`.
    recent: VecDeque<Instant>,
    relists_total: u64,
    last_prune: Instant,
    max_entries: usize,
}

impl RelistGate {
    pub fn new(floor: Duration, ceiling: Duration, now: Instant) -> RelistGate {
        RelistGate::with_max_entries(floor, ceiling, now, DEFAULT_MAX_ENTRIES)
    }

    pub fn with_max_entries(
        floor: Duration,
        ceiling: Duration,
        now: Instant,
        max_entries: usize,
    ) -> RelistGate {
        RelistGate {
            floor,
            ceiling: ceiling.max(floor),
            started: now,
            folders: HashMap::new(),
            deferred: HashMap::new(),
            recent: VecDeque::new(),
            relists_total: 0,
            last_prune: now,
            max_entries,
        }
    }

    /// Folder states currently held (bounded by the entry cap).
    pub fn entries(&self) -> usize {
        self.folders.len()
    }

    /// Decides whether a change in `path` re-lists it now or waits for its window.
    pub fn request(&mut self, path: &[u8], now: Instant) -> Decision {
        if self.floor.is_zero() {
            self.record_run(now);
            return Decision::Run;
        }
        let due = match self.folders.get(path) {
            None => {
                if self.folders.len() >= self.max_entries {
                    self.evict_settled(now);
                }
                if self.folders.len() >= self.max_entries {
                    // Full of live states: run this folder untracked instead of growing.
                    self.record_run(now);
                    return Decision::Run;
                }
                self.begin_run(path, now);
                return Decision::Run;
            }
            Some(s) => s.last + s.interval,
        };
        if now >= due {
            self.begin_run(path, now);
            return Decision::Run;
        }
        if let Some(s) = self.folders.get_mut(path) {
            s.hits = s.hits.saturating_add(1);
        }
        self.deferred.entry(path.to_vec()).or_insert(due);
        Decision::Deferred
    }

    /// Folders whose deferred re-list is due. Each one is recorded as run, so the caller
    /// re-lists exactly the returned paths.
    pub fn take_due(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let due: Vec<Vec<u8>> = self
            .deferred
            .iter()
            .filter(|(_, t)| **t <= now)
            .map(|(p, _)| p.clone())
            .collect();
        for p in &due {
            self.begin_run(p, now);
        }
        self.prune(now);
        due
    }

    /// Whether any folder re-list is waiting for its window.
    pub fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// The earliest deferred re-list, if any.
    pub fn next_due(&self) -> Option<Instant> {
        self.deferred.values().min().copied()
    }

    /// Re-lists recorded in the last minute, per second. Before a minute has passed the
    /// rate is averaged over the time since start.
    pub fn relists_per_s(&mut self, now: Instant) -> f64 {
        self.trim_recent(now);
        let span = now
            .duration_since(self.started)
            .min(RATE_WINDOW)
            .max(Duration::from_secs(1));
        self.recent.len() as f64 / span.as_secs_f64()
    }

    /// Folders currently backing off (interval above the floor).
    pub fn backoff_folders(&self) -> usize {
        self.folders
            .values()
            .filter(|s| s.interval > self.floor)
            .count()
    }

    /// Re-lists run since the gate was created.
    pub fn relists_total(&self) -> u64 {
        self.relists_total
    }

    /// Tracked heap of the gate's maps (estimate; see `Store::heap_bytes`).
    pub fn heap_bytes(&self) -> usize {
        let folders: usize = self
            .folders
            .keys()
            .map(|k| k.capacity() + 56)
            .sum::<usize>()
            + self.folders.capacity() * 40;
        let deferred: usize = self
            .deferred
            .keys()
            .map(|k| k.capacity() + 40)
            .sum::<usize>()
            + self.deferred.capacity() * 32;
        folders + deferred + self.recent.capacity() * 16
    }

    fn begin_run(&mut self, path: &[u8], now: Instant) {
        self.deferred.remove(path);
        let floor = self.floor;
        let ceiling = self.ceiling;
        let s = self.folders.entry(path.to_vec()).or_insert(FolderState {
            last: now,
            interval: floor,
            hits: 0,
        });
        s.interval = if s.hits > 0 {
            (s.interval * 2).max(floor * 2).min(ceiling)
        } else {
            floor
        };
        s.hits = 0;
        s.last = now;
        self.record_run(now);
    }

    fn record_run(&mut self, now: Instant) {
        self.relists_total += 1;
        self.recent.push_back(now);
        self.trim_recent(now);
    }

    fn trim_recent(&mut self, now: Instant) {
        while let Some(&t) = self.recent.front() {
            if now.duration_since(t) > RATE_WINDOW {
                self.recent.pop_front();
            } else {
                break;
            }
        }
    }

    /// Drops states that no longer carry information: nothing deferred and the folder's own
    /// interval has elapsed, so a new request would run at once with the floor interval anyway.
    fn evict_settled(&mut self, now: Instant) {
        let deferred = &self.deferred;
        self.folders.retain(|path, s| {
            s.hits > 0 || deferred.contains_key(path) || now < s.last + s.interval
        });
    }

    /// Drops states of folders not re-listed for `HORIZON_INTERVALS` of their own intervals
    /// (nothing deferred), whatever the interval, so the map does not grow with every folder
    /// ever touched.
    fn prune(&mut self, now: Instant) {
        if now.duration_since(self.last_prune) < PRUNE_EVERY {
            return;
        }
        self.last_prune = now;
        let deferred = &self.deferred;
        self.folders.retain(|path, s| {
            let horizon = (s.interval * HORIZON_INTERVALS).max(PRUNE_EVERY);
            s.hits > 0 || deferred.contains_key(path) || now.duration_since(s.last) < horizon
        });
    }
}
