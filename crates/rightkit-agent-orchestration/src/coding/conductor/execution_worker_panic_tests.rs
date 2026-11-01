//! Repro + regression coverage for audit B-1 (worker-panic hazard in
//! `Conductor::run_parallel`).
//!
//! `run_parallel` spawns each worker's `run()` call inside a bare
//! `tokio::spawn` and never inspects the resulting `JoinHandle`. If a worker
//! panics, the spawned task unwinds, its `tx` sender clone is dropped without
//! ever sending `(index, result)`, and that plan index's slot in `slots`
//! stays `None`. The verdict this test settles: does the `.expect("every
//! plan index must receive exactly one result")` at the end of
//! `run_parallel` actually fire when that happens, panicking the WHOLE
//! conductor run (not just the one worker)?

#[cfg(test)]
mod worker_panic_tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::coding::conductor::execution::{Conductor, Worker};
    use crate::coding::conductor::types::{ConductorPlan, WorkerResult, WorkerTask};
    use crate::coding::failure::FailureKind;

    fn task(id: &str) -> WorkerTask {
        WorkerTask {
            task_id: id.into(),
            agent: "agent".into(),
            payload: format!("payload-{id}"),
            agent_version: None,
            conductor_stable_cache_key: None,
        }
    }

    fn plan(ids: &[&str], parallel: bool) -> ConductorPlan {
        ConductorPlan {
            tasks: ids.iter().map(|id| task(id)).collect(),
            parallel,
            conductor_identity: None,
            conductor_day: None,
        }
    }

    /// Panics for every task whose id is in `panic_on`; otherwise succeeds.
    struct SometimesPanicsWorker {
        panic_on: Vec<String>,
    }

    #[async_trait]
    impl Worker for SometimesPanicsWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            if self.panic_on.contains(&task.task_id) {
                panic!("simulated worker panic for {}", task.task_id);
            }
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: task.payload.clone(),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: crate::coding::conductor::types::WorkerOutcome::Complete,
            }
        }
    }

    /// THE VERDICT TEST: one sibling worker (out of several) panics under
    /// `run_parallel`. Before the fix, this either (a) propagates the panic
    /// out of `run_parallel` via the `.expect()` on the missing slot, taking
    /// every sibling result down with it, or (b) hangs forever if the
    /// dropped-sender accounting doesn't close the channel. After the fix,
    /// the panicking worker's slot must resolve to a `WorkerResult` with
    /// `success: false` and `failure_kind: Some(FailureKind::WorkerPanic)`,
    /// while every sibling's real result survives untouched.
    #[tokio::test]
    async fn one_worker_panicking_does_not_take_down_the_whole_run() {
        let worker = Arc::new(SometimesPanicsWorker {
            panic_on: vec!["b".to_string()],
        });
        let conductor = Conductor::new(worker);

        let results = conductor.run_parallel(&plan(&["a", "b", "c"], true)).await;

        assert_eq!(
            results.len(),
            3,
            "a panicking worker must not shrink the result set — every plan index still needs an entry"
        );

        let by_id: std::collections::HashMap<&str, &WorkerResult> =
            results.iter().map(|r| (r.task_id.as_str(), r)).collect();

        assert!(by_id["a"].success, "sibling 'a' must complete normally");
        assert_eq!(by_id["a"].output, "payload-a");
        assert!(by_id["c"].success, "sibling 'c' must complete normally");
        assert_eq!(by_id["c"].output, "payload-c");

        let panicked = by_id["b"];
        assert!(
            !panicked.success,
            "the panicking worker's slot must resolve to a failed result, not be silently dropped"
        );
        assert_eq!(
            panicked.failure_kind,
            Some(FailureKind::WorkerPanic),
            "a worker panic must be typed as FailureKind::WorkerPanic so callers can distinguish it from a normal failure"
        );
    }

    /// Multiple siblings panicking simultaneously — every panicking slot
    /// must resolve independently; no cross-contamination between them.
    #[tokio::test]
    async fn multiple_workers_panicking_each_resolve_independently() {
        let worker = Arc::new(SometimesPanicsWorker {
            panic_on: vec!["b".to_string(), "d".to_string()],
        });
        let conductor = Conductor::new(worker);

        let results = conductor
            .run_parallel(&plan(&["a", "b", "c", "d"], true))
            .await;

        assert_eq!(results.len(), 4);
        let by_id: std::collections::HashMap<&str, &WorkerResult> =
            results.iter().map(|r| (r.task_id.as_str(), r)).collect();

        assert!(by_id["a"].success);
        assert!(by_id["c"].success);
        assert!(!by_id["b"].success);
        assert!(!by_id["d"].success);
        assert_eq!(by_id["b"].failure_kind, Some(FailureKind::WorkerPanic));
        assert_eq!(by_id["d"].failure_kind, Some(FailureKind::WorkerPanic));
    }

    /// A worker that panics on EVERY task (the whole plan fails) must still
    /// return one failed result per task rather than panicking the caller.
    #[tokio::test]
    async fn every_worker_panicking_still_returns_a_full_result_set() {
        let worker = Arc::new(SometimesPanicsWorker {
            panic_on: vec!["x".to_string(), "y".to_string()],
        });
        let conductor = Conductor::new(worker);

        let results = conductor.run_parallel(&plan(&["x", "y"], true)).await;

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| !r.success));
        assert!(results
            .iter()
            .all(|r| r.failure_kind == Some(FailureKind::WorkerPanic)));
    }

    /// Durable task identity is preserved even when completion order changes.
    #[tokio::test]
    async fn task_identity_preserved_across_a_panicking_sibling() {
        let worker = Arc::new(SometimesPanicsWorker {
            panic_on: vec!["t2".to_string()],
        });
        let conductor = Conductor::new(worker);

        let results = conductor
            .run_parallel(&plan(&["t0", "t1", "t2", "t3"], true))
            .await;

        let ids: std::collections::HashSet<_> =
            results.iter().map(|r| r.task_id.as_str()).collect();
        assert_eq!(
            ids,
            std::collections::HashSet::from(["t0", "t1", "t2", "t3"])
        );
    }
}
