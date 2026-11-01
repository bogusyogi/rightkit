use super::*;
use crate::coding::conductor::{EchoWorker, FailWorker};
use crate::coding::registry::AgentDescriptor;
use rightkit_agent_engine_protocol::TaskNode;

fn node(id: &str, deps: &[&str], role: &str) -> TaskNode {
    TaskNode {
        id: id.into(),
        title: format!("do {id}"),
        deps: deps.iter().map(|s| s.to_string()).collect(),
        done_when: format!("worker output contains the task marker do {id}"),
        done_when_predicates: vec![rightkit_agent_engine_protocol::DoneWhenPredicate {
            predicate: format!("contains:do {id}"),
            weight: 1.0,
        }],
        state: TaskState::Open,
        category: String::new(),
        estimated_duration: String::new(),
        assigned_role: role.into(),
        escalation_path: String::new(),
        open_questions: vec![],
        created_at: "t".into(),
        updated_at: "t".into(),
    }
}

fn registry() -> AgentRegistry {
    let mut r = AgentRegistry::new();
    r.register(AgentDescriptor::new("coder", "coder", "m"));
    r.register(AgentDescriptor::new("reviewer", "reviewer", "m"));
    r
}

#[path = "orchestrator_tests/contracts.rs"]
mod contracts;
#[path = "orchestrator_tests/execution.rs"]
mod execution;
#[path = "orchestrator_tests/predicates.rs"]
mod predicates;

#[tokio::test]
async fn resumable_graph_skips_completed_nodes_after_interruption() {
    use crate::coding::checkpoint::{
        run_graph_resumable, run_graph_resumable_with_conductor_and_handoff_archive,
        GraphCheckpointStore,
    };
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct RecordingWorker {
        calls: Arc<Mutex<Vec<WorkerTask>>>,
    }

    #[async_trait]
    impl Worker for RecordingWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            self.calls.lock().unwrap().push(task.clone());
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: format!("output-{}; completed do {}", task.task_id, task.task_id),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: crate::coding::conductor::WorkerOutcome::Complete,
            }
        }
    }

    fn checkpoint_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("rightkit-agent-dagcheckpoint-{nanos}.sqlite"))
    }

    let path = checkpoint_path();
    let store = GraphCheckpointStore::open(&path).unwrap();
    let run_id = "resume-mid-run";
    let first_calls = Arc::new(Mutex::new(Vec::new()));
    let mut first_graph = TaskGraph::new();
    first_graph.add_task(node("a", &[], "coder")).unwrap();
    first_graph.add_task(node("b", &["a"], "coder")).unwrap();
    first_graph.add_task(node("c", &["b"], "coder")).unwrap();

    let first_store = store.clone();
    let first_calls_for_worker = first_calls.clone();
    let interrupted = tokio::spawn(async move {
        run_graph_resumable(
            run_id,
            "ship it",
            &mut first_graph,
            &registry(),
            &AgentRouter::new(),
            Arc::new(RecordingWorker {
                calls: first_calls_for_worker,
            }),
            1,
            &first_store,
            |graph| {
                if graph.get("a").unwrap().state == TaskState::Done {
                    panic!("simulated process interruption after checkpoint");
                }
            },
        )
        .await
    })
    .await;
    assert!(interrupted.unwrap_err().is_panic());
    assert_eq!(first_calls.lock().unwrap()[0].task_id, "a");
    assert_eq!(store.pending_ready(run_id).unwrap(), vec!["b"]);

    let mut second_graph = TaskGraph::new();
    second_graph.add_task(node("a", &[], "coder")).unwrap();
    second_graph.add_task(node("b", &["a"], "coder")).unwrap();
    second_graph.add_task(node("c", &["b"], "coder")).unwrap();
    let second_calls = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_graph_resumable_with_conductor_and_handoff_archive(
        run_id,
        "ship it",
        &mut second_graph,
        &registry(),
        &AgentRouter::new(),
        Arc::new(RecordingWorker {
            calls: second_calls.clone(),
        }),
        1,
        &store,
        |_| {},
        Some("resume-conductor".into()),
        Some("2026-07-17".into()),
        None,
    )
    .await
    .unwrap();

    assert!(outcome.completed);
    let resumed_calls = second_calls.lock().unwrap();
    assert_eq!(
        resumed_calls
            .iter()
            .map(|task| task.task_id.as_str())
            .collect::<Vec<_>>(),
        ["b", "c"]
    );
    assert!(
        resumed_calls[0].payload.contains("output-a"),
        "restored dependency output must remain available to the first resumed handoff"
    );
    assert!(resumed_calls
        .iter()
        .all(|task| task.conductor_stable_cache_key.is_some()));
    assert_eq!(second_graph.get("a").unwrap().state, TaskState::Done);
    assert!(store
        .node_output(run_id, "a")
        .unwrap()
        .unwrap()
        .starts_with("output-a"));

    let _ = std::fs::remove_file(path);
}

// ---------------------------------------------------------------------------
// Cross-worker prefix-cache sharing (ROADMAP P1)
// ---------------------------------------------------------------------------

mod cross_worker_cache_sharing {
    use super::*;
    use crate::coding::conductor_cache_key::conductor_stable_cache_key;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct CapturingWorker {
        observed: Arc<Mutex<Vec<WorkerTask>>>,
    }

    #[async_trait]
    impl Worker for CapturingWorker {
        async fn run(&self, task: &WorkerTask) -> WorkerResult {
            self.observed.lock().unwrap().push(task.clone());
            WorkerResult {
                task_id: task.task_id.clone(),
                agent: task.agent.clone(),
                success: true,
                output: format!("output-{}; completed do {}", task.task_id, task.task_id),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: task.agent_version.clone(),
                rollback_step: None,
                outcome: crate::coding::conductor::WorkerOutcome::Complete,
            }
        }
    }

    fn dep_free_node(id: &str, role: &str) -> TaskNode {
        node(id, &[], role)
    }

    #[tokio::test]
    async fn run_graph_with_conductor_stamps_shared_key_on_every_sibling() {
        // End-to-end check of the public API: a single conductor run
        // with `conductor_identity` + `conductor_day` set MUST stamp the
        // same stable cache key onto every sibling worker task, so the
        // siblings cache-read against each other's prefix slot instead
        // of each independently recomputing.
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut graph = TaskGraph::new();
        graph.add_task(dep_free_node("alpha", "coder")).unwrap();
        graph.add_task(dep_free_node("beta", "coder")).unwrap();
        graph.add_task(dep_free_node("gamma", "coder")).unwrap();

        let outcome = run_graph_with_conductor(
            "ship it",
            &mut graph,
            &registry(),
            &AgentRouter::new(),
            Arc::new(CapturingWorker {
                observed: observed.clone(),
            }),
            0,
            |_| {},
            Some("rightkit-agent-conductor-v1".into()),
            Some("2026-06-30".into()),
        )
        .await;

        assert!(outcome.completed);

        let captured = observed.lock().unwrap();
        assert_eq!(captured.len(), 3, "all three siblings must be captured");

        let expected_key = conductor_stable_cache_key("rightkit-agent-conductor-v1", "2026-06-30");
        for (i, task) in captured.iter().enumerate() {
            assert_eq!(
                task.conductor_stable_cache_key,
                Some(expected_key.clone()),
                "sibling #{i} ({}) must carry the conductor's shared stable cache key",
                task.task_id
            );
        }
        // All siblings carry the SAME key (cache-read, not cache-break).
        let distinct_keys: std::collections::HashSet<_> = captured
            .iter()
            .filter_map(|t| t.conductor_stable_cache_key.clone())
            .collect();
        assert_eq!(
            distinct_keys.len(),
            1,
            "all siblings must collapse to a single shared stable cache key"
        );
    }

    #[tokio::test]
    async fn run_graph_without_conductor_identity_leaves_tasks_uninherited() {
        // Single-worker / direct path: `run_graph` (no conductor
        // identity) must NOT stamp an inherited key. Tasks pass through
        // with `conductor_stable_cache_key = None` so the worker
        // derives its own stable key via `SessionEngine`.
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut graph = TaskGraph::new();
        graph.add_task(dep_free_node("only", "coder")).unwrap();

        let outcome = run_graph(
            "do the thing",
            &mut graph,
            &registry(),
            &AgentRouter::new(),
            Arc::new(CapturingWorker {
                observed: observed.clone(),
            }),
            0,
            |_| {},
        )
        .await;

        assert!(outcome.completed);

        let captured = observed.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].conductor_stable_cache_key, None,
            "single-worker (direct) path must NOT carry an inherited conductor key"
        );
    }
}
