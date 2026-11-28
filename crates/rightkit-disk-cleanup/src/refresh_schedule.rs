//! When to read a changed folder of a live index again, and in what order. Moved out of Pulse's
//! `scanner.rs`.
//!
//! A folder read again is held for `min_gap`; the hold doubles with each further read inside
//! `churn_window` (an unchanged read counting double), up to `max_gap`, so a folder that rewrites
//! itself all day is read about once a `max_gap` while one that changes rarely is read at once. A
//! folder whose child folders were compared with the disk is held for `reconcile_gap`. History
//! older than `forget` is dropped.
//!
//! [`RefreshSchedule::plan`] splits the pending folders into the held ones and the due ones, drops
//! a due folder that a due outer folder (no calmer than it) covers, and orders the rest: due for
//! `starve` already first, then the cheap before the ones known to take `batch_budget` to
//! themselves, then the calm before the ones that keep changing, then the newest change. Reading
//! the folders, the budget loop and what a read means are the host's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The schedule's timings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPolicy {
    pub min_gap: Duration,
    pub max_gap: Duration,
    pub churn_window: Duration,
    pub forget: Duration,
    pub starve: Duration,
    pub reconcile_gap: Duration,
    pub batch_budget: Duration,
}

impl RefreshPolicy {
    /// Pulse's timings: 5 s to 60 s holds, a 2 min churn window, 10 min memory, 30 s starve,
    /// 2 s between reconciles and a 4 s pass.
    pub const STANDARD: Self = Self {
        min_gap: Duration::from_secs(5),
        max_gap: Duration::from_secs(60),
        churn_window: Duration::from_secs(120),
        forget: Duration::from_secs(600),
        starve: Duration::from_secs(30),
        reconcile_gap: Duration::from_secs(2),
        batch_budget: Duration::from_secs(4),
    };
}

/// How one folder read again lately behaved.
#[derive(Clone, Copy, Debug)]
struct Churn {
    last_read: Instant,
    /// Not read again before this.
    hold_until: Instant,
    /// Reads in a row inside the churn window, an unchanged read counting double.
    level: u32,
    /// How long its last read took.
    cost: Duration,
}

/// One folder waiting to be read again.
#[derive(Debug)]
pub struct Pending<T> {
    pub path: PathBuf,
    /// When the change was first reported.
    pub since: Instant,
    /// Its read history applies (false for a folder new to the index: it is never held).
    pub tracked: bool,
    /// Reading it covers every folder below it.
    pub covers_below: bool,
    /// The host's own data (what kind of read).
    pub item: T,
}

/// A due folder with the cost of its last read.
#[derive(Debug)]
pub struct Due<T> {
    pub pending: Pending<T>,
    /// How long its last read took (zero when unknown).
    pub cost: Duration,
}

/// What one pass should do.
#[derive(Debug)]
pub struct Plan<T> {
    /// Held: waits for a later pass.
    pub held: Vec<Pending<T>>,
    /// Due, in the order to read them.
    pub due: Vec<Due<T>>,
}

/// The read history of a live index's folders.
#[derive(Debug)]
pub struct RefreshSchedule {
    policy: RefreshPolicy,
    churn: BTreeMap<PathBuf, Churn>,
}

impl RefreshSchedule {
    pub const fn new(policy: RefreshPolicy) -> Self {
        Self {
            policy,
            churn: BTreeMap::new(),
        }
    }

    pub fn policy(&self) -> RefreshPolicy {
        self.policy
    }

    /// Forget every folder's history (the index was replaced).
    pub fn clear(&mut self) {
        self.churn.clear();
    }

    /// `path` was read again at `now` (`changed`: its numbers moved) in `took`.
    pub fn note_read(&mut self, path: &Path, changed: bool, took: Duration, now: Instant) {
        let level = match self.churn.get(path) {
            Some(before) if now.duration_since(before.last_read) < self.policy.churn_window => {
                before.level.saturating_add(if changed { 1 } else { 2 })
            }
            _ => u32::from(!changed),
        };
        let gap = (self.policy.min_gap * (1u32 << level.min(4))).min(self.policy.max_gap);
        self.churn.insert(
            path.to_path_buf(),
            Churn {
                last_read: now,
                hold_until: now + gap,
                level,
                cost: took,
            },
        );
    }

    /// The child folders of `path` were compared with the disk at `now`, in `took`.
    pub fn note_reconciled(&mut self, path: &Path, took: Duration, now: Instant) {
        self.churn.insert(
            path.to_path_buf(),
            Churn {
                last_read: now,
                hold_until: now + self.policy.reconcile_gap,
                level: 0,
                cost: took,
            },
        );
    }

    /// Split `pending` into held and due folders and order the due ones (see the module docs).
    pub fn plan<T>(&mut self, pending: Vec<Pending<T>>, now: Instant) -> Plan<T> {
        let forget = self.policy.forget;
        self.churn
            .retain(|_, seen| now.duration_since(seen.last_read) < forget);
        struct Ready<T> {
            pending: Pending<T>,
            level: u32,
            cost: Duration,
            aged: bool,
        }
        let mut held = Vec::new();
        let mut ready: Vec<Ready<T>> = Vec::new();
        for item in pending {
            let state = if item.tracked {
                self.churn.get(&item.path)
            } else {
                None
            };
            if state.is_some_and(|seen| seen.hold_until > now) {
                held.push(item);
                continue;
            }
            let due = state.map_or(item.since, |seen| item.since.max(seen.hold_until));
            ready.push(Ready {
                level: state.map_or(0, |seen| seen.level),
                cost: state.map_or(Duration::ZERO, |seen| seen.cost),
                aged: now.duration_since(due) > self.policy.starve,
                pending: item,
            });
        }
        let outer: Vec<(PathBuf, u32)> = ready
            .iter()
            .filter(|r| r.pending.covers_below)
            .map(|r| (r.pending.path.clone(), r.level))
            .collect();
        ready.retain(|r| {
            !r.pending.covers_below
                || !outer.iter().any(|(folder, level)| {
                    *folder != r.pending.path
                        && r.pending.path.starts_with(folder)
                        && *level <= r.level
                })
        });
        let budget = self.policy.batch_budget;
        ready.sort_by(|a, b| {
            (!a.aged, a.cost >= budget, a.level)
                .cmp(&(!b.aged, b.cost >= budget, b.level))
                .then_with(|| b.pending.since.cmp(&a.pending.since))
        });
        Plan {
            held,
            due: ready
                .into_iter()
                .map(|r| Due {
                    pending: r.pending,
                    cost: r.cost,
                })
                .collect(),
        }
    }
}
