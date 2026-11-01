//! Bounded autonomous progress audits for an explicit goal.
//!
//! The loop is deliberately an audit loop, not an executor: each iteration
//! asks the injected small-model client whether the supplied goal is on track.
//! It never invents a completion result or performs an external effect. The
//! caller owns the goal/context and can persist the returned audits and
//! blockers when the host is suspended.

use crate::small_model_utility::{
    SmallModelBudget, SmallModelClient, SmallModelError, SmallModelInput, SmallModelOutput,
    SmallModelTask, DEFAULT_CALL_TOKEN_ESTIMATE, DEFAULT_INPUT_BYTE_CAP,
};
use crate::watchdog::RunWatchdog;
use std::time::{Duration, Instant};

/// Default maximum number of progress audits in one autonomous run.
pub const DEFAULT_MAX_ITERATIONS: u32 = 8;
/// Default wall-clock ceiling for one autonomous audit run.
pub const DEFAULT_MAX_WALL_CLOCK: Duration = Duration::from_secs(30);
const CANCELLATION_POLL: Duration = Duration::from_millis(5);

/// Limits one goal-audit run so a background or disconnected host cannot leave
/// a model loop running indefinitely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalLoopConfig {
    /// Hard upper bound on model calls, independent of model budget.
    pub max_iterations: u32,
    /// Hard wall-clock ceiling, including time spent in a model call.
    pub max_wall_clock: Duration,
    /// Maximum bytes sent to each audit call.
    pub input_byte_cap: usize,
    /// Delay between completed audits. Zero is useful for deterministic hosts
    /// and tests; a non-zero value makes repeated audits periodic.
    pub interval: Duration,
}

impl Default for GoalLoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: DEFAULT_MAX_ITERATIONS,
            max_wall_clock: DEFAULT_MAX_WALL_CLOCK,
            input_byte_cap: DEFAULT_INPUT_BYTE_CAP,
            interval: Duration::from_secs(1),
        }
    }
}

/// One typed progress finding returned by the small-model helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalAudit {
    /// Whether the model judged the goal to remain on track.
    pub on_track: bool,
    /// Short explanation supplied by the model.
    pub summary: String,
    /// Concrete blockers supplied by the model.
    pub blockers: Vec<String>,
}

/// Why a bounded goal-audit run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalLoopStopReason {
    /// The caller's small-model call or token budget could not fund another audit.
    BudgetExhausted,
    /// An owner cancelled the watchdog while the loop was running.
    WatchdogCancelled,
    /// Repeated no-progress audits crossed the watchdog threshold.
    WatchdogStalled,
    /// The explicit iteration ceiling was reached.
    MaxIterations,
    /// The explicit wall-clock ceiling was reached.
    WallClockExceeded,
}

/// Complete, resumable output from one bounded audit run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalLoopReport {
    /// Audits completed before the stop reason was reached.
    pub audits: Vec<GoalAudit>,
    /// All blockers surfaced by completed audits, in observation order.
    pub blockers: Vec<String>,
    /// The first bound or cancellation that stopped the run.
    pub stop_reason: GoalLoopStopReason,
}

/// Failure from the injected audit client. No unavailable provider is treated
/// as a successful audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalLoopError {
    /// The small-model helper failed for a reason other than exhausted budget.
    SmallModel(SmallModelError),
    /// The client returned an output for a different task shape.
    UnexpectedOutput,
}

impl std::fmt::Display for GoalLoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SmallModel(error) => write!(f, "goal audit small-model error: {error}"),
            Self::UnexpectedOutput => write!(f, "goal audit client returned an unexpected output"),
        }
    }
}

impl std::error::Error for GoalLoopError {}

/// Run bounded progress audits against `goal` and `context`.
///
/// The watchdog is passed by value so the loop can update its no-progress
/// counter while a clone held by the owner can call [`RunWatchdog::cancel`].
/// The supplied budget is also local to this run; callers persist usage by
/// observing the number of returned audits rather than sharing mutable state
/// across an await boundary.
pub async fn run_goal_loop(
    client: &dyn SmallModelClient,
    goal: &str,
    context: &str,
    mut budget: SmallModelBudget,
    config: GoalLoopConfig,
    mut watchdog: RunWatchdog,
) -> Result<GoalLoopReport, GoalLoopError> {
    let started = Instant::now();
    let mut audits = Vec::new();
    let mut blockers = Vec::new();

    for iteration in 0..config.max_iterations {
        if watchdog.is_cancelled() {
            return Ok(report(
                audits,
                blockers,
                GoalLoopStopReason::WatchdogCancelled,
            ));
        }
        if watchdog.is_stalled() {
            return Ok(report(
                audits,
                blockers,
                GoalLoopStopReason::WatchdogStalled,
            ));
        }
        let Some(remaining) = config.max_wall_clock.checked_sub(started.elapsed()) else {
            return Ok(report(
                audits,
                blockers,
                GoalLoopStopReason::WallClockExceeded,
            ));
        };
        if remaining.is_zero() {
            return Ok(report(
                audits,
                blockers,
                GoalLoopStopReason::WallClockExceeded,
            ));
        }
        if !budget.can_afford(DEFAULT_CALL_TOKEN_ESTIMATE) {
            return Ok(report(
                audits,
                blockers,
                GoalLoopStopReason::BudgetExhausted,
            ));
        }

        let input = SmallModelInput::bounded(
            &format!("Goal:\n{goal}\n\nCurrent progress/context:\n{context}"),
            config.input_byte_cap,
        );
        let call = client.run(SmallModelTask::ProgressAudit, &input, &budget);
        let output = tokio::select! {
            result = call => result,
            _ = tokio::time::sleep(remaining) => {
                return Ok(report(audits, blockers, GoalLoopStopReason::WallClockExceeded));
            }
            _ = wait_for_cancellation(watchdog.clone()) => {
                return Ok(report(audits, blockers, GoalLoopStopReason::WatchdogCancelled));
            }
        };

        let output = match output {
            Ok(output) => output,
            Err(SmallModelError::BudgetExceeded) => {
                return Ok(report(
                    audits,
                    blockers,
                    GoalLoopStopReason::BudgetExhausted,
                ));
            }
            Err(error) => return Err(GoalLoopError::SmallModel(error)),
        };
        let SmallModelOutput::ProgressAudit {
            on_track,
            summary,
            blockers: audit_blockers,
        } = output
        else {
            return Err(GoalLoopError::UnexpectedOutput);
        };

        budget.spend(DEFAULT_CALL_TOKEN_ESTIMATE);
        if on_track {
            watchdog.record_progress();
        } else {
            watchdog.tick_without_progress();
        }
        blockers.extend(audit_blockers.iter().cloned());
        audits.push(GoalAudit {
            on_track,
            summary,
            blockers: audit_blockers,
        });

        if iteration + 1 == config.max_iterations {
            return Ok(report(audits, blockers, GoalLoopStopReason::MaxIterations));
        }
        if watchdog.is_cancelled() {
            return Ok(report(
                audits,
                blockers,
                GoalLoopStopReason::WatchdogCancelled,
            ));
        }
        let Some(remaining) = config.max_wall_clock.checked_sub(started.elapsed()) else {
            return Ok(report(
                audits,
                blockers,
                GoalLoopStopReason::WallClockExceeded,
            ));
        };
        if !wait_for_next_audit(config.interval, remaining, watchdog.clone()).await {
            if watchdog.is_cancelled() {
                return Ok(report(
                    audits,
                    blockers,
                    GoalLoopStopReason::WatchdogCancelled,
                ));
            }
            return Ok(report(
                audits,
                blockers,
                GoalLoopStopReason::WallClockExceeded,
            ));
        }
    }

    Ok(report(audits, blockers, GoalLoopStopReason::MaxIterations))
}

fn report(
    audits: Vec<GoalAudit>,
    blockers: Vec<String>,
    stop_reason: GoalLoopStopReason,
) -> GoalLoopReport {
    GoalLoopReport {
        audits,
        blockers,
        stop_reason,
    }
}

async fn wait_for_cancellation(watchdog: RunWatchdog) {
    loop {
        if watchdog.is_cancelled() {
            return;
        }
        tokio::time::sleep(CANCELLATION_POLL).await;
    }
}

async fn wait_for_next_audit(
    interval: Duration,
    remaining: Duration,
    watchdog: RunWatchdog,
) -> bool {
    if interval.is_zero() {
        return !watchdog.is_cancelled();
    }
    // The cancellation future takes its own handle so the sleep arm can still
    // consult the watchdog when it wins the select.
    let cancel_watchdog = watchdog.clone();
    tokio::select! {
        _ = tokio::time::sleep(interval.min(remaining)) => interval < remaining && !watchdog.is_cancelled(),
        _ = wait_for_cancellation(cancel_watchdog) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct StubClient {
        calls: Arc<AtomicUsize>,
        started: Option<Arc<tokio::sync::Notify>>,
        output: SmallModelOutput,
        wait: bool,
    }

    #[async_trait]
    impl SmallModelClient for StubClient {
        async fn run(
            &self,
            _task: SmallModelTask,
            _input: &SmallModelInput,
            budget: &SmallModelBudget,
        ) -> Result<SmallModelOutput, SmallModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = &self.started {
                started.notify_one();
            }
            if !budget.can_afford(DEFAULT_CALL_TOKEN_ESTIMATE) {
                return Err(SmallModelError::BudgetExceeded);
            }
            if self.wait {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            Ok(self.output.clone())
        }
    }

    fn config(max_iterations: u32) -> GoalLoopConfig {
        GoalLoopConfig {
            max_iterations,
            max_wall_clock: Duration::from_secs(2),
            input_byte_cap: 1_000,
            interval: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn budget_exhaustion_stops_the_loop() {
        let calls = Arc::new(AtomicUsize::new(0));
        let client = StubClient {
            calls: calls.clone(),
            started: None,
            output: SmallModelOutput::ProgressAudit {
                on_track: true,
                summary: "moving".into(),
                blockers: Vec::new(),
            },
            wait: false,
        };
        let report = run_goal_loop(
            &client,
            "ship it",
            "one step complete",
            SmallModelBudget::new(1, DEFAULT_CALL_TOKEN_ESTIMATE),
            config(10),
            RunWatchdog::new(3),
        )
        .await
        .expect("budget exhaustion is a normal stop");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.audits.len(), 1);
        assert_eq!(report.stop_reason, GoalLoopStopReason::BudgetExhausted);
    }

    #[tokio::test]
    async fn watchdog_cancellation_stops_an_in_flight_loop() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let client = StubClient {
            calls: calls.clone(),
            started: Some(started.clone()),
            output: SmallModelOutput::ProgressAudit {
                on_track: true,
                summary: "waiting".into(),
                blockers: Vec::new(),
            },
            wait: true,
        };
        let owner_watchdog = RunWatchdog::new(3);
        let loop_watchdog = owner_watchdog.clone();
        let task = tokio::spawn(async move {
            run_goal_loop(
                &client,
                "ship it",
                "waiting",
                SmallModelBudget::new(10, 10 * DEFAULT_CALL_TOKEN_ESTIMATE),
                config(10),
                loop_watchdog,
            )
            .await
        });
        started.notified().await;
        owner_watchdog.cancel();
        let report = task
            .await
            .expect("loop task must finish")
            .expect("cancel is normal");

        assert_eq!(report.stop_reason, GoalLoopStopReason::WatchdogCancelled);
        assert!(report.audits.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn off_track_audit_surfaces_blockers() {
        let client = StubClient {
            calls: Arc::new(AtomicUsize::new(0)),
            started: None,
            output: SmallModelOutput::ProgressAudit {
                on_track: false,
                summary: "blocked on review".into(),
                blockers: vec!["approval is missing".into(), "CI is red".into()],
            },
            wait: false,
        };
        let report = run_goal_loop(
            &client,
            "ship it",
            "review pending",
            SmallModelBudget::new(2, 2 * DEFAULT_CALL_TOKEN_ESTIMATE),
            config(1),
            RunWatchdog::new(3),
        )
        .await
        .expect("audit should succeed");

        assert!(!report.audits[0].on_track);
        assert_eq!(
            report.blockers,
            vec!["approval is missing".to_string(), "CI is red".to_string()]
        );
    }
}
