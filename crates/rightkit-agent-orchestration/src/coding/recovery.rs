//! Worker recovery — a [`Worker`] decorator that adds a per-task timeout and
//! bounded restarts so a single stuck or failing agent cannot stall the whole
//! conductor run.
//!
//! This is the runtime half of the watchdog/recovery policy: a task that makes no progress within the timeout
//! is treated as stuck and restarted (up to `max_attempts`); once attempts are
//! exhausted the task is aborted (returned as a failed [`WorkerResult`]), which
//! the orchestration loop records as a failed task rather than hanging forever.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::coding::conductor::{Worker, WorkerOutcome, WorkerResult, WorkerTask};
use crate::coding::failure::FailureKind;

/// A single identified step in an agent's output trajectory.
#[derive(Debug, Clone)]
pub struct TrajectoryStep {
    /// Zero-based index into the action sequence.
    pub index: usize,
    /// The action label (e.g. "read_file", "edit_file", "shell").
    pub action: String,
    /// Whether this step appears to have succeeded (heuristic).
    pub succeeded: bool,
}

/// Locates and analyzes trajectory steps in agent output.
pub struct StepLocator;

impl StepLocator {
    /// Parse the agent's output text into a sequence of identified steps.
    /// Heuristic: looks for lines that match tool-call patterns:
    ///   - lines containing "✓" or "success" after an action name → succeeded
    ///   - lines containing "✗", "error", "failed", "Error" → failed
    ///   - bare action-name lines (read_file, edit_file, shell, search) → unknown
    pub fn locate(output: &str) -> Vec<TrajectoryStep> {
        let action_names = [
            "read_file",
            "edit_file",
            "write_file",
            "shell",
            "search",
            "list_files",
            "apply_patch",
            "git",
            "web_search",
            "browser_fetch",
        ];

        let mut steps = Vec::new();
        let mut index = 0;

        for line in output.lines() {
            let line_lower = line.to_lowercase();

            // Check if this line mentions any action
            let mut found_action = None;
            for action in &action_names {
                if line_lower.contains(action) {
                    found_action = Some(action.to_string());
                    break;
                }
            }

            if let Some(action) = found_action {
                // Determine if this step succeeded
                let succeeded = (line_lower.contains("✓")
                    || line_lower.contains("success")
                    || line_lower.contains("succeeded")
                    || line_lower.contains("completed")
                    || line_lower.contains("done"))
                    && !line_lower.contains("✗")
                    && !line_lower.contains("error")
                    && !line_lower.contains("failed")
                    && !line_lower.contains("panic");

                steps.push(TrajectoryStep {
                    index,
                    action,
                    succeeded,
                });

                index += 1;
            }
        }

        steps
    }

    /// Returns the index of the last step that succeeded before the first failure,
    /// or None if no clear rollback point is found.
    pub fn last_good_step(steps: &[TrajectoryStep]) -> Option<usize> {
        let mut last_good = None;

        for step in steps {
            if !step.succeeded {
                // We found a failure; return the last good step before this
                return last_good;
            }
            last_good = Some(step.index);
        }

        // All steps succeeded or there were no steps
        last_good
    }
}

/// A recovery notification, emitted once per attempt that did not succeed.
/// `rollback_step` carries the [`StepLocator`] hint when a failing output
/// could be mapped to a last-good step — the daemon surfaces this via
/// `OrchestrationObservation` so the UI/API can show where to resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryNote {
    pub task_id: String,
    pub agent: String,
    pub attempt: u32,
    pub timed_out: bool,
    pub rollback_step: Option<usize>,
}

type RecoveryHook = Arc<dyn Fn(RecoveryNote) + Send + Sync>;

/// Wraps another [`Worker`], adding a per-attempt timeout and bounded restarts.
pub struct RecoveringWorker {
    inner: Arc<dyn Worker>,
    max_attempts: u32,
    per_task_timeout: Duration,
    on_recovery: Option<RecoveryHook>,
}

impl RecoveringWorker {
    pub fn new(inner: Arc<dyn Worker>, max_attempts: u32, per_task_timeout: Duration) -> Self {
        Self {
            inner,
            max_attempts: max_attempts.max(1),
            per_task_timeout,
            on_recovery: None,
        }
    }

    /// Register a hook invoked for each non-successful attempt (a stall/restart
    /// or a hard failure), so the daemon can stream recovery observability.
    pub fn with_hook(mut self, hook: RecoveryHook) -> Self {
        self.on_recovery = Some(hook);
        self
    }

    fn note(&self, task: &WorkerTask, attempt: u32, timed_out: bool, rollback_step: Option<usize>) {
        if let Some(hook) = &self.on_recovery {
            hook(RecoveryNote {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                attempt,
                timed_out,
                rollback_step,
            });
        }
    }
}

/// Every refusal the spawn admission path produces ends with "refusing to
/// spawn" (`daemon_state.rs`), so one phrase identifies it across depth, run
/// budget, tier ceiling and role-pack refusals.
fn is_terminal_spawn_refusal(output: &str) -> bool {
    output.contains("refusing to spawn")
}

#[async_trait]
impl Worker for RecoveringWorker {
    async fn run(&self, task: &WorkerTask) -> WorkerResult {
        let mut last: Option<WorkerResult> = None;
        for attempt in 1..=self.max_attempts {
            match tokio::time::timeout(self.per_task_timeout, self.inner.run(task)).await {
                Ok(result) if result.success => return result,
                Ok(mut result) => {
                    // For failures (not timeouts), locate the last good step for potential rollback.
                    if !result.success {
                        let steps = StepLocator::locate(&result.output);
                        result.rollback_step = StepLocator::last_good_step(&steps);
                    }
                    self.note(task, attempt, false, result.rollback_step);
                    // A refused spawn (depth, concurrent, cumulative budget, cost
                    // ceiling, role pack) is a terminal task failure, not a
                    // transient one: retrying only burns the cumulative budget
                    // that caused it (orchestration rule 7, D7).
                    if is_terminal_spawn_refusal(&result.output) {
                        return result;
                    }
                    last = Some(result);
                }
                Err(_) => {
                    self.note(task, attempt, true, None);
                    last = Some(WorkerResult {
                        task_id: task.task_id.clone(),
                        agent: task.agent.clone(),
                        success: false,
                        output: format!(
                            "worker stalled: no result within {:?} (attempt {attempt}/{})",
                            self.per_task_timeout, self.max_attempts
                        ),
                        input_tokens: 0,
                        mop_fired: false,
                        failure_kind: Some(FailureKind::Timeout),
                        agent_version: task.agent_version.clone(),
                        rollback_step: None,
                        outcome: WorkerOutcome::PartialWithLimit {
                            limit_kind: "timeout".into(),
                        },
                    });
                }
            }
        }
        last.unwrap_or_else(|| WorkerResult {
            task_id: task.task_id.clone(),
            agent: task.agent.clone(),
            success: false,
            output: "worker produced no result".into(),
            input_tokens: 0,
            mop_fired: false,
            failure_kind: Some(FailureKind::Unknown),
            agent_version: task.agent_version.clone(),
            rollback_step: None,
            outcome: WorkerOutcome::Complete,
        })
    }
}
