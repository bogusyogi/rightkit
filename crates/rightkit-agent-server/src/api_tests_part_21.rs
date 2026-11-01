#[tokio::test]
async fn scheduled_sessions_require_schedule_admin_toolset_for_schedule_management() {
    let root = unique_temp_dir("schedules-admin-policy");
    write_schedule_fixture(&root);
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["toolset"] = serde_json::json!("full");
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
    assert_eq!(live.toolset.as_deref(), Some("full"));
    assert!(live
        .disallowed_tools
        .iter()
        .any(|tool| tool == "schedule_cron"));

    let admin_root = unique_temp_dir("schedules-admin-policy-allowed");
    write_schedule_fixture(&admin_root);
    let admin_schedules_path = admin_root.join(".coderight").join("schedules.json");
    let mut admin_schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&admin_schedules_path).unwrap()).unwrap();
    admin_schedules["daily-review"]["toolset"] = serde_json::json!("schedule-admin");
    std::fs::write(
        &admin_schedules_path,
        serde_json::to_string_pretty(&admin_schedules).unwrap(),
    )
    .unwrap();
    let admin_state = DaemonState::new("");
    let admin_app = build_router(admin_state.clone());
    let admin_root_query = encode_query_value(&admin_root.to_string_lossy());

    let admin_run = admin_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/schedules/daily-review/run?workspaceRoot={admin_root_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(admin_run.status(), StatusCode::OK);
    let body = admin_run.into_body().collect().await.unwrap().to_bytes();
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let session_id = response["latestRun"]["sessionId"]
        .as_str()
        .expect("session id");
    assert!(session_id.starts_with("schedule-daily-review-"));

    let admin_live = admin_state
        .get_session(session_id)
        .expect("schedule-admin run-now session");
    assert_eq!(admin_live.toolset.as_deref(), Some("schedule-admin"));
    assert!(admin_live
        .allowed_tools
        .iter()
        .any(|tool| tool == "schedule_cron"));
    assert!(!admin_live
        .disallowed_tools
        .iter()
        .any(|tool| tool == "schedule_cron"));

    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(admin_root);
}

#[cfg(feature = "qualification")]
async fn eff001_scheduled_provider(
    State(captures): State<
        tokio::sync::mpsc::Sender<(serde_json::Value, tokio::sync::oneshot::Sender<String>)>,
    >,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    let (release, waiting) = tokio::sync::oneshot::channel();
    captures.send((body, release)).await.unwrap();
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(futures_util::stream::once(async move {
            Ok::<_, std::io::Error>(axum::body::Bytes::from(waiting.await.unwrap()))
        })))
        .unwrap()
}

#[cfg(feature = "qualification")]
#[tokio::test]
async fn eff001_scheduled_run_now_reaches_shared_admission_trace() {
    rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
        rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
    )
    .expect("qualification fixture must install process authority");
    let root = unique_temp_dir("eff001-scheduled");
    write_schedule_fixture(&root);
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["toolset"] = serde_json::json!("full");
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let (captures, mut captured) = tokio::sync::mpsc::channel(2);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new()
                .route("/v1/chat/completions", post(eff001_scheduled_provider))
                .with_state(captures),
        )
        .await
        .unwrap();
    });
    let storage = crate::api::SharedStorage::new(
        rightkit_agent_engine_storage::Storage::open(&root.join("eff001-scheduled.db")).unwrap(),
    );
    let state = DaemonState::new("")
        .with_storage(storage)
        .with_engine(crate::api::EngineConfig {
            api_key: "test-key".into(),
            model_id: "moonshotai/kimi-k2.6".into(),
            base_url: url,
            fallback_models: vec![],
        });
    let app = build_router(state.clone());
    let query = encode_query_value(&root.to_string_lossy());
    let run = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/schedules/daily-review/run?workspaceRoot={query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let body = run.into_body().collect().await.unwrap().to_bytes();
    let run_response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let session_id = run_response["latestRun"]["sessionId"]
        .as_str()
        .expect("scheduled run response must identify session")
        .to_owned();
    assert!(
        session_id.starts_with("schedule-daily-review-"),
        "unexpected session: {session_id}"
    );
    assert!(
        state.get_session(&session_id).is_some(),
        "scheduled session must be live"
    );
    assert!(rightkit_agent_engine_core::qualification_trace::snapshot()
        .stages_for("eff001-scheduled-9f7e")
        .is_empty());

    let (provider_request, release) =
        tokio::time::timeout(std::time::Duration::from_secs(5), captured.recv())
            .await
            .unwrap()
            .expect("scheduled run must reach provider");
    assert_eq!(
        provider_request["model"], "moonshotai/kimi-k2.6",
        "scheduled provider request must use accepted model: {provider_request:#}"
    );
    release.send(format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"eff001-scheduled-9f7e","type":"function","function":{"name":"get_time","arguments":serde_json::json!({"operation":"current_time","timezone":"UTC"}).to_string()}}]},"finish_reason":null}]}),
        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
    )).unwrap();
    let (_, release_terminal) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            tokio::select! {
                capture = captured.recv() => break capture.expect("scheduled tool round provider capture"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                    for live in state.list_sessions() {
                        let pending = crate::api::lock_recover(&live.pending_approvals, "test.approvals")
                            .keys().cloned().collect::<Vec<_>>();
                        for tool_call_id in pending {
                            assert_eq!(live.id, session_id);
                            assert_eq!(tool_call_id, "eff001-scheduled-9f7e");
                            let approved = app.clone().oneshot(Request::builder().method("POST")
                                .uri(format!("/sessions/{}/tool-approval", live.id))
                                .header("content-type", "application/json")
                                .body(Body::from(serde_json::json!({"toolCallId":tool_call_id,"approved":true}).to_string())).unwrap()).await.unwrap();
                            assert_eq!(approved.status(), StatusCode::OK);
                        }
                    }
                }
            }
        }
    }).await.unwrap();
    release_terminal.send(format!("data: {}\n\ndata: [DONE]\n\n", serde_json::json!({"choices":[{"index":0,"delta":{"content":"scheduled-ok"},"finish_reason":"stop"}]}))).unwrap();

    let live = state.get_session(&session_id).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while live.live_turn_handle.is_active() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !live.live_turn_handle.is_active(),
        "scheduled session did not settle: {session_id}"
    );
    let trace = rightkit_agent_engine_core::qualification_trace::snapshot();
    let transcript =
        crate::api::lock_recover(&live.transcript, "scheduled qualification transcript")
            .iter()
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>();
    let pending =
        crate::api::lock_recover(&live.pending_approvals, "scheduled qualification approvals")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
    let persistence = live.persistence_state.snapshot();
    assert_eq!(trace.stages_for("eff001-scheduled-9f7e"),
        rightkit_agent_engine_core::qualification_trace::EffectAdmissionStage::canonical_order(),
        "scheduled admission diagnostics: transcript={transcript:?}, pending={pending:?}, persistence={persistence:?}");
    assert!(trace.is_canonically_ordered("eff001-scheduled-9f7e"));
    assert!(trace.reached_execute("eff001-scheduled-9f7e"));
    provider.abort();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_schedule_now_executes_script_only_allowlisted_command_as_artifact() {
    let root = unique_temp_dir("schedules-script-only");
    write_schedule_fixture(&root);
    let command = if cfg!(windows) {
        "cmd /C echo scheduled-script-ok"
    } else {
        "printf scheduled-script-ok"
    };
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["mode"] = serde_json::json!("scriptOnly");
    schedules["daily-review"]["prompt"] = serde_json::json!(command);
    schedules["daily-review"]["script_allowlist"] = serde_json::json!([command]);
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
    assert!(lock_recover(&state.sessions, "sessions")
        .keys()
        .all(|id| !id.starts_with("schedule-daily-review-")));

    let body = run.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["latestRun"]["status"], "scriptCompleted");
    assert_eq!(json["latestRun"]["exitCode"], 0);
    let artifact_path = std::path::PathBuf::from(
        json["latestRun"]["artifactPath"]
            .as_str()
            .expect("artifact path"),
    );
    let artifact = std::fs::read_to_string(&artifact_path).unwrap();
    assert!(artifact.contains("scheduled-script-ok"), "{artifact}");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_schedule_now_times_out_script_only_command() {
    let root = unique_temp_dir("schedules-script-only-timeout");
    write_schedule_fixture(&root);
    let command = if cfg!(windows) {
        "for /L %i in (1,1,100000000) do @rem & echo late-script-output"
    } else {
        "sleep 5; echo late-script-output"
    };
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["mode"] = serde_json::json!("scriptOnly");
    schedules["daily-review"]["prompt"] = serde_json::json!(command);
    schedules["daily-review"]["script_allowlist"] = serde_json::json!([command]);
    schedules["daily-review"]["max_runtime_seconds"] = serde_json::json!(1);
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
    assert!(lock_recover(&state.sessions, "sessions")
        .keys()
        .all(|id| !id.starts_with("schedule-daily-review-")));

    let body = run.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let latest = &json["latestRun"];
    assert_eq!(latest["status"], "failed");
    assert_eq!(latest["artifactPath"], serde_json::Value::Null);
    assert_eq!(latest["exitCode"], serde_json::Value::Null);
    assert!(latest["message"]
        .as_str()
        .unwrap_or_default()
        .contains("max_runtime_seconds"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_schedule_now_rejects_nonlocal_script_backend() {
    let root = unique_temp_dir("schedules-script-only-nonlocal-backend");
    write_schedule_fixture(&root);
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["mode"] = serde_json::json!("scriptOnly");
    schedules["daily-review"]["prompt"] = serde_json::json!("echo should-not-run");
    schedules["daily-review"]["script_allowlist"] = serde_json::json!(["echo should-not-run"]);
    schedules["daily-review"]["execution_backend"] = serde_json::json!("missing-backend");
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let app = build_router(DaemonState::new(""));
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
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["latestRun"]["status"], "failed");
    assert!(
        json["latestRun"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("supported backends: local"),
        "{json:#}"
    );
    assert_eq!(json["latestRun"]["executionBackend"], "missing-backend");
    assert_eq!(
        json["latestRun"]["resolvedExecutionBackend"],
        serde_json::Value::Null
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn script_only_schedule_rejects_remote_backend() {
    let mut entry = crate::scheduler::ScheduleEntry {
        id: "nonlocal-script".to_string(),
        cron_expr: "* * * * *".to_string(),
        prompt: "echo nonlocal-script".to_string(),
        time_zone: "UTC".to_string(),
        skills: Vec::new(),
        workdir: None,
        toolset: None,
        execution_backend: Some("missing-backend".to_string()),
        delivery: Vec::new(),
        script_allowlist: vec!["echo nonlocal-script".to_string()],
        mode: "scriptOnly".to_string(),
        paused: false,
        source_recipe_id: None,
        recipe_params: Default::default(),
        max_runtime_seconds: None,
    };
    let error = crate::scheduler::script_only_execution_backend(&entry).unwrap_err();
    assert!(error.contains("supported backends: local"));

    entry.execution_backend = Some("another-backend".to_string());
    let error = crate::scheduler::script_only_execution_backend(&entry).unwrap_err();
    assert!(error.contains("supported backends: local"));
}

#[tokio::test]
async fn schedule_delivery_failure_does_not_discard_script_result() {
    let root = unique_temp_dir("schedules-delivery-result");
    write_schedule_fixture(&root);
    let command = if cfg!(windows) {
        "cmd /C echo delivery-result-ok"
    } else {
        "printf delivery-result-ok"
    };
    let delivered_path = root.join("delivered-script-output.txt");
    let schedules_path = root.join(".coderight").join("schedules.json");
    let mut schedules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schedules_path).unwrap()).unwrap();
    schedules["daily-review"]["mode"] = serde_json::json!("scriptOnly");
    schedules["daily-review"]["prompt"] = serde_json::json!(command);
    schedules["daily-review"]["script_allowlist"] = serde_json::json!([command]);
    schedules["daily-review"]["delivery"] = serde_json::json!([
        { "kind": "file_artifact", "target": delivered_path.to_string_lossy() },
        { "kind": "webhook", "target": "http://127.0.0.1:9/schedule" }
    ]);
    std::fs::write(
        &schedules_path,
        serde_json::to_string_pretty(&schedules).unwrap(),
    )
    .unwrap();

    let app = build_router(DaemonState::new(""));
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
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let latest = &json["latestRun"];
    assert_eq!(latest["status"], "scriptCompleted");
    assert_eq!(latest["deliveryStatus"], "partial");
    assert!(latest["deliveryErrors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error.as_str().unwrap_or_default().contains("webhook")));
    let artifact_path = std::path::PathBuf::from(latest["artifactPath"].as_str().unwrap());
    assert!(std::fs::read_to_string(artifact_path)
        .unwrap()
        .contains("delivery-result-ok"));
    assert!(std::fs::read_to_string(&delivered_path)
        .unwrap()
        .contains("delivery-result-ok"));

    let _ = std::fs::remove_dir_all(root);
}
