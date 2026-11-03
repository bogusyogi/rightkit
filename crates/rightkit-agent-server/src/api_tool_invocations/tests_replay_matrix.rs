#[tokio::test]
async fn raw_event_restart_replay_covers_tool_invocation_lifecycle_matrix() {
    // Own the parent directory: `Storage::open` hardens the database's
    // parent, which fails when the system temp dir belongs to another
    // process. A private tempdir keeps this test independent of who owns
    // the ambient TMP location.
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join(format!(
        "coderight-tool-invocation-lifecycle-matrix-{}.sqlite",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let mut store = rightkit_agent_engine_storage::Storage::open(&db_path).unwrap();
        store
            .create_session(&rightkit_agent_engine_storage::SessionRow {
                id: "s-lifecycle-replay".to_string(),
                title: "Lifecycle replay".to_string(),
                workspace_root: "/tmp".to_string(),
                created_at: "2026-06-08T03:00:00Z".to_string(),
                updated_at: "2026-06-08T03:00:08Z".to_string(),
                parent_session_id: None,
                role: None,
                status: "dormant".to_string(),
                config_json: "{}".to_string(),
                tags: "[]".to_string(),
                recovery_marker: None,
            })
            .unwrap();

        let append_event = |store: &mut rightkit_agent_engine_storage::Storage,
                            kind: &str,
                            event: ProtocolEvent| {
            store
                .append_event(
                    "s-lifecycle-replay",
                    kind,
                    &serde_json::to_string(&event).unwrap(),
                )
                .unwrap();
        };
        let call = |id: &str, name: &str, args: &str| ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments_json: args.to_string(),
        };

        append_event(
            &mut store,
            "toolCallStarted",
            ProtocolEvent::ToolCallStarted {
                session_id: "s-lifecycle-replay".to_string(),
                seq: 1,
                created_at: "2026-06-08T03:00:01Z".to_string(),
                call: call(
                    "turn-life:requested",
                    "read_file",
                    r#"{"path":"Cargo.toml","api_key":"secret"}"#,
                ),
            },
        );
        append_event(
            &mut store,
            "toolApprovalRequested",
            ProtocolEvent::ToolApprovalRequested {
                session_id: "s-lifecycle-replay".to_string(),
                seq: 2,
                created_at: "2026-06-08T03:00:02Z".to_string(),
                call: call(
                    "turn-life:approval",
                    "write_file",
                    r#"{"path":"out.txt","content":"hello"}"#,
                ),
                risk_label: "needs-approval".to_string(),
                proposed_change: Some("write out.txt".to_string()),
                policy_source: Some("policy:approval".to_string()),
                middleware_decisions: Vec::new(),
            },
        );
        append_event(
            &mut store,
            "toolApprovalRequested",
            ProtocolEvent::ToolApprovalRequested {
                session_id: "s-lifecycle-replay".to_string(),
                seq: 3,
                created_at: "2026-06-08T03:00:03Z".to_string(),
                call: call(
                    "turn-life:approved",
                    "shell_command",
                    r#"{"command":"echo ok"}"#,
                ),
                risk_label: "needs-approval".to_string(),
                proposed_change: Some("run echo".to_string()),
                policy_source: Some("policy:approved".to_string()),
                middleware_decisions: Vec::new(),
            },
        );
        append_event(
            &mut store,
            "toolApprovalResolved",
            ProtocolEvent::ToolApprovalResolved {
                session_id: "s-lifecycle-replay".to_string(),
                seq: 4,
                created_at: "2026-06-08T03:00:04Z".to_string(),
                tool_call_id: "turn-life:approved".to_string(),
                approved: true,
            },
        );
        append_event(
            &mut store,
            "toolApprovalRequested",
            ProtocolEvent::ToolApprovalRequested {
                session_id: "s-lifecycle-replay".to_string(),
                seq: 5,
                created_at: "2026-06-08T03:00:05Z".to_string(),
                call: call(
                    "turn-life:denied",
                    "write_file",
                    r#"{"path":"blocked.txt","content":"no"}"#,
                ),
                risk_label: "needs-approval".to_string(),
                proposed_change: Some("write blocked.txt".to_string()),
                policy_source: Some("policy:denied".to_string()),
                middleware_decisions: Vec::new(),
            },
        );
        append_event(
            &mut store,
            "toolApprovalResolved",
            ProtocolEvent::ToolApprovalResolved {
                session_id: "s-lifecycle-replay".to_string(),
                seq: 6,
                created_at: "2026-06-08T03:00:06Z".to_string(),
                tool_call_id: "turn-life:denied".to_string(),
                approved: false,
            },
        );
        append_event(
            &mut store,
            "toolCallStarted",
            ProtocolEvent::ToolCallStarted {
                session_id: "s-lifecycle-replay".to_string(),
                seq: 7,
                created_at: "2026-06-08T03:00:07Z".to_string(),
                call: call(
                    "turn-life:failed",
                    "shell_command",
                    r#"{"command":"cat /root/secret","token":"secret"}"#,
                ),
            },
        );
        append_event(
            &mut store,
            "toolResultAdded",
            ProtocolEvent::ToolResultAdded {
                session_id: "s-lifecycle-replay".to_string(),
                seq: 8,
                created_at: "2026-06-08T03:00:08Z".to_string(),
                result: TranscriptMessage::Tool {
                    id: "tr-life-failed".to_string(),
                    created_at: "2026-06-08T03:00:08Z".to_string(),
                    tool_call_id: "turn-life:failed".to_string(),
                    summary: "permission denied reading /root/secret".to_string(),
                    output: None,
                    trust: None,
                    is_error: true,
                    process_id: Some("life-pid-500".to_string()),
                    pinned: false,
                },
            },
        );
    }

    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open(&db_path).unwrap(),
    ));
    let app = crate::api::build_router(DaemonState::new("").with_storage(storage));

    let (list_status, list) =
        json_get(app.clone(), "/sessions/s-lifecycle-replay/tool-invocations").await;
    assert_eq!(list_status, StatusCode::OK);
    let records = list.as_array().unwrap();
    assert_eq!(records.len(), 5);
    let state_for = |id: &str| {
        records
            .iter()
            .find(|record| record["id"] == id)
            .and_then(|record| record["lifecycleState"].as_str())
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(state_for("turn-life:requested"), "requested");
    assert_eq!(state_for("turn-life:approval"), "approvalRequested");
    assert_eq!(state_for("turn-life:approved"), "approved");
    assert_eq!(state_for("turn-life:denied"), "denied");
    assert_eq!(state_for("turn-life:failed"), "failed");

    let requested = records
        .iter()
        .find(|record| record["id"] == "turn-life:requested")
        .unwrap();
    assert_eq!(requested["argsRedactedJson"]["api_key"], "[redacted]");

    let (failed_status, failed) =
        json_get(app.clone(), "/tool-invocations/turn-life%3Afailed").await;
    assert_eq!(failed_status, StatusCode::OK);
    assert_eq!(failed["processId"], "life-pid-500");
    assert_eq!(failed["errorClass"], "permission_denied");
    assert!(failed["outputHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    let (events_status, events) =
        json_get(app, "/tool-invocations/turn-life%3Adenied/events").await;
    assert_eq!(events_status, StatusCode::OK);
    let event_rows = events.as_array().unwrap();
    assert_eq!(event_rows.len(), 2);
    assert_eq!(event_rows[0]["kind"], "toolApprovalRequested");
    assert_eq!(event_rows[1]["kind"], "toolApprovalResolved");
    assert_eq!(event_rows[1]["payloadRedactedJson"]["approved"], false);

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn unmatched_pre_effect_origin_reconciles_once_as_interrupted_unknown() {
    let mut store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
    store
        .create_session(&rightkit_agent_engine_storage::SessionRow {
            id: "s-unknown".into(),
            title: "Unknown effect".into(),
            workspace_root: "/tmp".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            parent_session_id: None,
            role: None,
            status: "dormant".into(),
            config_json: "{}".into(),
            tags: String::new(),
            recovery_marker: None,
        })
        .unwrap();
    let start = ProtocolEvent::ToolCallStarted {
        session_id: "s-unknown".into(),
        seq: 1,
        created_at: "t1".into(),
        call: ToolCall {
            id: "turn-1:tc-write".into(),
            name: "write_file".into(),
            arguments_json: r#"{"path":"x.txt","content":"changed"}"#.into(),
        },
    };
    let payload = serde_json::to_string(&start).unwrap();
    store
        .append_event_at_seq("s-unknown", 1, "toolCallStarted", &payload)
        .unwrap();
    let before = store.all_events("s-unknown").unwrap();
    assert_eq!(
        reconcile_interrupted_effects(&mut store, "s-unknown", &before).unwrap(),
        1
    );
    let after = store.all_events("s-unknown").unwrap();
    assert_eq!(after.len(), 2);
    let settlement: ProtocolEvent = serde_json::from_str(&after[1].payload_json).unwrap();
    match settlement {
        ProtocolEvent::ToolResultAdded {
            result:
                TranscriptMessage::Tool {
                    tool_call_id,
                    summary,
                    is_error,
                    ..
                },
            ..
        } => {
            assert_eq!(tool_call_id, "turn-1:tc-write");
            assert!(summary.contains("interrupted/unknown"));
            assert!(is_error);
        }
        other => panic!("expected interrupted settlement result, got {other:?}"),
    }
    let row = store
        .get_tool_invocation("turn-1:tc-write")
        .unwrap()
        .unwrap();
    assert_eq!(row.lifecycle_state, "interrupted_unknown");
    assert_eq!(row.error_class.as_deref(), Some("interrupted_unknown"));
    let invocation_events = store
        .list_tool_invocation_events("turn-1:tc-write")
        .unwrap();
    assert!(invocation_events
        .iter()
        .any(|event| event.kind == "effectSettlementInterruptedUnknown"));
    assert!(
        !rightkit_agent_engine_protocol::ExternalEffectSettlement::InterruptedUnknown
            .allows_automatic_non_idempotent_replay()
    );
    assert_eq!(
        reconcile_interrupted_effects(&mut store, "s-unknown", &after).unwrap(),
        0
    );
    assert_eq!(store.all_events("s-unknown").unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_created_tool_invocations_replay_after_fresh_state_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("tool-invocation-engine-replay.sqlite");
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open(&db_path).unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    let live = state.create_session("s-engine-replay", None, "/tmp", "Engine replay", None);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let completed_call = ToolCall {
        id: "turn-engine:tc-shell".to_string(),
        name: "shell_command".to_string(),
        arguments_json: r#"{"command":"echo replay"}"#.to_string(),
    };
    live.conduit.publish(ProtocolEvent::ToolCallStarted {
        session_id: "s-engine-replay".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T01:00:00Z".to_string(),
        call: completed_call,
    });
    live.conduit.publish(ProtocolEvent::ToolResultAdded {
        session_id: "s-engine-replay".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T01:00:01Z".to_string(),
        result: TranscriptMessage::Tool {
            id: "tool-result-1".to_string(),
            created_at: "2026-06-08T01:00:01Z".to_string(),
            tool_call_id: "turn-engine:tc-shell".to_string(),
            summary: "echo replay".to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: Some("engine-pid-777".to_string()),
            pinned: false,
        },
    });

    let cancellable_call = ToolCall {
        id: "turn-engine:tc-bg".to_string(),
        name: "background_shell".to_string(),
        arguments_json: r#"{"command":"sleep 30"}"#.to_string(),
    };
    live.conduit.publish(ProtocolEvent::ToolCallStarted {
        session_id: "s-engine-replay".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T01:00:02Z".to_string(),
        call: cancellable_call,
    });
    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    live.cancel_tokens
        .lock()
        .unwrap()
        .insert("turn-engine:tc-bg".to_string(), cancel_tx);

    wait_for_tool_invocation_state(
        storage.clone(),
        "turn-engine:tc-shell",
        "completed",
        Some("engine-pid-777"),
    )
    .await;
    wait_for_tool_invocation_state(storage.clone(), "turn-engine:tc-bg", "requested", None).await;

    let app = crate::api::build_router(state.clone());
    let (cancel_status, cancel_json) = json_post(
        app,
        "/tool-invocations/turn-engine%3Atc-bg/cancel",
        serde_json::json!({"reason":"restart replay audit"}),
    )
    .await;
    assert_eq!(cancel_status, StatusCode::OK);
    assert_eq!(cancel_json["cancelled"], true);
    cancel_rx.changed().await.unwrap();
    assert!(*cancel_rx.borrow());
    wait_for_tool_invocation_state(storage.clone(), "turn-engine:tc-bg", "cancelled", None).await;

    let fresh_storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open(&db_path).unwrap(),
    ));
    let fresh_app =
        crate::api::build_router(DaemonState::new("").with_storage(fresh_storage.clone()));

    let (list_status, list) = json_get(
        fresh_app.clone(),
        "/sessions/s-engine-replay/tool-invocations",
    )
    .await;
    assert_eq!(list_status, StatusCode::OK);
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "turn-engine:tc-shell");
    assert_eq!(rows[0]["lifecycleState"], "completed");
    assert_eq!(rows[0]["processId"], "engine-pid-777");
    assert_eq!(rows[1]["id"], "turn-engine:tc-bg");
    assert_eq!(rows[1]["lifecycleState"], "cancelled");

    let (detail_status, detail) = json_get(
        fresh_app.clone(),
        "/tool-invocations/turn-engine%3Atc-shell",
    )
    .await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail["processId"], "engine-pid-777");

    let (events_status, events) =
        json_get(fresh_app, "/tool-invocations/turn-engine%3Atc-bg/events").await;
    assert_eq!(events_status, StatusCode::OK);
    let event_rows = events.as_array().unwrap();
    assert!(event_rows
        .iter()
        .any(|event| event["kind"] == "cancelRequested"));
    let cancellation = {
        let store = fresh_storage.lock().await;
        store
            .get_tool_invocation_cancellation("turn-engine:tc-bg")
            .unwrap()
            .unwrap()
    };
    assert_eq!(cancellation.reason.as_deref(), Some("restart replay audit"));

    let _ = std::fs::remove_file(db_path);
}
