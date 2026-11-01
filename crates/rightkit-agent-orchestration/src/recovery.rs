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

use crate::conductor::{Worker, WorkerOutcome, WorkerResult, WorkerTask};
use crate::failure::FailureKind;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Fails its first `fail_times` calls, then succeeds.
    struct FlakyWorker {
        fail_times: u32,
        calls: AtomicU32,
    }

    #[async_trait]
    impl Worker for FlakyWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let success = n >= self.fail_times;
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success,
                output: if success {
                    "ok".into()
                } else {
                    "transient".into()
                },
                input_tokens: 0,
                mop_fired: false,
                failure_kind: if success {
                    None
                } else {
                    Some(FailureKind::Unknown)
                },
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: WorkerOutcome::Complete,
            }
        }
    }

    /// Sleeps far longer than any test timeout.
    struct HangingWorker;

    #[async_trait]
    impl Worker for HangingWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            tokio::time::sleep(Duration::from_secs(30)).await;
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: "eventually".into(),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: WorkerOutcome::Complete,
            }
        }
    }

    fn task() -> WorkerTask {
        WorkerTask {
            task_id: "t".into(),
            agent: "a".into(),
            payload: "p".into(),
            agent_version: None,
            conductor_stable_cache_key: None,
        }
    }

    #[tokio::test]
    async fn retries_until_success() {
        let inner = Arc::new(FlakyWorker {
            fail_times: 2,
            calls: AtomicU32::new(0),
        });
        let w = RecoveringWorker::new(inner, 3, Duration::from_secs(5));
        let r = w.run(&task()).await;
        assert!(r.success, "should succeed on the third attempt");
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let inner = Arc::new(FlakyWorker {
            fail_times: 100,
            calls: AtomicU32::new(0),
        });
        let w = RecoveringWorker::new(inner, 3, Duration::from_secs(5));
        let r = w.run(&task()).await;
        assert!(!r.success);
    }

    #[tokio::test]
    async fn timeout_is_recovered_not_hung() {
        let w = RecoveringWorker::new(Arc::new(HangingWorker), 2, Duration::from_millis(40));
        let r = w.run(&task()).await;
        assert!(!r.success);
        assert!(r.output.contains("stalled"), "got: {}", r.output);
    }

    #[tokio::test]
    async fn hook_fires_per_failed_attempt() {
        let count = Arc::new(AtomicU32::new(0));
        let c2 = count.clone();
        let inner = Arc::new(FlakyWorker {
            fail_times: 100,
            calls: AtomicU32::new(0),
        });
        let w = RecoveringWorker::new(inner, 3, Duration::from_secs(5)).with_hook(Arc::new(
            move |_note| {
                c2.fetch_add(1, Ordering::SeqCst);
            },
        ));
        let _ = w.run(&task()).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "one note per failed attempt"
        );
    }

    #[test]
    fn locates_last_good_step_before_failure() {
        let output = "read_file ✓\nedit_file ✓\nshell ✗ error: permission denied";
        let steps = StepLocator::locate(output);
        assert_eq!(steps.len(), 3);
        assert!(steps[0].succeeded);
        assert!(steps[1].succeeded);
        assert!(!steps[2].succeeded);
        assert_eq!(StepLocator::last_good_step(&steps), Some(1));
    }

    #[test]
    fn returns_none_when_no_success() {
        let output = "shell ✗ error";
        let steps = StepLocator::locate(output);
        assert_eq!(steps.len(), 1);
        assert!(!steps[0].succeeded);
        assert_eq!(StepLocator::last_good_step(&steps), None);
    }

    #[test]
    fn all_success_returns_last_index() {
        let output = "read_file ✓\nedit_file ✓\nwrite_file ✓";
        let steps = StepLocator::locate(output);
        assert_eq!(steps.len(), 3);
        assert_eq!(StepLocator::last_good_step(&steps), Some(2));
    }

    #[test]
    fn empty_output_returns_none() {
        let output = "";
        let steps = StepLocator::locate(output);
        assert!(steps.is_empty());
        assert_eq!(StepLocator::last_good_step(&steps), None);
    }

    #[test]
    fn detects_multiple_actions_per_line() {
        let output = "read_file ✓ and then edit_file ✓";
        let steps = StepLocator::locate(output);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].action, "read_file");
    }

    #[test]
    fn recognizes_various_success_signals() {
        let output1 = "read_file success";
        let steps1 = StepLocator::locate(output1);
        assert!(steps1[0].succeeded);

        let output2 = "edit_file completed";
        let steps2 = StepLocator::locate(output2);
        assert!(steps2[0].succeeded);

        let output3 = "shell done";
        let steps3 = StepLocator::locate(output3);
        assert!(steps3[0].succeeded);
    }

    #[test]
    fn recognizes_various_failure_signals() {
        let output1 = "read_file error";
        let steps1 = StepLocator::locate(output1);
        assert!(!steps1[0].succeeded);

        let output2 = "edit_file failed";
        let steps2 = StepLocator::locate(output2);
        assert!(!steps2[0].succeeded);

        let output3 = "shell panic";
        let steps3 = StepLocator::locate(output3);
        assert!(!steps3[0].succeeded);
    }
}
