#[tokio::test]
async fn instructions_route_reports_sources_without_instruction_bodies() {
    let root = unique_temp_dir("instruction-status");
    std::fs::create_dir_all(root.join(".coderight")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "Agent project rules").unwrap();
    std::fs::write(
        root.join(".coderight").join("right.md"),
        "---\nglobs:\n  - src/**/*.rs\n---\nSecret local instruction body",
    )
    .unwrap();
    let app = build_router(DaemonState::new(""));
    let uri = format!(
        "/instructions?workspaceRoot={}",
        encode_query_value(&root.to_string_lossy())
    );

    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!raw.contains("Secret local instruction body"));
    let body: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(body["workspaceRoot"], root.to_string_lossy().to_string());
    assert_eq!(body["fileCount"], 2);
    assert!(body["composedBytes"].as_u64().unwrap() > 0);
    assert!(body["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"].as_str().unwrap().ends_with("AGENTS.md")
            && file["scope"] == "project"));
    assert!(body["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"].as_str().unwrap().ends_with("right.md")
            && file["scope"] == "local"
            && file["globs"][0] == "src/**/*.rs"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn instructions_slash_command_reports_sources_without_instruction_bodies() {
    let root = unique_temp_dir("instruction-slash");
    std::fs::create_dir_all(root.join(".claude").join("rules")).unwrap();
    std::fs::write(root.join("CLAUDE.md"), "Claude project rules").unwrap();
    std::fs::write(
        root.join(".claude").join("rules").join("rust.md"),
        "---\npaths:\n  - src/lib.rs\n---\nSensitive rust rules",
    )
    .unwrap();
    // `.claude/rules` is trust-gated (#131): mark this temp workspace trusted
    // in a temp trust store selected via the env override (never ~/.coderight).
    let _env_guard = crate::test_support::process_env_lock();
    let trust_store = root.join("workspace-trust.json");
    std::fs::write(
        &trust_store,
        serde_json::json!({"projects": {
            rightkit_app_settings::agent_config::workspace_trust_key(&root.to_string_lossy()): {
                "trust": "trusted", "approvedBy": "user", "updatedAt": "t"
            }
        }})
        .to_string(),
    )
    .unwrap();
    let prev_trust_store = std::env::var_os("RIGHTKIT_WORKSPACE_TRUST_STORE");
    std::env::set_var("RIGHTKIT_WORKSPACE_TRUST_STORE", &trust_store);
    let app = build_router(DaemonState::new(""));
    let input = format!("/instructions {}", root.to_string_lossy());

    let result = execute_slash_command(app, &input).await;
    match prev_trust_store {
        Some(prev) => std::env::set_var("RIGHTKIT_WORKSPACE_TRUST_STORE", prev),
        None => std::env::remove_var("RIGHTKIT_WORKSPACE_TRUST_STORE"),
    }

    let output = result["output"].as_str().unwrap();
    assert_eq!(result["success"], true);
    assert!(output.contains("Instruction sources"));
    assert!(output.contains("CLAUDE.md"));
    assert!(output.contains("rust.md"));
    assert!(output.contains("path:src/lib.rs"));
    assert!(!output.contains("Sensitive rust rules"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn schedules_route_lists_workspace_schedules_with_run_metadata() {
    let root = unique_temp_dir("schedules-list");
    write_schedule_fixture(&root);
    let app = build_router(DaemonState::new(""));
    let uri = format!(
        "/schedules?workspaceRoot={}",
        encode_query_value(&root.to_string_lossy())
    );

    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["workspaceRoot"], root.to_string_lossy().to_string());
    assert_eq!(v["schedules"][0]["id"], "daily-review");
    assert_eq!(v["schedules"][0]["cronExpr"], "0 9 * * *");
    assert_eq!(v["schedules"][0]["prompt"], "review pending PRs");
    assert_eq!(v["schedules"][0]["timeZone"], "UTC");
    assert_eq!(v["schedules"][0]["mode"], "agent");
    assert_eq!(v["schedules"][0]["paused"], false);
    assert!(v["schedules"][0]["skills"].as_array().unwrap().is_empty());
    assert!(v["schedules"][0]["nextDueEpochMinute"].is_number());
    assert!(v["schedules"][0]["nextDueError"].is_null());
    assert_eq!(v["schedules"][0]["lastClaimedMinute"], 12345);
    assert_eq!(v["schedules"][0]["latestRun"]["status"], "submitted");
    assert_eq!(v["schedules"][0]["latestRun"]["epochMinute"], 12345);
    // This is a persisted legacy fixture, not a newly-created live session.
    // Preserve its exact receipt ID while live-run tests below require UUIDv7.
    assert_eq!(
        v["schedules"][0]["latestRun"]["sessionId"],
        "schedule-daily-review"
    );
    assert_eq!(v["schedules"][0]["runs"].as_array().unwrap().len(), 2);
    assert_eq!(v["schedules"][0]["runs"][0]["status"], "submitted");
    assert_eq!(v["schedules"][0]["runs"][1]["status"], "claimed");
    assert!(
        v["scheduleStorePath"]
            .as_str()
            .unwrap()
            .ends_with(".coderight\\schedules.json")
            || v["scheduleStorePath"]
                .as_str()
                .unwrap()
                .ends_with(".coderight/schedules.json")
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn schedule_pause_resume_and_run_now_routes_control_workspace_schedule() {
    let root = unique_temp_dir("schedules-controls");
    write_schedule_fixture(&root);
    let state = DaemonState::new("");
    let app = build_router(state.clone());
    let root_query = encode_query_value(&root.to_string_lossy());

    let pause = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/schedules/daily-review/pause?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pause.status(), StatusCode::OK);

    let schedules_path = root.join(".coderight").join("schedules.json");
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    assert_eq!(persisted["daily-review"]["paused"], true);
    assert!(
        crate::scheduler::claim_due_schedules(&root, 42)
            .unwrap()
            .is_empty(),
        "paused schedule must not be claimed"
    );

    let resume = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/schedules/daily-review/resume?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resume.status(), StatusCode::OK);

    let run = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/schedules/daily-review/run?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let body = run.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["submitted"], true);
    assert_eq!(v["latestRun"]["status"], "submitted");
    let session_id = v["latestRun"]["sessionId"].as_str().expect("session id");
    assert!(session_id.starts_with("schedule-daily-review-"));

    let live = state.get_session(session_id).expect("run-now session");
    let transcript = lock_recover(&live.transcript, "session.transcript");
    assert!(transcript.iter().any(|message| matches!(
        message,
        TranscriptMessage::User { content, .. }
            if content.contains("review pending PRs") && content.contains("Mode: agent")
    )));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_schedule_now_binds_configured_toolset_to_session() {
    let root = unique_temp_dir("schedules-run-toolset");
    write_schedule_fixture(&root);
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["toolset"] = serde_json::json!("review");
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let state = DaemonState::new("");
    let app = build_router(state.clone());
    let root_query = encode_query_value(&root.to_string_lossy());

    let run = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/schedules/daily-review/run?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let body = run.into_body().collect().await.unwrap().to_bytes();
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let session_id = response["latestRun"]["sessionId"]
        .as_str()
        .expect("session id");
    assert!(session_id.starts_with("schedule-daily-review-"));

    let live = state.get_session(session_id).expect("run-now session");
    assert_eq!(live.toolset.as_deref(), Some("review"));
    assert!(live.allowed_tools.iter().any(|tool| tool == "git_diff"));
    assert!(live
        .disallowed_tools
        .iter()
        .any(|tool| tool == "write_file"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn run_schedule_now_records_active_skills_on_created_session() {
    let root = unique_temp_dir("schedules-run-active-skills");
    write_schedule_fixture(&root);
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["skills"] = serde_json::json!(["reviewer"]);
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    let app = build_router(state);
    let root_query = encode_query_value(&root.to_string_lossy());

    let run = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/schedules/daily-review/run?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let body = run.into_body().collect().await.unwrap().to_bytes();
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let session_id = response["latestRun"]["sessionId"]
        .as_str()
        .expect("session id");
    assert!(session_id.starts_with("schedule-daily-review-"));

    let row = wait_for_session(&storage, session_id).await;
    let config: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
    assert_eq!(config["activeSkills"], serde_json::json!(["reviewer"]));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn run_schedule_now_binds_execution_backend_to_created_session() {
    let root = unique_temp_dir("schedules-run-execution-backend");
    write_schedule_fixture(&root);
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["execution_backend"] = serde_json::json!("local");
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    let app = build_router(state);
    let root_query = encode_query_value(&root.to_string_lossy());

    let run = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/schedules/daily-review/run?workspaceRoot={root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let body = run.into_body().collect().await.unwrap().to_bytes();
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let session_id = response["latestRun"]["sessionId"]
        .as_str()
        .expect("session id");
    assert!(session_id.starts_with("schedule-daily-review-"));

    let row = wait_for_session(&storage, session_id).await;
    let config: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
    assert_eq!(config["executionBackend"], "local");

    let _ = std::fs::remove_dir_all(root);
}
