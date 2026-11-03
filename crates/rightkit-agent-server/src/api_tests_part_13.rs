#[tokio::test]
async fn benchmark_proof_route_reports_aggregate_numbers() {
    let store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
    store
        .record_benchmark(
            "bench-a",
            "coder",
            "model-a",
            "suite-a",
            3,
            2,
            "2026-06-06T00:00:00Z",
        )
        .unwrap();
    store
        .record_benchmark_with_detail(
            "bench-b",
            "coder",
            "model-a",
            rightkit_agent_orchestration::coding::TbenchCatalog::suite_id(),
            1,
            1,
            "2026-06-06T00:01:00Z",
            Some(&rightkit_agent_engine_storage::BenchmarkDetail {
                task_id: Some("tb-001".to_string()),
                passed: Some(true),
                agent_output: Some("done".to_string()),
                verify_output: Some("ok".to_string()),
                verify_error: Some(String::new()),
            }),
        )
        .unwrap();
    let storage = Arc::new(tokio::sync::Mutex::new(store));
    let app = build_router(DaemonState::new("").with_storage(storage));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/runtime/benchmarks/proof")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["totalReports"], 2);
    assert_eq!(json["totalRepeats"], 4);
    assert_eq!(json["totalSuccesses"], 3);
    assert_eq!(json["successRate"], 0.75);
    assert_eq!(json["terminalBench"]["reports"], 1);
    assert_eq!(json["terminalBench"]["passed"], 1);
    assert_eq!(json["terminalBench"]["passRate"], 1.0);
    assert!(json["suites"]
        .as_array()
        .unwrap()
        .iter()
        .any(|suite| suite["suite"] == "suite-a" && suite["successRate"] == 2.0 / 3.0));
    assert!(json["roleModels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|role_model| role_model["role"] == "coder"
            && role_model["model"] == "model-a"
            && role_model["successRate"] == 0.75));
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_terminal_bench_run_records_history() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage);
    state.create_session("s1", None, "/tmp", "Benchmark parent", None);
    let app = build_router(state);

    let run = execute_slash_command(app.clone(), "/terminal-bench run tb-001 coder s1").await;
    assert_eq!(run["success"], true);
    let output = run["output"].as_str().unwrap();
    assert!(output.contains("RightKit task pack tb-001"), "{output}");
    assert!(output.contains("Recorded in `/benchmarks`"), "{output}");

    let benchmarks = execute_slash_command(app, "/benchmarks").await;
    assert_eq!(benchmarks["success"], true);
    assert!(benchmarks["output"].as_str().unwrap().contains("coder on"));
}

#[tokio::test(flavor = "multi_thread")]
async fn session_fork_route_creates_child_with_parent_lineage() {
    let root = unique_temp_dir("session-fork");
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage);
    state.create_session_with_options(
        "parent",
        None,
        &root.to_string_lossy(),
        "Parent",
        None,
        SessionRuntimeOptions {
            allowed_tools: vec!["read_file".to_string()],
            execution_backend: Some("local".to_string()),
            mcp_channel: "desktop".to_string(),
            ..SessionRuntimeOptions::default()
        },
    );
    let app = build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/parent/fork")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id":"child","title":"Try B"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "child");
    assert_eq!(json["parentSessionId"], "parent");
    assert_eq!(json["title"], "Try B");
    assert_eq!(json["session"]["parentSessionId"], "parent");
    assert_eq!(json["session"]["forkedFromSessionId"], "parent");
    assert_eq!(
        json["session"]["allowedTools"],
        serde_json::json!(["read_file"])
    );
    assert_eq!(json["session"]["executionBackend"], "local");

    let sessions = app
        .oneshot(
            Request::builder()
                .uri("/sessions?includeSubagents=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sessions.status(), StatusCode::OK);
    let body = sessions.into_body().collect().await.unwrap().to_bytes();
    let rows: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let child = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "child")
        .expect("forked session listed");
    assert_eq!(child["parentSessionId"], "parent");
    assert_eq!(child["forkedFromSessionId"], "parent");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn subagent_spawn_can_use_isolated_git_worktree_workspace() {
    use rightkit_agent_tools::coding_tools::SubagentSpawner;

    let root = unique_temp_dir("subagent-worktree");
    let repo = root.join("repo");
    let worktree = root.join("agent-worktree");
    write_git_diff_fixture(&repo);
    let state = DaemonState::new("");
    state.create_session("parent", None, &repo.to_string_lossy(), "Parent", None);

    let result = state
        .spawn(
            "coder",
            "make an isolated change",
            "parent",
            rightkit_agent_tools::coding_tools::SubagentSpawnOptions {
                worktree: Some(
                    rightkit_agent_tools::coding_tools::SubagentWorktreeOptions {
                        branch: Some("codex/subagent-isolation".into()),
                        path: Some(worktree.to_string_lossy().into_owned()),
                        cleanup: false,
                    },
                ),
                max_iterations: None,
                ..Default::default()
            },
        )
        .await;

    assert!(result.is_err());
    let child = {
        let sessions = lock_recover(&state.sessions, "sessions");
        sessions
            .values()
            .find(|session| session.parent_session_id.as_deref() == Some("parent"))
            .cloned()
            .expect("child session created")
    };
    assert_eq!(child.workspace_root, worktree.to_string_lossy());
    assert!(worktree.join(".git").exists());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_spawn_inherits_parent_execution_backend() {
    use rightkit_agent_tools::coding_tools::SubagentSpawner;

    let root = unique_temp_dir("subagent-backend");
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    let parent = state.create_session_with_options(
        "parent",
        None,
        &root.to_string_lossy(),
        "Parent",
        None,
        SessionRuntimeOptions {
            execution_backend: Some("local".to_string()),
            ..SessionRuntimeOptions::default()
        },
    );
    assert_eq!(parent.execution_backend.as_deref(), Some("local"));

    let result = state
        .spawn(
            "coder",
            "try a child task",
            "parent",
            rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
        )
        .await;

    assert!(result.is_err());
    let child = {
        let sessions = lock_recover(&state.sessions, "sessions");
        sessions
            .values()
            .find(|session| session.parent_session_id.as_deref() == Some("parent"))
            .cloned()
            .expect("child session created")
    };
    assert_eq!(child.execution_backend.as_deref(), Some("local"));

    let row = wait_for_session(&storage, &child.id).await;
    let config: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
    assert_eq!(config["executionBackend"], "local");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn subagent_fork_does_not_seed_parent_transcript() {
    use rightkit_agent_tools::coding_tools::SubagentSpawner;

    let root = unique_temp_dir("subagent-cache-safe-fork");
    let state = DaemonState::new("");
    let parent = state.create_session("parent", None, &root.to_string_lossy(), "Parent", None);
    lock_recover(&parent.transcript, "session.transcript").push(TranscriptMessage::User {
        id: "parent-user-1".to_string(),
        content: "parent-only cache sentinel".to_string(),
        created_at: now_iso(),
        pinned: false,
        trust: None,
        attachments: Vec::new(),
    });

    let result = state
        .spawn(
            "coder",
            "child task should start from fork metadata only",
            "parent",
            rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
        )
        .await;

    assert!(result.is_err());
    let child = {
        let sessions = lock_recover(&state.sessions, "sessions");
        sessions
            .values()
            .find(|session| session.parent_session_id.as_deref() == Some("parent"))
            .cloned()
            .expect("child session created")
    };
    let child_transcript = lock_recover(&child.transcript, "session.transcript");
    let child_json = serde_json::to_string(&*child_transcript).unwrap();
    assert!(
        !child_json.contains("parent-only cache sentinel"),
        "{child_json}"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn subagent_spawn_records_parent_sidechain_transcript_artifact() {
    use rightkit_agent_tools::coding_tools::SubagentSpawner;

    let root = unique_temp_dir("subagent-sidechain-artifact");
    let state = DaemonState::new("");
    let parent = state.create_session("parent", None, &root.to_string_lossy(), "Parent", None);

    let result = state
        .spawn(
            "auditor",
            "inspect without merging transcript into parent",
            "parent",
            rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
        )
        .await;

    assert!(result.is_err());
    let artifact = lock_recover(&parent.artifacts, "session.artifacts")
        .iter()
        .find(|artifact| artifact.kind == "subagent_transcript")
        .cloned()
        .expect("side-chain transcript artifact");
    assert!(artifact.path.starts_with("session://sub-"));
    assert!(artifact.path.ends_with("/transcript"));
    assert_eq!(artifact.producer, "auditor");
    assert!(artifact.content_hash.starts_with("sha256:"));
    let meta: serde_json::Value = serde_json::from_str(&artifact.meta).unwrap();
    assert_eq!(meta["parentSessionId"], "parent");
    assert_eq!(meta["role"], "auditor");
    assert_eq!(meta["sideChain"], true);
    assert_eq!(meta["cacheSafeFork"], true);
    assert_eq!(meta["providerCacheShared"], false);
    assert_eq!(meta["parentTranscriptSeeded"], false);
    assert_eq!(meta["parentTranscriptMerged"], false);
    assert_eq!(meta["seedMessageCount"], 0);
    assert_eq!(
        meta["transcriptRoute"],
        format!("/sessions/{}/transcript", artifact.provenance)
    );

    let _ = std::fs::remove_dir_all(root);
}
