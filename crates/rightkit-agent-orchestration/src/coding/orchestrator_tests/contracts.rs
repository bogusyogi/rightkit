use super::*;

#[tokio::test]
async fn input_schema_rejects_task_before_worker_dispatch() {
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingWorker {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Worker for CountingWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: "should not run".into(),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: crate::coding::conductor::WorkerOutcome::Complete,
            }
        }
    }

    let mut registry = AgentRegistry::new();
    let mut agent = AgentDescriptor::new("coder", "coder", "m");
    agent.input_schema = Some(json!({
        "type": "object",
        "required": ["missing_contract_field"]
    }));
    registry.register(agent);

    let mut g = TaskGraph::new();
    g.add_task(node("a", &[], "coder")).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_graph(
        "",
        &mut g,
        &registry,
        &AgentRouter::new(),
        Arc::new(CountingWorker {
            calls: calls.clone(),
        }),
        0,
        |_| {},
    )
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!outcome.completed);
    assert_eq!(g.get("a").unwrap().state, TaskState::Failed);
    assert!(outcome.failed.contains(&"a".to_string()));
    assert!(outcome.results[0]
        .output
        .contains("input schema validation failed"));
}

#[tokio::test]
async fn output_schema_rejects_worker_result_before_marking_done() {
    use async_trait::async_trait;
    use serde_json::json;

    struct BadOutputWorker;

    #[async_trait]
    impl Worker for BadOutputWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: "missing-json-contract".into(),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: crate::coding::conductor::WorkerOutcome::Complete,
            }
        }
    }

    let mut registry = AgentRegistry::new();
    let mut agent = AgentDescriptor::new("coder", "coder", "m");
    agent.output_schema = Some(json!({
        "type": "object",
        "required": ["success", "output", "contract"],
        "properties": {
            "success": { "type": "boolean" },
            "output": { "type": "string" },
            "contract": { "type": "object" }
        }
    }));
    registry.register(agent);

    let mut g = TaskGraph::new();
    g.add_task(node("a", &[], "coder")).unwrap();

    let outcome = run_graph(
        "",
        &mut g,
        &registry,
        &AgentRouter::new(),
        Arc::new(BadOutputWorker),
        0,
        |_| {},
    )
    .await;

    assert!(!outcome.completed);
    assert_eq!(g.get("a").unwrap().state, TaskState::Failed);
    assert!(outcome.failed.contains(&"a".to_string()));
    assert!(outcome.results[0]
        .output
        .contains("output schema validation failed"));
}
