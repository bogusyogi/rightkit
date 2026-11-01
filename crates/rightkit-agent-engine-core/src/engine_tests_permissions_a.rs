use super::*;

#[tokio::test]
async fn events_are_emitted_to_conduit() {
    let (mut e, c) = engine_with(vec![LlmResponse::text("hello")], PermissionMode::Bypass);
    e.submit_user_message("hi").await.unwrap();
    assert!(c.history_len() >= 3);
}

#[tokio::test]
async fn denied_tool_records_error_and_continues() {
    let call = ToolCall {
        id: "tc1".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "echo hi"}).to_string(),
    };
    let (mut e, c) = engine_with(
        vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("ok then"),
        ],
        PermissionMode::Plan,
    );
    let out = e.submit_user_message("run echo").await.unwrap();
    assert_eq!(out, "ok then");
    let has_error = e.transcript().messages().iter().any(
        |m| matches!(m, rightkit_agent_engine_protocol::TranscriptMessage::Tool { is_error, .. } if *is_error),
    );
    assert!(has_error);
    let _ = c;
}

#[tokio::test]
async fn session_permission_rule_can_deny_even_in_bypass() {
    let mut store = RuleStore::new();
    store.add(Rule::new(
        "shell",
        PermissionDecision::Deny,
        RuleSource::Session,
    ));
    let call = ToolCall {
        id: "tc1".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "echo should-not-run"}).to_string(),
    };
    let (mut e, _c) = engine_with_rules(
        vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("after deny"),
        ],
        PermissionMode::Bypass,
        Arc::new(Mutex::new(store)),
    );

    let out = e.submit_user_message("run").await.unwrap();
    assert_eq!(out, "after deny");
    let denied = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: true,
                ..
            } if summary.contains("permission denied")
                && summary.contains("rule:session:shell")
        )
    });
    assert!(denied);
}

#[tokio::test]
async fn session_permission_rule_can_deny_read_only_batch_call() {
    let mut store = RuleStore::new();
    store.add(Rule::new(
        "read_file",
        PermissionDecision::Deny,
        RuleSource::Session,
    ));
    let call = ToolCall {
        id: "tc1".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "Cargo.toml"}).to_string(),
    };
    let (mut e, _c) = engine_with_rules(
        vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("after deny"),
        ],
        PermissionMode::Bypass,
        Arc::new(Mutex::new(store)),
    );

    let out = e.submit_user_message("read").await.unwrap();
    assert_eq!(out, "after deny");
    let denied = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: true,
                ..
            } if summary.contains("rule:session:read_file")
        )
    });
    assert!(denied);
}

#[tokio::test]
async fn qualified_permission_rule_denies_shell_command_match() {
    let mut store = RuleStore::new();
    store.add(Rule::new(
        "Bash(git push*)",
        PermissionDecision::Deny,
        RuleSource::Session,
    ));
    let call = ToolCall {
        id: "tc1".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "git push origin main"}).to_string(),
    };
    let (mut e, _c) = engine_with_rules(
        vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("after deny"),
        ],
        PermissionMode::Bypass,
        Arc::new(Mutex::new(store)),
    );

    let out = e.submit_user_message("push").await.unwrap();
    assert_eq!(out, "after deny");
    let denied = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: true,
                ..
            } if summary.contains("Bash(git push*)")
        )
    });
    assert!(denied);
}

#[tokio::test]
async fn qualified_permission_rule_denies_read_only_path_fast_path() {
    let mut store = RuleStore::new();
    store.add(Rule::new(
        "read_file(private/**)",
        PermissionDecision::Deny,
        RuleSource::Session,
    ));
    let call = ToolCall {
        id: "tc1".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "private/key.txt"}).to_string(),
    };
    let (mut e, _c) = engine_with_rules(
        vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("after deny"),
        ],
        PermissionMode::Bypass,
        Arc::new(Mutex::new(store)),
    );

    let out = e.submit_user_message("read secret").await.unwrap();
    assert_eq!(out, "after deny");
    let denied = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: true,
                ..
            } if summary.contains("read_file(private/**)")
        )
    });
    assert!(denied, "{:#?}", e.transcript().messages());
}

#[tokio::test]
async fn ask_mode_emits_approval_request() {
    let call = ToolCall {
        id: "tc1".into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({"path": "x.txt", "content": "hi"}).to_string(),
    };
    let workspace = std::env::temp_dir().join(format!(
        "cr-ask-approval-{}",
        rightkit_agent_engine_protocol::uuid_v7()
    ));
    std::fs::create_dir_all(&workspace).unwrap();
    let c = Arc::new(Conduit::new(256));
    let mut e = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("waiting"),
        ])),
        Arc::new(ToolRegistry::build_default()),
        c.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Ask,
            workspace_root: workspace,
            // Post-edit verification is not under test; it would force an
            // extra, unscripted model turn after the approved write.
            auto_post_edit_diagnostics: false,
            ..Default::default()
        },
    );

    // Share the approval store so the test can resolve it.
    let approvals = e.pending_approvals.clone();
    let mut sub = c.subscribe_human();

    // Run the engine concurrently; it will pause waiting for tool approval.
    let engine_task =
        tokio::spawn(async move { e.submit_user_message("write a file").await.unwrap() });

    // Wait until the ToolApprovalRequested event appears, then approve it.
    loop {
        if let crate::conduit::ConduitReceive::Event(ev) = sub.recv().await {
            if matches!(ev, ProtocolEvent::ToolApprovalRequested { .. }) {
                // Resolve the approval so the engine can continue.
                let sender = approvals.lock().unwrap().remove("tc1");
                if let Some(tx) = sender {
                    let _ = tx.send(true);
                }
                break;
            }
        }
    }

    engine_task.await.unwrap();

    let approval = c
        .events_after(0)
        .iter()
        .any(|ev| matches!(ev, ProtocolEvent::ToolApprovalRequested { .. }));
    assert!(approval);
}
