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

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn first_change_runs_immediately_and_inside_the_window_defers() {
        let t0 = Instant::now();
        let mut g = RelistGate::new(ms(1000), ms(8000), t0);
        assert_eq!(g.request(b"/a", t0), Decision::Run);
        assert_eq!(g.request(b"/a", t0 + ms(100)), Decision::Deferred);
        assert_eq!(g.request(b"/a", t0 + ms(200)), Decision::Deferred);
        assert!(g.take_due(t0 + ms(999)).is_empty());
        assert_eq!(g.take_due(t0 + ms(1000)), vec![b"/a".to_vec()]);
        assert_eq!(g.relists_total(), 2);
    }

    #[test]
    fn continuous_changes_back_off_to_the_ceiling() {
        let t0 = Instant::now();
        let mut g = RelistGate::new(ms(1000), ms(8000), t0);
        let mut runs = 0u32;
        // A change every 50 ms for 60 s on one folder.
        let mut t = t0;
        let end = t0 + Duration::from_secs(60);
        while t <= end {
            if g.request(b"/busy", t) == Decision::Run {
                runs += 1;
            }
            for p in g.take_due(t) {
                assert_eq!(p, b"/busy".to_vec());
                runs += 1;
            }
            t += ms(50);
        }
        // Floor 1 s, doubling to the 8 s ceiling: far fewer re-lists than the 1200 changes.
        assert!(runs <= 16, "runs={runs}");
        assert!(runs >= 5, "runs={runs}");
        assert_eq!(g.backoff_folders(), 1);
        assert!(g.relists_per_s(end) <= 1.0);
    }

    #[test]
    fn a_quiet_folder_returns_to_the_floor() {
        let t0 = Instant::now();
        let mut g = RelistGate::new(ms(1000), ms(8000), t0);
        g.request(b"/q", t0);
        g.request(b"/q", t0 + ms(10));
        g.take_due(t0 + ms(1000));
        assert_eq!(g.backoff_folders(), 1);
        // Quiet for a while: the next change runs at once and the interval resets.
        assert_eq!(g.request(b"/q", t0 + ms(20_000)), Decision::Run);
        assert_eq!(g.backoff_folders(), 0);
    }

    #[test]
    fn zero_floor_disables_the_gate() {
        let t0 = Instant::now();
        let mut g = RelistGate::new(Duration::ZERO, ms(8000), t0);
        for i in 0..10 {
            assert_eq!(g.request(b"/z", t0 + ms(i)), Decision::Run);
        }
        assert!(g.next_due().is_none());
    }

    #[test]
    fn separate_folders_are_gated_independently() {
        let t0 = Instant::now();
        let mut g = RelistGate::new(ms(1000), ms(8000), t0);
        assert_eq!(g.request(b"/a", t0), Decision::Run);
        assert_eq!(g.request(b"/b", t0), Decision::Run);
        assert_eq!(g.request(b"/a", t0 + ms(1)), Decision::Deferred);
        assert_eq!(g.next_due(), Some(t0 + ms(1000)));
    }

    #[test]
    fn idle_gate_forgets_every_folder_after_the_horizon() {
        let t0 = Instant::now();
        let mut g = RelistGate::with_max_entries(ms(1000), ms(8000), t0, 200_000);
        let paths: Vec<Vec<u8>> = (0..100_000)
            .map(|i| format!("/build/dir{i}").into_bytes())
            .collect();
        // Touch each folder twice: the first runs, the second defers and runs when due.
        for p in &paths {
            assert_eq!(g.request(p, t0), Decision::Run);
        }
        for p in &paths {
            assert_eq!(g.request(p, t0 + ms(10)), Decision::Deferred);
        }
        assert_eq!(g.take_due(t0 + ms(1000)).len(), 100_000);
        assert_eq!(g.entries(), 100_000);
        assert_eq!(g.backoff_folders(), 100_000);
        // Idle: past the horizon (4 x the doubled 2 s interval, at least 60 s) nothing is left.
        let _ = g.take_due(t0 + Duration::from_secs(200));
        assert_eq!(g.entries(), 0);
        assert_eq!(g.backoff_folders(), 0);
        assert!(!g.has_deferred());
    }

    #[test]
    fn a_folder_still_changing_keeps_its_back_off_while_quiet_ones_go() {
        let t0 = Instant::now();
        let mut g = RelistGate::new(ms(1000), ms(8000), t0);
        for i in 0..100 {
            g.request(format!("/quiet{i}").as_bytes(), t0);
            g.request(format!("/quiet{i}").as_bytes(), t0 + ms(5));
        }
        let mut t = t0;
        let end = t0 + Duration::from_secs(300);
        while t <= end {
            g.request(b"/busy", t);
            g.take_due(t);
            t += ms(50);
        }
        assert_eq!(g.entries(), 1);
        assert_eq!(g.backoff_folders(), 1);
    }

    #[test]
    fn the_entry_cap_holds_and_settled_states_make_room() {
        let t0 = Instant::now();
        let mut g = RelistGate::with_max_entries(ms(1000), ms(8000), t0, 10);
        for i in 0..50 {
            assert_eq!(g.request(format!("/f{i}").as_bytes(), t0), Decision::Run);
        }
        assert_eq!(g.entries(), 10);
        assert_eq!(g.relists_total(), 50);
        // Once their intervals have elapsed the states are settled and a new folder fits.
        assert_eq!(g.request(b"/new", t0 + ms(1500)), Decision::Run);
        assert_eq!(g.entries(), 1);
    }
}
