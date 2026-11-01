#[tokio::test]
async fn plugin_commands_use_manifest_metadata() {
    let plugin_dir = unique_temp_dir("plugins-command-metadata");
    let plugin_root = plugin_dir.join("fixture");
    std::fs::create_dir_all(&plugin_root).unwrap();
    std::fs::write(
        plugin_root.join("plugin.py"),
        r#"
import json
print(json.dumps({"success": True, "output": "ok"}))
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_root.join("plugin.json"),
        serde_json::json!({
            "id": "fixture",
            "name": "Fixture",
            "version": "1.0.0",
            "description": "fixture plugin",
            "entry": "plugin.py",
            "providesCommands": ["inspect"],
            "providesTools": [],
            "commands": {
                "inspect": {
                    "description": "Inspect plugin state",
                    "usage": "/inspect [target]"
                }
            },
            "enabled": true
        })
        .to_string(),
    )
    .unwrap();

    let app = build_router(DaemonState::new("").with_plugin_dir(plugin_dir.clone()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/commands")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let commands: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    let inspect = commands
        .iter()
        .find(|row| row["name"] == "inspect")
        .expect("inspect command row");
    assert_eq!(inspect["description"], "Inspect plugin state");
    assert_eq!(inspect["usage"], "/inspect [target]");
    assert_eq!(inspect["pluginId"], "fixture");

    let _ = std::fs::remove_dir_all(plugin_dir);
}

#[tokio::test]
async fn slash_usage_reports_latest_session_usage_event() {
    let state = state_with_session("usage-session");
    let live = state.get_session("usage-session").unwrap();
    live.conduit.publish(ProtocolEvent::UsageUpdated {
        session_id: "usage-session".into(),
        seq: live.conduit.next_seq(),
        created_at: now_iso(),
        prompt_tokens: 1200,
        completion_tokens: 340,
        cost_usd: 0.012345,
        context_tokens: 1540,
        context_percent: 0.42,
        cache_read_tokens: 100,
        cache_creation_tokens: 25,
        cost_known: true,
    });
    let app = build_router(state);

    let usage = execute_slash_command_for_session(app, "/usage", "usage-session").await;

    assert_eq!(usage["success"], true);
    let output = usage["output"].as_str().unwrap();
    assert!(output.contains("**Usage:** usage-session"), "{output}");
    assert!(output.contains("Prompt tokens: 1200"), "{output}");
    assert!(output.contains("Completion tokens: 340"), "{output}");
    assert!(output.contains("Context tokens: 1540 (42.0%)"), "{output}");
    assert!(output.contains("Cache read tokens: 100"), "{output}");
    assert!(output.contains("Cache creation tokens: 25"), "{output}");
    assert!(output.contains("Estimated cost: $0.012345"), "{output}");
}

#[tokio::test]
async fn slash_commands_report_real_mcp_and_plugin_state() {
    let root = unique_temp_dir("slash-runtime-state");
    let plugin_dir = root.join("plugins");
    write_test_plugin(&plugin_dir, "enabled-plugin", true);
    write_test_plugin(&plugin_dir, "disabled-plugin", false);
    let user_mcp = root.join("mcp.json");
    write_mcp_config(
        &user_mcp,
        serde_json::json!({
            "mcpServers": {
                "docs": { "type": "http", "url": "http://127.0.0.1:9555/mcp" }
            }
        }),
    );
    let app = build_router(
        DaemonState::new("")
            .with_plugin_dir(plugin_dir)
            .with_mcp_user_config_path(user_mcp),
    );

    let mcp = execute_slash_command(app.clone(), "/mcp").await;
    assert_eq!(mcp["success"], true);
    assert!(mcp["output"].as_str().unwrap().contains("docs"));
    assert!(mcp["output"].as_str().unwrap().contains("1 server"));
    assert!(mcp["output"]
        .as_str()
        .unwrap()
        .contains("coderight mcp-server"));
    assert!(mcp["output"].as_str().unwrap().contains("--allow-mutate"));

    let plugins = execute_slash_command(app, "/plugin").await;
    assert_eq!(plugins["success"], true);
    assert!(plugins["output"]
        .as_str()
        .unwrap()
        .contains("enabled-plugin"));
    assert!(plugins["output"]
        .as_str()
        .unwrap()
        .contains("disabled-plugin"));
    assert!(plugins["output"].as_str().unwrap().contains("1 enabled"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn slash_commands_report_real_runtime_records() {
    let root = unique_temp_dir("slash-runtime-records");
    let workspace = root.join("workspace");
    write_task_fixture(&workspace);
    write_schedule_fixture(&workspace);
    let state = DaemonState::new("").with_message_store_path(root.join("messages.json"));
    state.create_session("s1", None, "/tmp", "Sender", None);
    state.create_session("s2", None, "/tmp", "Target", None);
    rightkit_agent_tools::coding_tools::MessageDispatcher::dispatch(
        &state,
        rightkit_agent_tools::coding_tools::MessageDispatch {
            from_session_id: "s1".to_string(),
            target_session_id: Some("s2".to_string()),
            channel: "handoff".to_string(),
            message: "please inspect the diff".to_string(),
        },
    )
    .await
    .unwrap();
    let app = build_router(state);
    let workspace_arg = workspace.to_string_lossy();

    let messages = execute_slash_command(app.clone(), "/messages").await;
    assert_eq!(messages["success"], true);
    assert!(messages["output"].as_str().unwrap().contains("s1 -> s2"));
    assert!(messages["output"].as_str().unwrap().contains("handoff"));

    let tasks = execute_slash_command(app.clone(), &format!("/tasks {workspace_arg}")).await;
    assert_eq!(tasks["success"], true);
    assert!(tasks["output"].as_str().unwrap().contains("task-42"));
    assert!(tasks["output"].as_str().unwrap().contains("cargo test"));

    let schedules = execute_slash_command(app, &format!("/schedules {workspace_arg}")).await;
    assert_eq!(schedules["success"], true);
    assert!(schedules["output"]
        .as_str()
        .unwrap()
        .contains("daily-review"));
    assert!(schedules["output"].as_str().unwrap().contains("0 9 * * *"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn spec_slash_command_creates_analyzes_and_traces_specs() {
    let state = DaemonState::new("");
    state.create_session(
        "spec-slash-session",
        None,
        "D:/work",
        "Spec slash session",
        None,
    );
    let app = build_router(state);
    let created = execute_slash_command_for_session(
        app.clone(),
        "/spec new Build a reliable spec command surface",
        "spec-slash-session",
    )
    .await;
    assert_eq!(created["success"], true);
    let output = created["output"].as_str().unwrap();
    assert!(output.contains("Spec created"), "{output}");
    assert!(output.contains("D:/work"), "{output}");
    let spec_id = output
        .split('`')
        .nth(1)
        .expect("created spec id should be wrapped in backticks")
        .to_string();

    let status = execute_slash_command_for_session(
        app.clone(),
        &format!("/spec status {spec_id}"),
        "spec-slash-session",
    )
    .await;
    assert_eq!(status["success"], true);
    assert!(status["output"]
        .as_str()
        .unwrap()
        .contains("Latest analysis: none"));

    let analysis = execute_slash_command_for_session(
        app.clone(),
        &format!("/spec analyze {spec_id}"),
        "spec-slash-session",
    )
    .await;
    assert_eq!(analysis["success"], true);
    assert!(analysis["output"]
        .as_str()
        .unwrap()
        .contains("Spec analysis"));

    let clarify = execute_slash_command_for_session(
        app.clone(),
        &format!("/spec clarify {spec_id}"),
        "spec-slash-session",
    )
    .await;
    assert_eq!(clarify["success"], true);
    assert!(clarify["output"].as_str().unwrap().contains("Spec clarify"));

    let plan = execute_slash_command_for_session(
        app.clone(),
        &format!("/spec plan {spec_id} --constraints terminal-friendly"),
        "spec-slash-session",
    )
    .await;
    assert_eq!(plan["success"], true);
    let output = plan["output"].as_str().unwrap();
    assert!(output.contains("Spec plan"), "{output}");
    assert!(output.contains("Created tasks"), "{output}");

    let checklist = execute_slash_command_for_session(
        app.clone(),
        &format!("/spec checklist {spec_id} --focus readiness"),
        "spec-slash-session",
    )
    .await;
    assert_eq!(checklist["success"], true);
    let output = checklist["output"].as_str().unwrap();
    assert!(output.contains("Spec checklist"), "{output}");
    assert!(output.contains("Ready:"), "{output}");

    let implement = execute_slash_command_for_session(
        app.clone(),
        &format!("/spec implement {spec_id} --dry-run"),
        "spec-slash-session",
    )
    .await;
    assert_eq!(implement["success"], true);
    let output = implement["output"].as_str().unwrap();
    assert!(output.contains("Spec implement dry-run"), "{output}");
    assert!(output.contains("Prompt bytes"), "{output}");

    let trace = execute_slash_command_for_session(
        app,
        &format!("/spec trace {spec_id}"),
        "spec-slash-session",
    )
    .await;
    assert_eq!(trace["success"], true);
    assert!(trace["output"].as_str().unwrap().contains("Spec trace"));
}

#[tokio::test]
async fn slash_benchmarks_and_terminal_bench_tasks_report_real_state() {
    let store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
    store
        .record_benchmark(
            "bench-fixture",
            "coder",
            "model-fixture",
            rightkit_agent_orchestration::coding::TbenchCatalog::suite_id(),
            2,
            1,
            "2026-06-06T00:00:00Z",
        )
        .unwrap();
    let storage = Arc::new(tokio::sync::Mutex::new(store));
    let app = build_router(DaemonState::new("").with_storage(storage));

    let benchmarks = execute_slash_command(app.clone(), "/benchmarks").await;
    assert_eq!(benchmarks["success"], true);
    let output = benchmarks["output"].as_str().unwrap();
    assert!(output.contains("coder on model-fixture"), "{output}");
    assert!(output.contains("1/2"), "{output}");
    assert!(output.contains("Proof: 1/2 successful repeats"), "{output}");

    let tasks = execute_slash_command(app, "/terminal-bench tasks").await;
    assert_eq!(tasks["success"], true);
    let output = tasks["output"].as_str().unwrap();
    assert!(output.contains("RightKit Internal Tasks"), "{output}");
    assert!(output.contains("tb-001"), "{output}");
    assert!(output.contains(rightkit_agent_orchestration::coding::TbenchCatalog::suite_id()));
}
