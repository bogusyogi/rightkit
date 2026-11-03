async fn wait_for_tool_invocation_state(
    storage: Arc<tokio::sync::Mutex<rightkit_agent_engine_storage::Storage>>,
    invocation_id: &str,
    lifecycle_state: &str,
    process_id: Option<&str>,
) {
    for _ in 0..800 {
        let row = {
            let store = storage.lock().await;
            store.get_tool_invocation(invocation_id).unwrap()
        };
        if let Some(row) = row {
            if row.lifecycle_state == lifecycle_state
                && process_id.is_none_or(|expected| row.process_id.as_deref() == Some(expected))
            {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!(
            "tool invocation {invocation_id} did not reach state {lifecycle_state:?} with process id {process_id:?}"
        );
}

#[test]
fn persist_tool_invocation_event_updates_sql_ledger() {
    let mut store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
    let call = ToolCall {
        id: "turn-persist:tc-read".to_string(),
        name: "read_file".to_string(),
        arguments_json: r#"{"path":"Cargo.toml","token":"secret"}"#.to_string(),
    };
    persist_tool_invocation_event(
        &mut store,
        &ProtocolEvent::ToolCallStarted {
            session_id: "s-persist".to_string(),
            seq: 1,
            created_at: "2026-06-08T00:00:00Z".to_string(),
            call,
        },
    )
    .unwrap();
    persist_tool_invocation_event(
        &mut store,
        &ProtocolEvent::ToolResultAdded {
            session_id: "s-persist".to_string(),
            seq: 2,
            created_at: "2026-06-08T00:00:01Z".to_string(),
            result: TranscriptMessage::Tool {
                id: "tr-1".to_string(),
                created_at: "2026-06-08T00:00:01Z".to_string(),
                tool_call_id: "turn-persist:tc-read".to_string(),
                summary: "read ok".to_string(),
                output: None,
                trust: None,
                is_error: false,
                process_id: Some("12345".to_string()),
                pinned: false,
            },
        },
    )
    .unwrap();

    let row = store
        .get_tool_invocation("turn-persist:tc-read")
        .unwrap()
        .unwrap();
    assert_eq!(row.lifecycle_state, "completed");
    assert_eq!(row.completed_at.as_deref(), Some("2026-06-08T00:00:01Z"));
    assert_eq!(row.process_id.as_deref(), Some("12345"));
    assert!(row.args_redacted_json.contains("[redacted]"));

    let events = store
        .list_tool_invocation_events("turn-persist:tc-read")
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, "toolResultAdded");
}

#[tokio::test]
async fn tool_invocation_route_returns_404_for_missing_invocation() {
    let app = crate::api::build_router(DaemonState::new(""));
    let (status, _) = json_get(app, "/tool-invocations/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancel_tool_invocation_resolves_pending_approval_as_denied() {
    let state = DaemonState::new("");
    state.create_session("s-tools", None, "/tmp", "Tools", None);
    let live = state.get_session("s-tools").unwrap();
    let call = ToolCall {
        id: "turn-2:tc-write".to_string(),
        name: "write_file".to_string(),
        arguments_json: r#"{"path":"out.txt","content":"hello"}"#.to_string(),
    };
    live.conduit.publish(ProtocolEvent::ToolApprovalRequested {
        session_id: "s-tools".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:02Z".to_string(),
        call,
        risk_label: "needs-approval".to_string(),
        proposed_change: Some("write out.txt".to_string()),
        policy_source: Some("test-policy".to_string()),
        middleware_decisions: vec![rightkit_agent_engine_protocol::MiddlewareDecisionEvidence {
            middleware_id: "builtin.observe-approvals".to_string(),
            title: "Observe approvals".to_string(),
            phase: "approvalRequested".to_string(),
            effect: "observe".to_string(),
            decision: "emitObservation".to_string(),
            reason: "Approval prompt emitted".to_string(),
        }],
    });
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    live.pending_approvals
        .lock()
        .unwrap()
        .insert("turn-2:tc-write".to_string(), tx);
    let app = crate::api::build_router(state);

    let (status, json) = json_post(
        app.clone(),
        "/tool-invocations/turn-2%3Atc-write/cancel",
        serde_json::json!({"reason":"operator cancelled"}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["cancelled"], true);
    assert_eq!(json["sessionId"], "s-tools");
    assert_eq!(json["toolInvocationId"], "turn-2:tc-write");
    assert!(!rx.await.unwrap());

    let (detail_status, detail) =
        json_get(app.clone(), "/tool-invocations/turn-2%3Atc-write").await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail["lifecycleState"], "cancelled");

    let (events_status, events) = json_get(app, "/tool-invocations/turn-2%3Atc-write/events").await;
    assert_eq!(events_status, StatusCode::OK);
    let rows = events.as_array().unwrap();
    assert_eq!(rows.last().unwrap()["kind"], "cancelRequested");
    assert_eq!(
        rows.last().unwrap()["payloadRedactedJson"]["reason"],
        "operator cancelled"
    );
}

#[tokio::test]
async fn cancel_tool_invocation_settles_pending_ask_user() {
    let state = DaemonState::new("");
    state.create_session("s-question", None, "/tmp", "Question", None);
    let live = state.get_session("s-question").unwrap();
    live.conduit.publish(ProtocolEvent::ToolCallStarted {
        session_id: "s-question".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:02Z".to_string(),
        call: ToolCall {
            id: "turn-3:tc-question".to_string(),
            name: "ask_user".to_string(),
            arguments_json: r#"{"questions":[]}"#.to_string(),
        },
    });
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    live.pending_questions
        .lock()
        .unwrap()
        .insert("q-1".to_string(), tx);
    let app = crate::api::build_router(state);

    let (status, json) = json_post(
        app.clone(),
        "/tool-invocations/turn-3%3Atc-question/cancel",
        serde_json::json!({"reason":"operator cancelled question"}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["cancelled"], true);
    assert!(rx.await.is_err());
    let (detail_status, detail) = json_get(app, "/tool-invocations/turn-3%3Atc-question").await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail["lifecycleState"], "cancelled");
}

#[test]
fn tool_invocation_openapi_components_cover_routes() {
    let schemas = tool_invocation_openapi_components();
    let record_schema = schemas.get("ToolInvocationRecord").unwrap();
    assert_eq!(record_schema["type"], "object");
    let required = record_schema["required"].as_array().unwrap();
    assert!(required.contains(&serde_json::json!("id")));
    assert!(required.contains(&serde_json::json!("sessionId")));
    assert!(required.contains(&serde_json::json!("artifactIds")));

    let events_schema = schemas.get("ToolInvocationEventRow").unwrap();
    assert_eq!(events_schema["type"], "object");
    assert_eq!(
        tool_invocations_list_response_schema()["items"]["$ref"],
        "#/components/schemas/ToolInvocationRecord"
    );
    assert_eq!(
        tool_invocation_response_schema()["$ref"],
        "#/components/schemas/ToolInvocationRecord"
    );
    assert_eq!(
        cancel_tool_invocation_request_schema()["$ref"],
        "#/components/schemas/CancelToolInvocationRequest"
    );
    assert_eq!(
        cancel_tool_invocation_response_schema()["$ref"],
        "#/components/schemas/CancelToolInvocationResponse"
    );
    assert_eq!(
        tool_invocation_events_response_schema()["items"]["$ref"],
        "#/components/schemas/ToolInvocationEventRow"
    );
}
