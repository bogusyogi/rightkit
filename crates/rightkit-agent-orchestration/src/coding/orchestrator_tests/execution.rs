use super::*;

#[tokio::test]
async fn runs_all_tasks_respecting_deps() {
    let mut g = TaskGraph::new();
    g.add_task(node("a", &[], "coder")).unwrap();
    g.add_task(node("b", &["a"], "coder")).unwrap();
    g.add_task(node("c", &["b"], "reviewer")).unwrap();

    let mut updates = 0;
    let outcome = run_graph(
        "ship it",
        &mut g,
        &registry(),
        &AgentRouter::new(),
        Arc::new(EchoWorker),
        0,
        |_| updates += 1,
    )
    .await;

    assert!(outcome.completed, "all tasks should complete");
    assert!(outcome.failed.is_empty());
    assert_eq!(outcome.results.len(), 3);
    assert!(g.tasks().all(|t| t.state == TaskState::Done));
    assert!(updates > 0);
}

#[tokio::test]
async fn parallel_independent_tasks_complete() {
    let mut g = TaskGraph::new();
    g.add_task(node("a", &[], "coder")).unwrap();
    g.add_task(node("b", &[], "coder")).unwrap();
    g.add_task(node("c", &[], "reviewer")).unwrap();

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
    assert_eq!(outcome.results.len(), 3);
}

#[tokio::test]
async fn review_task_executes_jury_panel_not_single_worker() {
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct RecordingPanelWorker {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Worker for RecordingPanelWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            self.calls.lock().unwrap().push(task.task_id.clone());
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: "VERDICT: APPROVE\nCONFIDENCE: 0.90\npanel ok".into(),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: crate::coding::conductor::WorkerOutcome::Complete,
            }
        }
    }

    let mut g = TaskGraph::new();
    let mut review = node("review-a", &[], "");
    review.category = "review".into();
    review.done_when.clear();
    review.done_when_predicates.clear();
    g.add_task(review).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_graph(
        "ship it",
        &mut g,
        &AgentRegistry::with_builtins(),
        &AgentRouter::new(),
        Arc::new(RecordingPanelWorker {
            calls: calls.clone(),
        }),
        0,
        |_| {},
    )
    .await;

    assert!(outcome.completed);
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(outcome.results[0].agent, "reviewer-jury");
    assert_eq!(calls.lock().unwrap().len(), 6);
    assert!(outcome.results[0]
        .output
        .contains("Advisory Council Preflight"));
    assert!(outcome.results[0].output.contains("Verdict Jury"));
    assert!(outcome.results[0]
        .output
        .contains("Decision: Some(Approved"));
    assert_eq!(g.get("review-a").unwrap().state, TaskState::Done);
}

#[tokio::test]
async fn failed_task_blocks_dependents_and_reports() {
    let mut g = TaskGraph::new();
    g.add_task(node("a", &[], "coder")).unwrap();
    g.add_task(node("b", &["a"], "coder")).unwrap();

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

    assert!(!outcome.completed, "a failed, so the goal is not complete");
    assert!(outcome.failed.contains(&"a".to_string()));
    // b never runs because its dependency failed.
    assert_eq!(g.get("b").unwrap().state, TaskState::Blocked);
}

#[tokio::test]
async fn stalled_task_receives_one_forced_pivot_and_can_recover() {
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct PivotWorker {
        payloads: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Worker for PivotWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            let pivoted = task.payload.contains("[WATCHDOG FORCED PIVOT]");
            self.payloads.lock().unwrap().push(task.payload.clone());
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: pivoted,
                output: if pivoted {
                    "completed do a".into()
                } else {
                    "stalled".into()
                },
                input_tokens: 0,
                mop_fired: false,
                failure_kind: (!pivoted).then_some(crate::coding::failure::FailureKind::Unknown),
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: WorkerOutcome::Complete,
            }
        }
    }

    let payloads = Arc::new(Mutex::new(Vec::new()));
    let mut graph = TaskGraph::new();
    let mut stalled = node("a", &[], "coder");
    stalled.escalation_path = "coder,coder,coder".into();
    graph.add_task(stalled).unwrap();
    let outcome = run_graph(
        "ship it",
        &mut graph,
        &registry(),
        &AgentRouter::new(),
        Arc::new(PivotWorker {
            payloads: payloads.clone(),
        }),
        0,
        |_| {},
    )
    .await;

    assert!(outcome.completed);
    assert_eq!(graph.get("a").unwrap().state, TaskState::Done);
    assert_eq!(payloads.lock().unwrap().len(), 4);
    assert!(payloads.lock().unwrap()[3].contains("[WATCHDOG FORCED PIVOT]"));
    assert!(outcome.stall_nudge.is_none());
}

#[tokio::test]
async fn unroutable_task_fails_gracefully() {
    let mut g = TaskGraph::new();
    g.add_task(node("a", &[], "no-such-role")).unwrap();

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
    assert!(outcome.failed.contains(&"a".to_string()));
    assert_eq!(g.get("a").unwrap().state, TaskState::Failed);
}

#[tokio::test]
async fn empty_graph_is_not_completed() {
    let mut g = TaskGraph::new();
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
    assert!(outcome.results.is_empty());
}
