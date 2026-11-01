use super::*;

#[tokio::test]
async fn task_with_escalation_path_exhausts_after_failures() {
    let mut g = TaskGraph::new();
    let mut a = node("a", &[], "coder");
    a.escalation_path = "reviewer".to_string();
    g.add_task(a).unwrap();

    let outcome = run_graph(
        "",
        &mut g,
        &registry(),
        &AgentRouter::new(),
        Arc::new(FailWorker), // Task will fail
        0,
        |_| {},
    )
    .await;

    // Task fails on first attempt (coder fails).
    // Escalates to reviewer, but reviewer is also simulated by FailWorker,
    // so it fails again. With escalation_path exhausted, task goes to Failed.
    assert!(!outcome.completed);
    assert!(outcome.failed.contains(&"a".to_string()));
    assert_eq!(g.get("a").unwrap().state, TaskState::Failed);
    assert_eq!(g.get("a").unwrap().escalation_path, "");
}

#[tokio::test]
async fn task_fails_when_escalation_path_exhausted() {
    let mut g = TaskGraph::new();
    let mut a = node("a", &[], "coder");
    a.escalation_path = String::new(); // No escalation
    g.add_task(a).unwrap();

    let outcome = run_graph(
        "",
        &mut g,
        &registry(),
        &AgentRouter::new(),
        Arc::new(FailWorker),
        0,
        |_| {},
    )
    .await;

    assert!(!outcome.completed);
    assert!(outcome.failed.contains(&"a".to_string()));
    assert_eq!(g.get("a").unwrap().state, TaskState::Failed);
}

#[tokio::test]
async fn predicates_unmet_on_success_fails_task() {
    use rightkit_agent_engine_protocol::DoneWhenPredicate;

    let mut g = TaskGraph::new();
    let mut a = node("a", &[], "coder");
    a.done_when_predicates = vec![DoneWhenPredicate {
        predicate: "contains:PASSED".to_string(),
        weight: 1.0,
    }];
    g.add_task(a).unwrap();

    // EchoWorker echoes the payload ("Your task: do a"), which never contains
    // "PASSED", so the DoneWhen predicate is unsatisfied. With no escalation
    // path, the task fails rather than being marked Done on unverified
    // self-reported success.
    let outcome = run_graph(
        "",
        &mut g,
        &registry(),
        &AgentRouter::new(),
        Arc::new(EchoWorker),
        0,
        |_| {},
    )
    .await;

    assert!(!outcome.completed);
    assert_eq!(g.get("a").unwrap().state, TaskState::Failed);
}

#[tokio::test]
async fn predicates_met_on_success_completes_task() {
    use rightkit_agent_engine_protocol::DoneWhenPredicate;

    let mut g = TaskGraph::new();
    let mut a = node("a", &[], "coder");
    a.done_when = "tests pass".to_string();
    a.done_when_predicates = vec![DoneWhenPredicate {
        predicate: "contains:tests pass".to_string(),
        weight: 1.0,
    }];
    g.add_task(a).unwrap();

    // EchoWorker echoes the payload, which includes "Done when: tests pass",
    // satisfying the predicate, so the task completes.
    let outcome = run_graph(
        "",
        &mut g,
        &registry(),
        &AgentRouter::new(),
        Arc::new(EchoWorker),
        0,
        |_| {},
    )
    .await;

    assert!(outcome.completed);
    assert_eq!(g.get("a").unwrap().state, TaskState::Done);
}

#[tokio::test]
async fn empty_predicates_trust_success() {
    let mut g = TaskGraph::new();
    let a = node("a", &[], "coder");
    // No done_when_predicates
    g.add_task(a).unwrap();

    let outcome = run_graph(
        "",
        &mut g,
        &registry(),
        &AgentRouter::new(),
        Arc::new(EchoWorker),
        0,
        |_| {},
    )
    .await;

    // With no predicates and success, task should be Done
    assert!(outcome.completed);
    assert_eq!(g.get("a").unwrap().state, TaskState::Done);
}

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

    #[tokio::test]
    async fn partial_with_limit_never_counts_as_spec_complete_even_with_matching_predicate() {
        use rightkit_agent_engine_protocol::DoneWhenPredicate;

        let mut g = TaskGraph::new();
        let mut a = node("a", &[], "coder");
        // A predicate that WOULD be satisfied by the partial text alone —
        // proves the gate is `outcome`-driven, not just predicate-driven.
        a.done_when_predicates = vec![DoneWhenPredicate {
            predicate: "contains:draft".to_string(),
            weight: 1.0,
        }];
        g.add_task(a).unwrap();

        let worker = Arc::new(FixedOutcomeWorker {
            output: "partial draft before the 429 hit",
            outcome: WorkerOutcome::PartialWithLimit {
                limit_kind: "rate_limited".into(),
            },
        });

        let outcome = run_graph(
            "",
            &mut g,
            &registry(),
            &AgentRouter::new(),
            worker,
            0,
            |_| {},
        )
        .await;

        assert!(
            !outcome.completed,
            "a rate-limit cutoff must never satisfy spec-completion, even with success:true and a matching predicate"
        );
        assert_eq!(g.get("a").unwrap().state, TaskState::Failed);
        assert!(outcome.failed.contains(&"a".to_string()));
    }

    #[tokio::test]
    async fn api_error_with_no_partial_output_never_counts_as_spec_complete() {
        let mut g = TaskGraph::new();
        g.add_task(node("a", &[], "coder")).unwrap();

        let worker = Arc::new(FixedOutcomeWorker {
            output: "",
            outcome: WorkerOutcome::ApiError {
                error: "connection reset by peer".into(),
                partial_output: None,
            },
        });

        let outcome = run_graph(
            "",
            &mut g,
            &registry(),
            &AgentRouter::new(),
            worker,
            0,
            |_| {},
        )
        .await;

        assert!(!outcome.completed);
        assert_eq!(g.get("a").unwrap().state, TaskState::Failed);
    }

    #[tokio::test]
    async fn complete_outcome_path_is_byte_identical_to_today() {
        // A worker reporting `WorkerOutcome::Complete` (the default every
        // existing construction site uses) must behave exactly as before
        // this change: `empty_predicates_trust_success` still passes with
        // no predicates configured.
        let mut g = TaskGraph::new();
        g.add_task(node("a", &[], "coder")).unwrap();

        let worker = Arc::new(FixedOutcomeWorker {
            output: "Your task: do a",
            outcome: WorkerOutcome::Complete,
        });

        let outcome = run_graph(
            "",
            &mut g,
            &registry(),
            &AgentRouter::new(),
            worker,
            0,
            |_| {},
        )
        .await;

        assert!(outcome.completed);
        assert_eq!(g.get("a").unwrap().state, TaskState::Done);
    }
}

#[tokio::test]
async fn schemaless_descriptor_runs_unchanged() {
    let mut g = TaskGraph::new();
    g.add_task(node("a", &[], "coder")).unwrap();

    let outcome = run_graph(
        "",
        &mut g,
        &registry(),
        &AgentRouter::new(),
        Arc::new(EchoWorker),
        0,
        |_| {},
    )
    .await;

    assert!(outcome.completed);
    assert!(outcome.failed.is_empty());
    // The payload is now a handoff brief containing the task description.
    assert!(outcome.results[0].output.contains("do a"));
    assert!(outcome.results[0].output.contains("Handoff Brief"));
}
