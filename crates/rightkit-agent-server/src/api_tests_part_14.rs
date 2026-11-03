#[tokio::test]
async fn subagent_spawn_records_runtime_middleware_spans() {
    use rightkit_agent_tools::coding_tools::SubagentSpawner;

    let root = unique_temp_dir("subagent-runtime-middleware");
    let state = DaemonState::new("");
    state.create_session_with_options(
        "parent",
        None,
        &root.to_string_lossy(),
        "Parent",
        None,
        SessionRuntimeOptions {
            execution_backend: Some("local".to_string()),
            execution_workspace_id: Some("workspace-1".to_string()),
            execution_network_profile: Some("local-host".to_string()),
            mcp_channel: "desktop".to_string(),
            ..SessionRuntimeOptions::default()
        },
    );

    let result = state
        .spawn(
            "auditor",
            "inspect the runtime middleware boundary",
            "parent",
            rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
        )
        .await;

    assert!(result.is_err());
    let spans = lock_recover(&state.runtime_middleware, "runtime_middleware").spans(Some("parent"));
    let start = spans
        .iter()
        .find(|span| span.phase == MiddlewarePhase::SubagentStart)
        .expect("subagent start span");
    let stop = spans
        .iter()
        .find(|span| span.phase == MiddlewarePhase::SubagentStop)
        .expect("subagent stop span");

    assert_eq!(start.status, "started");
    assert_eq!(stop.status, "failed");
    assert_eq!(stop.parent_id.as_deref(), Some(start.id.as_str()));
    assert_eq!(start.attributes["parentSessionId"], "parent");
    assert_eq!(start.attributes["role"], "auditor");
    assert_eq!(start.attributes["executionBackend"], "local");
    assert_eq!(start.attributes["executionWorkspaceId"], "workspace-1");
    assert_eq!(start.attributes["executionNetworkProfile"], "local-host");
    assert_eq!(start.attributes["mcpChannel"], "desktop");
    assert_eq!(stop.attributes["failureStage"], "sendTask");
    assert_eq!(
        stop.attributes["subSessionId"],
        start.attributes["subSessionId"]
    );
    assert!(stop.attributes["middlewareIds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "builtin.trace-subagent-stop"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn subagent_worktree_cleanup_runs_when_child_cannot_start() {
    use rightkit_agent_tools::coding_tools::SubagentSpawner;

    let root = unique_temp_dir("subagent-worktree-cleanup");
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
                        branch: Some("codex/subagent-cleanup".into()),
                        path: Some(worktree.to_string_lossy().into_owned()),
                        cleanup: true,
                    },
                ),
                max_iterations: None,
                ..Default::default()
            },
        )
        .await;

    assert!(result.is_err());
    assert!(
        !worktree.exists(),
        "worktree should be cleaned after failed child start"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn subagent_spawn_publishes_subagent_stop_hook_on_child_failure() {
    use rightkit_agent_tools::coding_tools::SubagentSpawner;

    let root = unique_temp_dir("subagent-stop-hook");
    let repo = root.join("repo");
    write_git_diff_fixture(&repo);
    let mut state = DaemonState::new("").with_engine(EngineConfig {
        api_key: "test-key".into(),
        model_id: "test-model".into(),
        base_url: "http://127.0.0.1:9".into(),
        fallback_models: Vec::new(),
    });
    let mut registry = AgentRegistry::with_builtins();
    registry.apply_config_overrides(&std::collections::HashMap::from([(
        "coder".to_string(),
        (
            "test-model".to_string(),
            "unreachable-test-provider".to_string(),
        ),
    )]));
    state.agent_registry = Arc::new(registry);
    state.create_session("parent", None, &repo.to_string_lossy(), "Parent", None);

    let spawn = state.spawn(
        "coder",
        "complete a task that will fail because the provider is unreachable",
        "parent",
        rightkit_agent_tools::coding_tools::SubagentSpawnOptions {
            worktree: None,
            max_iterations: Some(1),
            ..Default::default()
        },
    );
    let spawn_handle = tokio::spawn(spawn);

    let child = loop {
        if let Some(child) = {
            let sessions = lock_recover(&state.sessions, "sessions");
            sessions
                .values()
                .find(|session| session.parent_session_id.as_deref() == Some("parent"))
                .cloned()
        } {
            break child;
        }
        tokio::task::yield_now().await;
    };
    let mut hooks = child.hook_bus.subscribe();

    let result = spawn_handle.await.expect("spawn task should not panic");
    assert!(result.is_err());

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = hooks.recv().await.expect("hook bus should remain open");
            if matches!(
                event,
                rightkit_hooks::agent_hooks::HookEvent::SubagentStop { .. }
            ) {
                break event;
            }
        }
    })
    .await
    .expect("SubagentStop hook should be published");
    assert_eq!(
        event,
        rightkit_hooks::agent_hooks::HookEvent::SubagentStop {
            session_id: child.id.clone(),
            role: "coder".into(),
            success: false,
        }
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn slash_diff_reports_real_git_status_and_patch() {
    let root = unique_temp_dir("slash-diff");
    let workspace = root.join("workspace");
    write_git_diff_fixture(&workspace);
    let app = build_router(DaemonState::new(""));

    let diff = execute_slash_command(app, &format!("/diff {}", workspace.display())).await;

    assert_eq!(diff["success"], true, "{diff}");
    let output = diff["output"].as_str().unwrap();
    assert!(output.contains("**Diff:**"), "{output}");
    assert!(output.contains("tracked.txt"), "{output}");
    assert!(output.contains("-before"), "{output}");
    assert!(output.contains("+after"), "{output}");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn slash_review_returns_auditor_code_review_action() {
    let review = rightkit_agent_host_traits::skill_host::commands::CommandRegistry::new()
        .execute("review", &["focus on test coverage".to_string()])
        .expect("review command handler");

    assert!(review.success);
    assert!(review.output.contains("read-only auditor review"));
    let rightkit_agent_host_traits::skill_host::CommandAction::ConductGoal(action) = review.action
    else {
        panic!("review must produce a conductor goal");
    };
    assert!(action.contains("categorized as `code-review`"));
    assert!(action.contains("routes this to `auditor`"));
    assert!(action.contains("not the plan-review jury/council"));
    assert!(action.contains("Findings should lead"));
    assert!(action.contains("Additional focus: focus on test coverage"));
}

#[tokio::test]
async fn slash_status_reports_real_daemon_state() {
    let state = state_with_session("s1").with_runtime_info(61234, "/tmp/coderight-test.db");
    let app = build_router(state);

    let status = execute_slash_command(app, "/status").await;

    assert_eq!(status["success"], true);
    let output = status["output"].as_str().unwrap();
    assert!(output.contains("**RightKit status**"), "{output}");
    assert!(output.contains("Port: 61234"), "{output}");
    assert!(
        output.contains("Database: /tmp/coderight-test.db"),
        "{output}"
    );
    assert!(output.contains("Live sessions: 1"), "{output}");
    assert!(output.contains("s1"), "{output}");
    assert!(
        !output.contains("Status is shown in the top bar"),
        "{output}"
    );
}

#[tokio::test]
async fn slash_context_reports_runtime_and_workspace_context() {
    let workspace = unique_temp_dir("slash-context");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("AGENTS.md"), "Workspace instructions").unwrap();

    let state = state_with_session("ctx-session").with_runtime_info(61235, "ctx.db");
    let app = build_router(state);

    let context = execute_slash_command(app, &format!("/context {}", workspace.display())).await;

    assert_eq!(context["success"], true);
    let output = context["output"].as_str().unwrap();
    assert!(output.contains("**Context:**"), "{output}");
    assert!(output.contains("Live sessions: 1"), "{output}");
    assert!(output.contains("Instruction sources: 1"), "{output}");
    assert!(
        output.contains("Durable memories: Membrane (Pulse)"),
        "{output}"
    );
    assert!(output.contains("Tools:"), "{output}");
    // A048/A049: the budget and retained-delivery inventory are reported
    // per live session, typed unavailable when nothing was observed.
    assert!(
        output.contains("Host request budget (H8) and retained delivery inventory (H9):"),
        "{output}"
    );
    assert!(
        output.contains("- ctx-session: ") && output.contains("unavailable"),
        "{output}"
    );
    assert!(output.contains("ctx-session"), "{output}");
    assert!(output.contains("AGENTS.md"), "{output}");
}

#[tokio::test]
async fn slash_caps_reports_real_tool_capabilities() {
    let app = build_router(DaemonState::new(""));

    let caps = execute_slash_command(app, "/caps").await;

    assert_eq!(caps["success"], true);
    let output = caps["output"].as_str().unwrap();
    assert!(output.contains("RightKit capabilities"), "{output}");
    assert!(output.contains("### files"), "{output}");
    assert!(output.contains("`read_file`"), "{output}");
    assert!(output.contains("`shell`"), "{output}");
    assert!(output.contains("mutating"), "{output}");
}

#[tokio::test]
async fn slash_doctor_reports_runtime_diagnostics() {
    let app = build_router(DaemonState::new(""));

    let doctor = execute_slash_command(app, "/doctor").await;

    assert_eq!(doctor["success"], true);
    let output = doctor["output"].as_str().unwrap();
    assert!(output.contains("RightKit doctor"), "{output}");
    assert!(output.contains("Verdict:"), "{output}");
    assert!(output.contains("Provider key"), "{output}");
    assert!(
        !output.contains("Diagnostics are available from `/health/diagnostics`"),
        "{output}"
    );
}
