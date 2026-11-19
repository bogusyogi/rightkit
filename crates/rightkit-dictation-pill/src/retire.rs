//! Retirement of window threads, the owner thread, event forwarders and workers, process-wide.
//!
//! The overlay crate's `OverlayWindow::close_into_join_handle` posts the close and hands the
//! window's owner-thread `JoinHandle` to the caller (`window.rs`, `close_into_join_handle`; the
//! thread's last steps are `drop(owner)` (window destroyed, `hwnd` stored as 0) and
//! `shared.finish()` (`Closed` delivered, subscribers dropped), `win/mod.rs` `run`, so a finished
//! thread means the window is closed and its `hwnd` is 0). The driver keeps every such handle here,
//! keyed by generation, together with the owner thread's own handle, until the thread has finished.
//! Nothing here ever waits on a thread that has not finished: [`Retire::wait_retired`] polls
//! `JoinHandle::is_finished` and joins only a handle that already reports finished (that join
//! returns at once), so no caller can block on a wedged window thread past the bound it passes.
//!
//! Event forwarders (one per window) and host-callback workers are counted here too: a forwarder
//! ends only when its window thread dropped the event sender, and a worker ends when its host
//! callback returned. A new generation is admitted only when no handle is retained, no forwarder
//! of any generation is alive and no worker is running.
//!
//! One record for the whole process (the instance lives in `driver.rs`). Bound on what it holds:
//! at most one generation's handles (one owner thread plus three window threads), because a
//! generation is admitted only when the record is empty, a refused attempt creates nothing, and
//! `hold` drops finished handles before it adds one; at most three forwarders per generation; at
//! most `Limits::max_worker_threads` workers (they are counted, not kept).
//!
//! Pure logic: no Win32, no thread of its own (the waiter passes a bound; it polls every
//! [`POLL`]).

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::clock::after_duration;

/// How often a waiter re-checks `is_finished` (a thread ending does not signal the condvar).
const POLL: Duration = Duration::from_millis(5);

/// What [`Retire::wait_retired`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No retained thread is unfinished, no forwarder is alive and no worker is running.
    Retired,
    /// Something was still running when the bound ran out.
    Pending,
}

/// What is still outstanding, for the diagnostic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Retained owner and window threads that have not finished.
    pub(crate) threads: usize,
    /// Event forwarders alive, all generations.
    pub(crate) forwarders: usize,
    /// Host-callback workers running, all generations.
    pub(crate) workers: usize,
}

struct State {
    /// Forwarders alive per generation; an entry is removed when it reaches zero.
    live: BTreeMap<u64, usize>,
    /// Retained thread handles (the owner and its windows), with their generation.
    threads: Vec<(u64, JoinHandle<()>)>,
    workers: usize,
}

pub(crate) struct Retire {
    state: Mutex<State>,
    changed: Condvar,
}

fn lock(mutex: &Mutex<State>) -> MutexGuard<'_, State> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Join and drop every retained handle that already finished. Never waits on a running thread.
fn reap(state: &mut State) {
    let mut index = 0;
    while index < state.threads.len() {
        if state.threads[index].1.is_finished() {
            let (_, handle) = state.threads.swap_remove(index);
            let _ = handle.join();
        } else {
            index += 1;
        }
    }
}

fn counts(state: &State) -> Counts {
    Counts {
        threads: state.threads.len(),
        forwarders: state
            .live
            .values()
            .fold(0usize, |sum, count| sum.saturating_add(*count)),
        workers: state.workers,
    }
}

impl Retire {
    pub(crate) const fn new() -> Retire {
        Retire {
            state: Mutex::new(State {
                live: BTreeMap::new(),
                threads: Vec::new(),
                workers: 0,
            }),
            changed: Condvar::new(),
        }
    }

    /// One forwarder of `generation` is about to run. Call before it is spawned.
    pub(crate) fn enter(&self, generation: u64) {
        let mut state = lock(&self.state);
        let count = state.live.entry(generation).or_insert(0);
        *count = count.saturating_add(1);
    }

    /// That forwarder ended (or was never spawned).
    pub(crate) fn leave(&self, generation: u64) {
        {
            let mut state = lock(&self.state);
            if let Some(count) = state.live.get_mut(&generation) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.live.remove(&generation);
                }
            }
        }
        self.changed.notify_all();
    }

    /// One host-callback worker is about to run. Call before it is spawned.
    pub(crate) fn enter_worker(&self) {
        let mut state = lock(&self.state);
        state.workers = state.workers.saturating_add(1);
    }

    /// That worker ended (or was never spawned).
    pub(crate) fn leave_worker(&self) {
        {
            let mut state = lock(&self.state);
            state.workers = state.workers.saturating_sub(1);
        }
        self.changed.notify_all();
    }

    /// Keep `handle` (the owner thread, or a window thread whose close was requested) until it has
    /// finished. Finished handles already held are joined and dropped first, so the record never
    /// accumulates beyond one generation's threads.
    pub(crate) fn hold(&self, generation: u64, handle: JoinHandle<()>) {
        {
            let mut state = lock(&self.state);
            reap(&mut state);
            state.threads.push((generation, handle));
        }
        self.changed.notify_all();
    }

    /// What is outstanding right now (finished handles are reaped first).
    pub(crate) fn counts(&self) -> Counts {
        let mut state = lock(&self.state);
        reap(&mut state);
        counts(&state)
    }

    /// Wait up to `bound` for every retained thread to finish, every forwarder to end and every
    /// worker to return. Never blocks past `bound` (plus the lock hold of one reap).
    pub(crate) fn wait_retired(&self, bound: Duration) -> Verdict {
        let deadline = after_duration(Instant::now(), bound);
        let mut guard = lock(&self.state);
        loop {
            reap(&mut guard);
            let now = counts(&guard);
            if now.threads == 0 && now.forwarders == 0 && now.workers == 0 {
                return Verdict::Retired;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Verdict::Pending;
            }
            let (next, _) = self
                .changed
                .wait_timeout(guard, left.min(POLL))
                .unwrap_or_else(|e| e.into_inner());
            guard = next;
        }
    }
}
