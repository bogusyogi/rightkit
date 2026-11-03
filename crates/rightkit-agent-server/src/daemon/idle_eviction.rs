//! Idle resource minimization (DL-010).
//!
//! Ported from prime-agent's `daemon-supervisor.ts` idle sweep.  The sweep
//! refreshes worker summaries before making a decision, passivates idle child
//! sessions, fences mutations while whole worker trees are rechecked, and
//! evicts only workers that remain eligible.  The timer is an unref-equivalent
//! Tokio task: it is not a blocking thread or a process-lifetime handle, and
//! dropping the controller cancels it.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::residency::{
    can_evict_worker, idle_eviction_sweep_interval, IdleEvictionMinutes, WorkerResidencySnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct DrainState {
    active_mutations: usize,
    fenced: bool,
}

/// Error returned when a mutation tries to begin after eviction fencing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationAdmissionError;

impl std::fmt::Display for MutationAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("daemon mutation admission is fenced for idle eviction")
    }
}

impl std::error::Error for MutationAdmissionError {}

/// Error returned when the existing mutation set does not drain in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationDrainTimeout;

impl std::fmt::Display for MutationDrainTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("timed out draining daemon mutations for idle eviction")
    }
}

impl std::error::Error for MutationDrainTimeout {}

/// Admission fence shared by mutating handlers and an idle sweep.
///
/// The double check in [`MutationDrainFence::begin_mutation`] closes the race
/// in which a mutation observes an open gate immediately before the sweep
/// fences it.  The permit is RAII-based, so a handler that returns through an
/// error path still releases its count.
#[derive(Debug, Clone, Default)]
pub struct MutationDrainFence {
    state: Arc<(Mutex<DrainState>, Condvar)>,
}

impl MutationDrainFence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin_mutation(&self) -> Result<MutationPermit, MutationAdmissionError> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.fenced {
            return Err(MutationAdmissionError);
        }
        state.active_mutations += 1;
        // A sweep may fence between the first check and increment.  Re-check
        // while holding the same mutex used by the sweeper.
        if state.fenced {
            state.active_mutations -= 1;
            return Err(MutationAdmissionError);
        }
        Ok(MutationPermit {
            state: Arc::clone(&self.state),
        })
    }

    /// Fence new mutations and wait for admitted mutations to finish.
    pub fn try_begin_eviction(
        &self,
        timeout: Duration,
    ) -> Result<MutationDrainGuard, MutationDrainTimeout> {
        let (lock, condition) = &*self.state;
        let deadline = Instant::now() + timeout;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.fenced = true;
        while state.active_mutations != 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.fenced = false;
                condition.notify_all();
                return Err(MutationDrainTimeout);
            }
            let (next, wait) = condition
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if wait.timed_out() && state.active_mutations != 0 {
                state.fenced = false;
                condition.notify_all();
                return Err(MutationDrainTimeout);
            }
        }
        Ok(MutationDrainGuard {
            state: Arc::clone(&self.state),
        })
    }

    pub fn is_fenced(&self) -> bool {
        let (lock, _) = &*self.state;
        lock.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fenced
    }

    pub fn active_mutations(&self) -> usize {
        let (lock, _) = &*self.state;
        lock.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active_mutations
    }
}

/// Permit held for the duration of one daemon mutation.
pub struct MutationPermit {
    state: Arc<(Mutex<DrainState>, Condvar)>,
}

impl Drop for MutationPermit {
    fn drop(&mut self) {
        let (lock, condition) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_mutations = state.active_mutations.saturating_sub(1);
        if state.active_mutations == 0 {
            condition.notify_all();
        }
    }
}

/// Guard held while the caller performs the final whole-tree recheck and
/// eviction. Dropping it reopens mutation admission even on an error path.
pub struct MutationDrainGuard {
    state: Arc<(Mutex<DrainState>, Condvar)>,
}

impl Drop for MutationDrainGuard {
    fn drop(&mut self) {
        let (lock, condition) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.fenced = false;
        condition.notify_all();
    }
}

/// Counts and failure visibility for one idle sweep.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IdleEvictionSweepReport {
    pub refreshed_workers: usize,
    pub refresh_failures: usize,
    pub passivated_workers: usize,
    pub passivation_failures: usize,
    pub evicted_workers: usize,
    pub eviction_failures: usize,
    pub drain_timed_out: bool,
}

/// Run one refresh/passivation/whole-tree-eviction sweep.
///
/// This is the supervisor seam. `refresh` must obtain fresh summaries, rather
/// than reusing a prior sweep. `passivate_children` owns per-child traversal;
/// it receives the same policy and timestamp so descendants can be checked
/// with [`super::residency::can_passivate_session`]. `evict_worker` must stop
/// the complete worker tree, not only its root session.
#[allow(clippy::too_many_arguments)]
pub fn run_idle_eviction_sweep<W, Workers, Refresh, Passivate, Evict>(
    workers: Workers,
    policy: IdleEvictionMinutes,
    now_ms: u128,
    drain: &MutationDrainFence,
    drain_timeout: Duration,
    mut refresh: Refresh,
    mut passivate_children: Passivate,
    mut evict_worker: Evict,
) -> IdleEvictionSweepReport
where
    W: Clone + PartialEq,
    Workers: IntoIterator<Item = W>,
    Refresh: FnMut(&W) -> Result<WorkerResidencySnapshot, String>,
    Passivate: FnMut(&W, &WorkerResidencySnapshot, IdleEvictionMinutes, u128) -> Result<(), String>,
    Evict: FnMut(&W) -> Result<(), String>,
{
    let mut report = IdleEvictionSweepReport::default();
    let mut refreshed: Vec<(W, WorkerResidencySnapshot)> = Vec::new();
    for worker in workers {
        match refresh(&worker) {
            Ok(snapshot) => {
                report.refreshed_workers += 1;
                refreshed.push((worker, snapshot));
            }
            Err(_) => report.refresh_failures += 1,
        }
    }

    // As in prime-agent, whole-tree candidates do not receive child work:
    // stopping the worker releases the complete tree. Other workers get a
    // best-effort child passivation pass, and failed refreshes are removed
    // from this sweep rather than treated as evictable.
    let mut candidates = Vec::new();
    for (worker, snapshot) in &mut refreshed {
        if can_evict_worker(snapshot, policy, now_ms) {
            candidates.push(worker.clone());
            continue;
        }
        if passivate_children(worker, snapshot, policy, now_ms).is_ok() {
            report.passivated_workers += 1;
        } else {
            report.passivation_failures += 1;
        }
    }

    let Ok(_fence) = drain.try_begin_eviction(drain_timeout) else {
        report.drain_timed_out = true;
        return report;
    };

    // Re-read every candidate under the mutation fence. A successful first
    // refresh is not sufficient evidence: a mutation may have made scheduled
    // or busy work appear after candidate selection.
    let mut evictable = Vec::new();
    for worker in candidates {
        match refresh(&worker) {
            Ok(snapshot) if can_evict_worker(&snapshot, policy, now_ms) => {
                evictable.push(worker);
            }
            Ok(_) | Err(_) => {}
        }
    }
    for worker in evictable {
        match evict_worker(&worker) {
            Ok(()) => report.evicted_workers += 1,
            Err(_) => report.eviction_failures += 1,
        }
    }
    report
}

/// A repeating, unref-equivalent idle sweep timer.
///
/// Tokio tasks do not keep the process alive after the runtime begins
/// shutting down, unlike a dedicated blocking thread. The task is also
/// explicitly aborted on drop, so a stopped daemon cannot retain a timer.
pub struct IdleEvictionLoop {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl IdleEvictionLoop {
    pub fn start<F>(interval: Duration, sweep: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let interval = if interval.is_zero() {
            idle_eviction_sweep_interval(IdleEvictionMinutes::minutes(1))
        } else {
            interval
        };
        let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
        let sweep = Arc::new(sweep);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => sweep(),
                    _ = &mut cancelled => break,
                }
            }
        });
        Self {
            cancel: Some(cancel),
            task,
        }
    }

    pub fn stop(mut self) {
        self.cancel.take();
        self.task.abort();
    }
}

impl Drop for IdleEvictionLoop {
    fn drop(&mut self) {
        self.cancel.take();
        self.task.abort();
    }
}
