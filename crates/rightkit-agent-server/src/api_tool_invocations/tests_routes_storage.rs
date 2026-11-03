#[tokio::test]
async fn tool_invocations_route_lists_normalized_lifecycle_records() {
    let state = DaemonState::new("");
    publish_read_file_lifecycle(&state);
    let app = crate::api::build_router(state);

    let (status, json) = json_get(app, "/sessions/s-tools/tool-invocations").await;

    assert_eq!(status, StatusCode::OK);
    let records = json.as_array().unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["id"], "turn-1:tc-read");
    assert_eq!(record["toolName"], "read_file");
    assert_eq!(record["lifecycleState"], "completed");
    assert_eq!(record["argsRedactedJson"]["api_key"], "[redacted]");
    assert!(record["schemaVersion"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert!(record["outputHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
}

#[tokio::test]
async fn tool_invocation_detail_and_events_routes_find_live_invocation() {
    let state = DaemonState::new("");
    publish_read_file_lifecycle(&state);
    let app = crate::api::build_router(state);

    let (detail_status, detail) = json_get(app.clone(), "/tool-invocations/turn-1:tc-read").await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail["sessionId"], "s-tools");

    let (events_status, events) = json_get(app, "/tool-invocations/turn-1:tc-read/events").await;
    assert_eq!(events_status, StatusCode::OK);
    let rows = events.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["kind"], "toolCallStarted");
    let arguments: serde_json::Value = serde_json::from_str(
        rows[0]["payloadRedactedJson"]["call"]["argumentsJson"]
            .as_str()
            .expect("redacted arguments JSON"),
    )
    .expect("parse redacted arguments JSON");
    assert_eq!(arguments["api_key"], "[redacted]");
    assert_eq!(arguments["path"], "Cargo.toml");
}

#[tokio::test]
async fn tool_invocation_routes_replay_sql_ledger_after_storage_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("tool-invocation-replay.sqlite");
    {
        let store = rightkit_agent_engine_storage::Storage::open(&db_path).unwrap();
        store
            .upsert_tool_invocation(&rightkit_agent_engine_storage::ToolInvocationStoreRow {
                id: "turn-sql:tc-read".to_string(),
                session_id: "s-sql".to_string(),
                turn_id: "turn-sql".to_string(),
                tool_name: "read_file".to_string(),
                schema_version: "sha256:test-schema".to_string(),
                args_hash: "sha256:test-args".to_string(),
                args_redacted_json: r#"{"path":"Cargo.toml"}"#.to_string(),
                lifecycle_state: "completed".to_string(),
                policy_verdict_id: None,
                approval_id: None,
                process_id: Some("4242".to_string()),
                started_at: Some("2026-06-08T00:00:00Z".to_string()),
                completed_at: Some("2026-06-08T00:00:01Z".to_string()),
                output_hash: Some("sha256:test-output".to_string()),
                artifact_ids_json: r#"["artifact-1"]"#.to_string(),
                error_class: None,
                updated_at: "2026-06-08T00:00:01Z".to_string(),
            })
            .unwrap();
        store
            .append_tool_invocation_event(
                &rightkit_agent_engine_storage::ToolInvocationEventStoreRow {
                    invocation_id: "turn-sql:tc-read".to_string(),
                    session_id: "s-sql".to_string(),
                    seq: 1,
                    kind: "toolCallStarted".to_string(),
                    created_at: "2026-06-08T00:00:00Z".to_string(),
                    payload_redacted_json: r#"{"call":{"id":"turn-sql:tc-read"}}"#.to_string(),
                },
            )
            .unwrap();
        store
            .append_tool_invocation_event(
                &rightkit_agent_engine_storage::ToolInvocationEventStoreRow {
                    invocation_id: "turn-sql:tc-read".to_string(),
                    session_id: "s-sql".to_string(),
                    seq: 2,
                    kind: "toolResultAdded".to_string(),
                    created_at: "2026-06-08T00:00:01Z".to_string(),
                    payload_redacted_json:
                        r#"{"toolCallId":"turn-sql:tc-read","processId":"4242"}"#.to_string(),
                },
            )
            .unwrap();
        store
            .upsert_tool_invocation_cancellation(
                &rightkit_agent_engine_storage::ToolInvocationCancellationRow {
                    invocation_id: "turn-sql:tc-read".to_string(),
                    session_id: "s-sql".to_string(),
                    reason: Some("operator replay audit".to_string()),
                    created_at: "2026-06-08T00:00:02Z".to_string(),
                },
            )
            .unwrap();
    }

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open(&db_path).unwrap(),
    ));
    {
        let store = storage.lock().await;
        let cancellation = store
            .get_tool_invocation_cancellation("turn-sql:tc-read")
            .unwrap()
            .unwrap();
        assert_eq!(
            cancellation.reason.as_deref(),
            Some("operator replay audit")
        );
    }
    let app = crate::api::build_router(DaemonState::new("").with_storage(storage));

    let (list_status, list) = json_get(app.clone(), "/sessions/s-sql/tool-invocations").await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap()[0]["id"], "turn-sql:tc-read");
    assert_eq!(list.as_array().unwrap()[0]["artifactIds"][0], "artifact-1");
    assert_eq!(list.as_array().unwrap()[0]["processId"], "4242");

    let (detail_status, detail) =
        json_get(app.clone(), "/tool-invocations/turn-sql%3Atc-read").await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail["lifecycleState"], "completed");
    assert_eq!(detail["processId"], "4242");

    let (events_status, events) =
        json_get(app, "/tool-invocations/turn-sql%3Atc-read/events").await;
    assert_eq!(events_status, StatusCode::OK);
    assert_eq!(events.as_array().unwrap()[0]["kind"], "toolCallStarted");
    assert_eq!(events.as_array().unwrap()[1]["kind"], "toolResultAdded");
    assert_eq!(
        events.as_array().unwrap()[1]["payloadRedactedJson"]["processId"],
        "4242"
    );

    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn tool_invocation_routes_rebuild_from_raw_events_after_fresh_state_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir
        .path()
        .join("tool-invocation-raw-event-replay.sqlite");
    {
        let mut store = rightkit_agent_engine_storage::Storage::open(&db_path).unwrap();
        store
            .create_session(&rightkit_agent_engine_storage::SessionRow {
                id: "s-raw-replay".to_string(),
                title: "Raw replay".to_string(),
                workspace_root: "/tmp".to_string(),
                created_at: "2026-06-08T02:00:00Z".to_string(),
                updated_at: "2026-06-08T02:00:02Z".to_string(),
                parent_session_id: None,
                role: None,
                status: "dormant".to_string(),
                config_json: "{}".to_string(),
                tags: "[]".to_string(),
                recovery_marker: None,
            })
            .unwrap();
        let call = ToolCall {
            id: "turn-raw:tc-shell".to_string(),
            name: "shell_command".to_string(),
            arguments_json: r#"{"command":"echo replay","token":"secret"}"#.to_string(),
        };
        let started = ProtocolEvent::ToolCallStarted {
            session_id: "s-raw-replay".to_string(),
            seq: 1,
            created_at: "2026-06-08T02:00:01Z".to_string(),
            call,
        };
        let completed = ProtocolEvent::ToolResultAdded {
            session_id: "s-raw-replay".to_string(),
            seq: 2,
            created_at: "2026-06-08T02:00:02Z".to_string(),
            result: TranscriptMessage::Tool {
                id: "tr-raw".to_string(),
                created_at: "2026-06-08T02:00:02Z".to_string(),
                tool_call_id: "turn-raw:tc-shell".to_string(),
                summary: "raw replay ok".to_string(),
                output: None,
                trust: None,
                is_error: false,
                process_id: Some("raw-pid-31337".to_string()),
                pinned: false,
            },
        };
        store
            .append_event(
                "s-raw-replay",
                "toolCallStarted",
                &serde_json::to_string(&started).unwrap(),
            )
            .unwrap();
        store
            .append_event(
                "s-raw-replay",
                "toolResultAdded",
                &serde_json::to_string(&completed).unwrap(),
            )
            .unwrap();
    }

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open(&db_path).unwrap(),
    ));
    let app = crate::api::build_router(DaemonState::new("").with_storage(storage));

    let (list_status, list) =
        json_get(app.clone(), "/sessions/s-raw-replay/tool-invocations").await;
    assert_eq!(list_status, StatusCode::OK);
    let records = list.as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], "turn-raw:tc-shell");
    assert_eq!(records[0]["lifecycleState"], "completed");
    assert_eq!(records[0]["processId"], "raw-pid-31337");
    assert_eq!(records[0]["argsRedactedJson"]["token"], "[redacted]");
    assert!(records[0]["outputHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    let (detail_status, detail) =
        json_get(app.clone(), "/tool-invocations/turn-raw%3Atc-shell").await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail["sessionId"], "s-raw-replay");
    assert_eq!(detail["lifecycleState"], "completed");
    assert_eq!(detail["processId"], "raw-pid-31337");

    let (events_status, events) =
        json_get(app, "/tool-invocations/turn-raw%3Atc-shell/events").await;
    assert_eq!(events_status, StatusCode::OK);
    let event_rows = events.as_array().unwrap();
    assert_eq!(event_rows.len(), 2);
    assert_eq!(event_rows[0]["kind"], "toolCallStarted");
    assert_eq!(
        event_rows[0]["payloadRedactedJson"]["call"]["argumentsJson"],
        r#"{"command":"echo replay","token":"[redacted]"}"#
    );
    assert_eq!(event_rows[1]["kind"], "toolResultAdded");
    assert_eq!(
        event_rows[1]["payloadRedactedJson"]["toolCallId"],
        "turn-raw:tc-shell"
    );

    let _ = std::fs::remove_file(db_path);
}
