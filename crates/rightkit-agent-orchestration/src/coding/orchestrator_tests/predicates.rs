use super::*;

// ---------------------------------------------------------------------------
// Subagent error propagation (parity P-2): a worker cut off by a rate limit
// or hit by an unrecovered API error must never be treated as satisfied for
// spec-completion, even if it (incorrectly) reports `success: true` with the
// partial text it produced before the cutoff.
// ---------------------------------------------------------------------------

mod subagent_error_propagation {
    use super::*;
    use crate::coding::conductor::{Worker, WorkerOutcome, WorkerResult, WorkerTask};
    use async_trait::async_trait;

    /// A worker that always reports `success: true` (as a real subagent
    /// spawner would after extracting whatever partial text it had), but
    /// whose `outcome` tells the true story. Mirrors the mock-worker-hitting
    /// a-429-mid-task scenario from the P-2 spec.
    struct FixedOutcomeWorker {
        output: &'static str,
        outcome: WorkerOutcome,
    }

    #[async_trait]
    impl Worker for FixedOutcomeWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: self.output.to_string(),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: self.outcome.clone(),
            }
        }
    }
}
