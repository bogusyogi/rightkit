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
        let progress = rep.uploaded + rep.downloaded + rep.tombstones_applied + rep.tombstones_written;
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
        PassOutcome::Unavailable(_) | PassOutcome::Failed(_) => engine
            .unwrap_or_else(|| policy.backoff.delay_secs(failures.max(1), None)),
    };
    Some(secs.max(policy.min_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> SchedulePolicy {
        SchedulePolicy::default()
    }

    #[test]
    fn synced_waits_the_idle_period() {
        assert_eq!(next_delay(&PassOutcome::Synced, None, 0, &p()), Some(900.0));
    }

    #[test]
    fn disabled_and_consent_schedule_nothing() {
        assert_eq!(next_delay(&PassOutcome::Disabled, None, 0, &p()), None);
        let req = ConsentRequest {
            from: "a".into(),
            to: "b".into(),
        };
        assert_eq!(
            next_delay(&PassOutcome::ConsentRequired(req.clone()), None, 0, &p()),
            None
        );
        assert_eq!(work_outcome(&PassOutcome::Disabled), WorkOutcome::Failure);
        assert_eq!(
            work_outcome(&PassOutcome::ConsentRequired(req)),
            WorkOutcome::Failure
        );
    }

    #[test]
    fn pending_uses_the_engine_figure_else_the_floor() {
        let o = PassOutcome::Pending("x".into());
        assert_eq!(next_delay(&o, Some(45.0), 0, &p()), Some(45.0));
        assert_eq!(next_delay(&o, None, 0, &p()), Some(30.0));
        assert_eq!(work_outcome(&o), WorkOutcome::Retry);
    }

    #[test]
    fn deferred_keeps_the_remaining_window_and_never_goes_below_the_minimum() {
        assert_eq!(
            next_delay(&PassOutcome::Deferred { secs: 240.0 }, Some(240.0), 3, &p()),
            Some(240.0)
        );
        assert_eq!(
            next_delay(&PassOutcome::Deferred { secs: 0.4 }, Some(0.4), 3, &p()),
            Some(15.0)
        );
        assert_eq!(
            next_delay(&PassOutcome::Deferred { secs: f64::NAN }, None, 0, &p()),
            Some(15.0)
        );
    }

    #[test]
    fn failures_prefer_the_engine_figure_and_fall_back_to_the_exponential() {
        let f = PassOutcome::Failed("e".into());
        // The engine's figure already includes a server hint larger than the policy.
        assert_eq!(next_delay(&f, Some(600.0), 2, &p()), Some(600.0));
        // No engine figure: 15 s doubling per failure, capped at 900 s.
        assert_eq!(next_delay(&f, None, 1, &p()), Some(15.0));
        assert_eq!(next_delay(&f, None, 2, &p()), Some(30.0));
        assert_eq!(next_delay(&f, None, 4, &p()), Some(120.0));
        assert_eq!(next_delay(&f, None, 50, &p()), Some(900.0));
        // Zero failures is treated as one.
        assert_eq!(next_delay(&f, None, 0, &p()), Some(15.0));
        let u = PassOutcome::Unavailable("signed out".into());
        assert_eq!(next_delay(&u, None, 3, &p()), Some(60.0));
        assert_eq!(work_outcome(&u), WorkOutcome::Retry);
    }

    #[test]
    fn non_finite_or_non_positive_engine_figures_are_ignored() {
        let f = PassOutcome::Failed("e".into());
        assert_eq!(next_delay(&f, Some(f64::INFINITY), 2, &p()), Some(30.0));
        assert_eq!(next_delay(&f, Some(-5.0), 2, &p()), Some(30.0));
        assert_eq!(next_delay(&f, Some(0.0), 2, &p()), Some(30.0));
    }

    #[test]
    fn cancelled_retries_soon() {
        assert_eq!(
            next_delay(&PassOutcome::Cancelled, None, 0, &p()),
            Some(30.0)
        );
        assert_eq!(work_outcome(&PassOutcome::Cancelled), WorkOutcome::Retry);
        assert_eq!(work_outcome(&PassOutcome::Synced), WorkOutcome::Success);
    }

    #[test]
    fn a_larger_minimum_lifts_every_delay() {
        let mut pol = p();
        pol.min_secs = 60.0;
        assert_eq!(
            next_delay(&PassOutcome::Pending("x".into()), Some(30.0), 0, &pol),
            Some(60.0)
        );
        assert_eq!(
            next_delay(&PassOutcome::Failed("e".into()), None, 1, &pol),
            Some(60.0)
        );
    }

    fn cont(
        outcome: &PassOutcome,
        retry: Option<f64>,
        progress: usize,
        passes: u32,
        budget: &SyncBudget,
        elapsed: Duration,
    ) -> bool {
        should_continue(outcome, retry, progress, passes, budget.max_passes, elapsed, budget)
    }

    #[test]
    fn the_default_budget_is_one_pass() {
        let b = SyncBudget::default();
        let pending = PassOutcome::Pending("x".into());
        assert!(!cont(&pending, Some(30.0), 5, 1, &b, Duration::ZERO));
    }

    #[test]
    fn a_drain_loop_needs_pending_progress_a_short_wait_and_budget_left() {
        let b = SyncBudget {
            max_passes: 3,
            max_elapsed: Duration::from_secs(10),
            max_inline_delay_secs: 1.0,
        };
        let pending = PassOutcome::Pending("x".into());
        assert!(cont(&pending, Some(0.5), 2, 1, &b, Duration::from_secs(1)));
        assert!(cont(&pending, None, 2, 1, &b, Duration::from_secs(1)));
        // The engine wants to wait longer than the inline allowance.
        assert!(!cont(&pending, Some(30.0), 2, 1, &b, Duration::from_secs(1)));
        // No progress: another pass would do the same.
        assert!(!cont(&pending, Some(0.5), 0, 1, &b, Duration::from_secs(1)));
        // Pass cap and time cap.
        assert!(!cont(&pending, Some(0.5), 2, 3, &b, Duration::from_secs(1)));
        assert!(!cont(&pending, Some(0.5), 2, 1, &b, Duration::from_secs(10)));
        // Only Pending continues.
        assert!(!cont(&PassOutcome::Synced, None, 2, 1, &b, Duration::ZERO));
        assert!(!cont(
            &PassOutcome::Failed("e".into()),
            Some(0.5),
            2,
            1,
            &b,
            Duration::ZERO
        ));
        assert!(!cont(
            &PassOutcome::Deferred { secs: 0.5 },
            Some(0.5),
            2,
            1,
            &b,
            Duration::ZERO
        ));
    }
}
