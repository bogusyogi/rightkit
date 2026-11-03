//! Platform-neutral scheduler surface: one bounded sync pass and a suggested next delay.
//!
//! The host owns the clock and the process (Android WorkManager, a desktop timer, an iOS
//! background task); this module owns what a *pass* means for scheduling. [`sync_once`] runs the
//! engine for at most [`SyncBudget::max_passes`] passes inside [`SyncBudget::max_elapsed`], and
//! returns a [`SyncReport`] with what happened, how the host should map it ([`WorkOutcome`]) and
//! when to run next ([`SyncReport::next_delay_secs`]).
//!
//! It never sleeps and never spawns a thread: it cannot interrupt a pass already running (the
//! backend's own HTTP timeouts bound that), so the elapsed budget is checked between passes only.
//! Failure backoff is the engine's own, kept in the ledger (`consecutive_failures`,
//! `retry_after_ms`), so it survives the process being killed between WorkManager runs; this module
//! only turns the pass outcome into a delay the OS scheduler can use.
//!
//! The pure decision functions ([`next_delay`], [`work_outcome`], [`should_continue`]) take plain
//! values and are unit tested without an engine.

use std::time::{Duration, Instant};

use crate::backoff::BackoffPolicy;
use crate::engine::{PassOutcome, PassReport, SyncEngine};
use crate::ledger::ConsentRequest;
use crate::traits::RecordMapper;

/// What one [`sync_once`] call may spend.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncBudget {
    /// Most passes in one call. `1` (the default) is one pass.
    pub max_passes: u32,
    /// Wall-clock allowance, checked between passes (a running pass is not interrupted).
    pub max_elapsed: Duration,
    /// Another pass runs only after a `Pending` pass that made progress, and only when the engine
    /// asks to wait no longer than this. The default `0.0` never re-runs (the engine's pending
    /// floor is 30 s), so a host that wants a drain loop raises it knowingly. This function never
    /// sleeps: a re-run that the engine would defer is reported as `Deferred` and ends the call.
    pub max_inline_delay_secs: f64,
}

impl Default for SyncBudget {
    fn default() -> Self {
        Self {
            max_passes: 1,
            max_elapsed: Duration::from_secs(60),
            max_inline_delay_secs: 0.0,
        }
    }
}

/// Delays the scheduler suggests. `backoff` should match the engine's `SyncConfig::backoff`.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedulePolicy {
    pub backoff: BackoffPolicy,
    /// Delay after a fully synced pass. Android's periodic WorkManager minimum is 15 minutes.
    pub idle_secs: f64,
    /// No suggested delay is shorter than this.
    pub min_secs: f64,
    /// Suggested delay after a pass that could not give an engine delay (cancelled).
    pub cancelled_secs: f64,
}

impl Default for SchedulePolicy {
    fn default() -> Self {
        Self {
            backoff: BackoffPolicy::default(),
            idle_secs: 900.0,
            min_secs: 15.0,
            cancelled_secs: 30.0,
        }
    }
}

/// How the host should report the run to its scheduler.
///
/// WorkManager mapping: `Success` -> `Result.success()`, `Retry` -> `Result.retry()`,
/// `Failure` -> `Result.failure()` (do not retry; the host decides how to surface it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkOutcome {
    /// Nothing for the scheduler to redo before the next regular run.
    Success,
    /// Run again after `next_delay_secs` (pending work, a deferral, or a transient failure).
    Retry,
    /// Do not retry on its own: sync is off, or the app must act first (see
    /// [`SyncReport::consent_required`]).
    Failure,
}

/// What [`sync_once`] did, in plain data a host can pass across an FFI boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncReport {
    /// Passes actually run (a deferred pass counts: it was attempted and did nothing).
    pub passes: u32,
    /// The last pass's outcome.
    pub outcome: PassOutcome,
    pub uploaded: usize,
    pub downloaded: usize,
    pub tombstones_applied: usize,
    pub tombstones_written: usize,
    pub quarantined: usize,
    pub elapsed_ms: u64,
    pub work: WorkOutcome,
    /// Seconds until the host should run again; `None` when no run should be scheduled
    /// (sync disabled, or waiting on the app).
    pub next_delay_secs: Option<f64>,
    /// Set when the signed-in account changed and the app must call
    /// `SyncEngine::grant_consent`.
    pub consent_required: Option<ConsentRequest>,
}

/// Runs the engine for one bounded unit of work. Never sleeps; see the module docs.
pub fn sync_once<M: RecordMapper + 'static>(
    engine: &mut SyncEngine<M>,
    budget: &SyncBudget,
    policy: &SchedulePolicy,
) -> SyncReport {
    let started = Instant::now();
    let mut passes = 0u32;
    let mut total = PassReport {
        outcome: PassOutcome::Synced,
        uploaded: 0,
        downloaded: 0,
        tombstones_applied: 0,
        tombstones_written: 0,
        quarantined: 0,
        blocked_reuploads: 0,
        retry_after_secs: None,
    };
    let max_passes = budget.max_passes.max(1);
    loop {
        let rep = engine.run_pass();
        passes += 1;
        total.uploaded += rep.uploaded;
        total.downloaded += rep.downloaded;
        total.tombstones_applied += rep.tombstones_applied;
        total.tombstones_written += rep.tombstones_written;
        total.quarantined += rep.quarantined;
        total.blocked_reuploads += rep.blocked_reuploads;
        let progress =
            rep.uploaded + rep.downloaded + rep.tombstones_applied + rep.tombstones_written;
        total.outcome = rep.outcome;
        total.retry_after_secs = rep.retry_after_secs;
        if !should_continue(
            &total.outcome,
            total.retry_after_secs,
            progress,
            passes,
            max_passes,
            started.elapsed(),
            budget,
        ) {
            break;
        }
    }
    let failures = engine.ledger().consecutive_failures;
    let consent_required = match &total.outcome {
        PassOutcome::ConsentRequired(req) => Some(req.clone()),
        _ => None,
    };
    SyncReport {
        passes,
        work: work_outcome(&total.outcome),
        next_delay_secs: next_delay(&total.outcome, total.retry_after_secs, failures, policy),
        outcome: total.outcome,
        uploaded: total.uploaded,
        downloaded: total.downloaded,
        tombstones_applied: total.tombstones_applied,
        tombstones_written: total.tombstones_written,
        quarantined: total.quarantined,
        elapsed_ms: started.elapsed().as_millis() as u64,
        consent_required,
    }
}

/// Whether another pass should run inside the same call.
pub fn should_continue(
    outcome: &PassOutcome,
    retry_after_secs: Option<f64>,
    progress: usize,
    passes_run: u32,
    max_passes: u32,
    elapsed: Duration,
    budget: &SyncBudget,
) -> bool {
    if passes_run >= max_passes || elapsed >= budget.max_elapsed {
        return false;
    }
    if !matches!(outcome, PassOutcome::Pending(_)) || progress == 0 {
        return false;
    }
    match retry_after_secs {
        Some(s) => s.is_finite() && s <= budget.max_inline_delay_secs,
        None => true,
    }
}

/// How the host reports `outcome` to its scheduler.
pub fn work_outcome(outcome: &PassOutcome) -> WorkOutcome {
    match outcome {
        PassOutcome::Synced => WorkOutcome::Success,
        PassOutcome::Pending(_)
        | PassOutcome::Deferred { .. }
        | PassOutcome::Unavailable(_)
        | PassOutcome::Cancelled
        | PassOutcome::Failed(_) => WorkOutcome::Retry,
        PassOutcome::Disabled | PassOutcome::ConsentRequired(_) => WorkOutcome::Failure,
    }
}

/// Seconds until the next run, never below `policy.min_secs`; `None` when nothing should be
/// scheduled (disabled, or waiting for the app's consent answer).
///
/// `retry_after_secs` is the engine's own figure for the pass (its backoff, server hint and
/// pending floor already applied); `failures` is the ledger's consecutive failure count and is
/// used only when a failed pass carries no engine figure.
pub fn next_delay(
    outcome: &PassOutcome,
    retry_after_secs: Option<f64>,
    failures: u32,
    policy: &SchedulePolicy,
) -> Option<f64> {
    let engine = retry_after_secs.filter(|s| s.is_finite() && *s > 0.0);
    let secs = match outcome {
        PassOutcome::Disabled | PassOutcome::ConsentRequired(_) => return None,
        PassOutcome::Synced => policy.idle_secs,
        PassOutcome::Pending(_) => engine.unwrap_or(policy.backoff.pending_floor_secs),
        PassOutcome::Deferred { secs } => {
            if secs.is_finite() && *secs > 0.0 {
                *secs
            } else {
                policy.min_secs
            }
        }
        PassOutcome::Cancelled => engine.unwrap_or(policy.cancelled_secs),
        PassOutcome::Unavailable(_) | PassOutcome::Failed(_) => {
            engine.unwrap_or_else(|| policy.backoff.delay_secs(failures.max(1), None))
        }
    };
    Some(secs.max(policy.min_secs))
}
