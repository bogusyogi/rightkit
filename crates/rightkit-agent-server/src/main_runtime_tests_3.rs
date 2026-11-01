#[test]
fn render_review_findings_markdown_reports_empty_state() {
    let body = serde_json::json!({
        "sessionId": "sess-review",
        "findings": [],
    });
    assert!(render_review_findings_markdown(&body).contains("No auditor findings found"));
}

#[test]
fn conduct_session_create_body_threads_safe_mode() {
    let body = conduct_create_session_body("conduct-1", "D:/repo", true, None, false);
    assert_eq!(body["id"], "conduct-1");
    assert_eq!(body["workspaceRoot"], "D:/repo");
    assert_eq!(body["safeMode"], true);
    assert!(body["model"].is_null());
    assert!(body["permissionMode"].is_null());
}

#[test]
fn conduct_session_create_body_threads_model_and_auto_approve() {
    let body = conduct_create_session_body(
        "conduct-2",
        "D:/repo",
        false,
        Some("moonshotai/kimi-k2.6"),
        true,
    );
    assert_eq!(body["model"], "moonshotai/kimi-k2.6");
    assert_eq!(body["permissionMode"], "accept-edits");
}

#[test]
fn review_rejected_options_names_each_unsupported_flag() {
    assert!(review_rejected_options(None, false, None, None).is_empty());
    assert_eq!(
        review_rejected_options(Some("openrouter"), true, Some(5), Some(1.0)),
        vec![
            "--provider",
            "--stream-json",
            "--max-turns",
            "--max-budget-usd"
        ]
    );
    assert_eq!(
        review_rejected_options(None, false, None, Some(0.5)),
        vec!["--max-budget-usd"]
    );
}

#[test]
fn caps_markdown_reports_real_tools() {
    let capabilities = rightkit_agent_tools::coding_tools::default_tool_capabilities();
    let output = rightkit_agent_tools::coding_tools::render_tool_capabilities_markdown(&capabilities);

    assert!(output.contains("RightKit capabilities"));
    assert!(output.contains("read-only"));
    assert!(output.contains("mutating"));
    assert!(output.contains("### files"));
    assert!(output.contains("`read_file`"));
    assert!(output.contains("`shell`"));
}

#[test]
fn url_encode_component_encodes_query_values() {
    assert_eq!(url_encode_component("billing bug"), "billing%20bug");
    assert_eq!(url_encode_component("tag:a/b"), "tag%3Aa%2Fb");
    assert_eq!(url_encode_component("safe-_.~"), "safe-_.~");
}

#[test]
fn learning_cli_commands_map_to_runtime_routes() {
    assert_eq!(
        learning_command_request(LearningCommand::Approve {
            proposal_id: "proposal/with space".to_string()
        }),
        (
            "/runtime/learning/proposal%2Fwith%20space/approve".to_string(),
            "learning approve"
        )
    );
    assert_eq!(
        learning_command_request(LearningCommand::Reject {
            proposal_id: "proposal-2".to_string()
        }),
        (
            "/runtime/learning/proposal-2/reject".to_string(),
            "learning reject"
        )
    );
}

#[test]
fn mcp_management_query_encodes_scope_and_workspace_root() {
    assert_eq!(
        mcp_management_query(Some("workspace"), Some("D:/Claude/coderight")),
        "?scope=workspace&workspaceRoot=D%3A%2FClaude%2Fcoderight"
    );
    assert_eq!(mcp_management_query(None, None), "");
}

#[test]
fn workspace_root_query_encodes_workspace_root() {
    assert_eq!(
        workspace_root_query(Some("D:/Claude/coderight")),
        "?workspaceRoot=D%3A%2FClaude%2Fcoderight"
    );
}

#[test]
fn filter_agent_session_rows_hides_archived_unless_all() {
    let rows = vec![
        serde_json::json!({"id": "run-1", "status": "running", "workspaceRoot": "D:/ws"}),
        serde_json::json!({"id": "old-1", "status": "archived", "workspaceRoot": "D:/ws"}),
    ];
    let visible = filter_agent_session_rows(rows.clone(), false, false);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0]["id"], "run-1");

    let all = filter_agent_session_rows(rows, true, false);
    assert_eq!(all.len(), 2);
}

#[test]
fn select_last_resume_session_filters_workspace_and_archived() {
    let cwd = std::env::current_dir().unwrap().display().to_string();
    let rows = vec![
        serde_json::json!({
            "id": "current-old",
            "status": "done",
            "workspaceRoot": cwd,
            "updatedAt": "2026-06-23T10:00:00Z"
        }),
        serde_json::json!({
            "id": "current-new",
            "status": "dormant",
            "workspaceRoot": cwd,
            "updatedAt": "2026-06-23T11:00:00Z"
        }),
        serde_json::json!({
            "id": "archived-newer",
            "status": "archived",
            "workspaceRoot": cwd,
            "updatedAt": "2026-06-23T12:00:00Z"
        }),
        serde_json::json!({
            "id": "other-newest",
            "status": "dormant",
            "workspaceRoot": "D:/other",
            "updatedAt": "2026-06-23T13:00:00Z"
        }),
    ];

    assert_eq!(
        select_last_resume_session_id(&rows, false, Some(&cwd)).as_deref(),
        Some("current-new")
    );
    assert_eq!(
        select_last_resume_session_id(&rows, true, Some(&cwd)).as_deref(),
        Some("archived-newer")
    );
    assert_eq!(
        select_last_resume_session_id(&rows, false, None).as_deref(),
        Some("other-newest")
    );
}

#[test]
fn select_last_resume_session_falls_back_to_created_at_and_skips_empty_ids() {
    let rows = vec![
        serde_json::json!({
            "id": "created-newest",
            "status": "dormant",
            "createdAt": "2026-06-23T15:00:00Z",
            "updatedAt": ""
        }),
        serde_json::json!({
            "id": "updated-older",
            "status": "dormant",
            "createdAt": "2026-06-23T10:00:00Z",
            "updatedAt": "2026-06-23T14:00:00Z"
        }),
        serde_json::json!({
            "id": "",
            "status": "dormant",
            "createdAt": "2026-06-23T16:00:00Z",
            "updatedAt": "2026-06-23T16:00:00Z"
        }),
    ];

    assert_eq!(
        select_last_resume_session_id(&rows, false, None).as_deref(),
        Some("created-newest")
    );
}

#[test]
fn cli_acquisition_gate_detects_only_new_wrapper_names() {
    let args = vec![
        "coderight".to_string(),
        "mcp".to_string(),
        "list".to_string(),
    ];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("mcp"));
    let args = vec!["coderight".to_string(), "completion".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("completion"));
    let args = vec!["coderight".to_string(), "schedule".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("schedule"));
    let args = vec!["coderight".to_string(), "fork".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("fork"));
    let args = vec!["coderight".to_string(), "permission".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("permission"));
    let args = vec!["coderight".to_string(), "provider".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("provider"));
    let args = vec!["coderight".to_string(), "local-model".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("local-model"));
    let args = vec!["coderight".to_string(), "local-models".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("local-models"));
    let args = vec!["coderight".to_string(), "hooks".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("hooks"));
    let args = vec!["coderight".to_string(), "hook".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("hook"));
    let args = vec!["coderight".to_string(), "toolset".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("toolset"));
    let args = vec!["coderight".to_string(), "toolsets".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("toolsets"));
    let args = vec!["coderight".to_string(), "execution".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), Some("execution"));
    let args = vec!["coderight".to_string(), "mcp-server".to_string()];
    assert_eq!(cli_acquisition_wrapper_command(&args), None);
}

#[test]
fn completion_scripts_cover_every_supported_shell_from_canonical_cli_grammar() {
    for shell in [
        CompletionShell::Bash,
        CompletionShell::Zsh,
        CompletionShell::Fish,
        CompletionShell::Powershell,
        CompletionShell::Elvish,
    ] {
        let output = generate_completion(shell);
        let script = String::from_utf8(output).expect("completion script must be UTF-8");
        assert!(
            script.contains("coderight"),
            "missing binary name for {shell:?}"
        );
        for canonical_command in ["onboard", "serve", "completion", "acp-server"] {
            assert!(
                script.contains(canonical_command),
                "missing canonical command {canonical_command} for {shell:?}"
            );
        }
    }
}

#[test]
fn cli_acquisition_v1_defaults_enabled() {
    let cfg = rightkit_app_settings::agent_config::HostConfig::default();
    assert!(cli_acquisition_v1_enabled_from_config(&cfg));

    let mut cfg = rightkit_app_settings::agent_config::HostConfig::default();
    cfg.features.cli_acquisition_v1 = Some(false);
    assert!(!cli_acquisition_v1_enabled_from_config(&cfg));
}

#[tokio::test]
async fn daemon_status_client_waits_for_bounded_session_projection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/sessions", listener.local_addr().unwrap());
    let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route(
                "/sessions",
                axum::routing::get(|| async {
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                    axum::Json(serde_json::json!([{"status":"running","turnActive":true}]))
                }),
            ),
        )
        .await
        .unwrap();
    }));
    let projection: serde_json::Value =
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            daemon_status_client(&url)
                .unwrap()
                .get_json::<serde_json::Value>(&url)
                .await
                .unwrap()
        })
        .await
        .expect("bounded delayed session projection must remain available");
    assert_eq!(
        projection[0]["turnActive"], true,
        "slow verification must retain active-turn refusal information"
    );
    drop(server);
}

#[test]
fn status_unreachable_json_reports_probe_failure() {
    let value = status_unreachable_json("http://127.0.0.1:9", "connection refused");
    assert_eq!(value["running"], false);
    assert_eq!(value["status"], "unreachable");
    assert_eq!(value["daemonUrl"], "http://127.0.0.1:9");
    assert!(value["error"].as_str().unwrap().contains("connection"));
}

#[test]
fn status_degraded_json_reports_reachable_http_failure() {
    let value = status_degraded_json("http://127.0.0.1:8080", "health returned 503");
    assert_eq!(value["running"], true);
    assert_eq!(value["status"], "degraded");
    assert_eq!(value["daemonUrl"], "http://127.0.0.1:8080");
    assert!(value["error"].as_str().unwrap().contains("503"));
}

#[test]
fn machine_status_derives_health_truthfully() {
    assert_eq!(
        daemon_machine_status_from_health(&serde_json::json!({"ok": true})),
        DaemonMachineStatus::Healthy
    );
    assert_eq!(
        daemon_machine_status_from_health(&serde_json::json!({"ok": false})),
        DaemonMachineStatus::Degraded
    );
    assert_eq!(
        daemon_machine_status_from_health(&serde_json::json!({"ready": true})),
        DaemonMachineStatus::Degraded
    );
}

#[test]
fn stopped_status_is_distinct_from_unreachable() {
    let value = status_stopped_json();
    assert_eq!(value["running"], false);
    assert_eq!(value["status"], "stopped");
    assert!(value.get("daemonUrl").is_none());
    assert_ne!(
        value["status"],
        status_unreachable_json("http://127.0.0.1:9", "down")["status"]
    );
}

#[test]
fn startup_info_for_serve_is_wellformed() {
    let info = LoopbackRecord::for_this_process(
        1234,
        &generate_token(),
        &RecordLabels {
            database_path: default_db_path().to_string(),
            protocol_version: 1,
            schema_id: "coderight.daemon.v1".to_string(),
            runtime: "rust".to_string(),
            generation: 1,
        },
    );
    let parsed = LoopbackRecord::parse(&info.to_line()).unwrap();
    assert_eq!(parsed.port, 1234);
    assert!(!parsed.token.is_empty());
}

#[test]
fn resolve_daemon_from_env() {
    let result = resolve_daemon_from_sources(
        Some("http://127.0.0.1:9999".into()),
        Some("test-token".into()),
        None,
    );
    let (url, tok) = result.unwrap();
    assert_eq!(url, "http://127.0.0.1:9999");
    assert_eq!(tok, "test-token");
}

#[test]
fn resolve_daemon_missing_returns_none_when_no_port_file() {
    assert!(resolve_daemon_from_sources(None, None, None).is_none());
}

#[test]
fn write_and_read_daemon_port_file() {
    let tmp = std::env::temp_dir().join("coderight_test_home");
    let _ = std::fs::create_dir_all(&tmp);
    let dir = tmp.join(".coderight");
    let _ = std::fs::create_dir_all(&dir);

    let info = LoopbackRecord::for_this_process(
        7777,
        "tok-abc",
        &RecordLabels {
            database_path: "/tmp/test.db".to_string(),
            protocol_version: 1,
            ..RecordLabels::default()
        },
    );
    let path = dir.join("daemon.port");
    std::fs::write(&path, info.to_line()).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed = LoopbackRecord::parse(&content).unwrap();
    assert_eq!(parsed.port, 7777);
    assert_eq!(parsed.token, "tok-abc");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn cli_instruction_context_reads_workspace_and_extra_roots() {
    let root = std::env::temp_dir().join(format!(
        "coderight_cli_instruction_context_{}",
        std::process::id()
    ));
    let extra = std::env::temp_dir().join(format!(
        "coderight_cli_instruction_extra_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&extra);
    std::fs::create_dir_all(root.join(".coderight")).unwrap();
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(root.join("AGENTS.md"), "Workspace agent rules").unwrap();
    std::fs::write(
        root.join(".coderight").join("right.md"),
        "Local RightKit rules",
    )
    .unwrap();
    std::fs::write(extra.join("CLAUDE.md"), "Extra instruction rules").unwrap();

    let context = load_cli_instruction_context(&root, &[extra.to_string_lossy().to_string()])
        .expect("expected instruction context");

    assert!(context.contains("Workspace agent rules"));
    assert!(context.contains("Local RightKit rules"));
    assert!(context.contains("Extra instruction rules"));

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&extra);
}

#[test]
fn cli_system_prompt_combines_instructions_and_override() {
    let prompt = compose_cli_system_prompt(
        Some("Project instructions"),
        Some("Be concise."),
        Some("Append this."),
    )
    .expect("expected system prompt");

    assert!(prompt.contains("Project instructions"));
    assert!(prompt.contains("Be concise."));
    assert!(prompt.contains("Append this."));
    assert!(prompt.find("Project instructions").unwrap() < prompt.find("Be concise.").unwrap());
    assert!(prompt.find("Be concise.").unwrap() < prompt.find("Append this.").unwrap());
}

#[test]
fn cli_prompt_text_rejects_inline_and_file_conflict() {
    let err = resolve_prompt_text(
        "--system-prompt",
        Some("inline".into()),
        "--system-prompt-file",
        Some("system.md".into()),
    )
    .unwrap_err();

    assert!(err.contains("Cannot use both --system-prompt and --system-prompt-file"));
}

#[test]
fn session_deeplink_url_percent_encodes_session_id() {
    assert_eq!(
        session_deeplink_url("sess-1"),
        "coderight://sessions/sess-1"
    );
    assert_eq!(
        session_deeplink_url("review child/1"),
        "coderight://sessions/review%20child%2F1"
    );
}
