#[tokio::test]
async fn slash_export_returns_real_live_transcript_markdown_and_json() {
    let state = DaemonState::new("");
    state.create_session("exported", None, "/tmp", "Exported", None);
    let live = state.get_session("exported").unwrap();
    {
        let mut transcript = lock_recover(&live.transcript, "session.transcript");
        transcript.push(TranscriptMessage::System {
            id: "s1".to_string(),
            created_at: "2026-06-06T00:00:00Z".to_string(),
            content: "hidden system policy".to_string(),
            pinned: true,
        });
        transcript.push(TranscriptMessage::User {
            id: "u1".to_string(),
            created_at: "2026-06-06T00:00:00Z".to_string(),
            content: "please fix the flaky test\n<system-reminder>hidden clock</system-reminder>"
                .to_string(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        });
        transcript.push(TranscriptMessage::Assistant {
            id: "a1".to_string(),
            created_at: "2026-06-06T00:00:01Z".to_string(),
            content: "I found the race and patched it.".to_string(),
            reasoning_content: Some("private chain of thought".to_string()),
            tool_calls: vec![rightkit_agent_engine_protocol::ToolCall {
                id: "tc1".to_string(),
                name: "shell".to_string(),
                arguments_json: serde_json::json!({"command": "cargo test"}).to_string(),
            }],
            pinned: false,
        });
        transcript.push(TranscriptMessage::Tool {
            id: "t1".to_string(),
            created_at: "2026-06-06T00:00:02Z".to_string(),
            tool_call_id: "tc1".to_string(),
            summary: "cargo test output".to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        });
    }
    let app = build_router(state);

    let markdown = execute_slash_command(app.clone(), "/export exported").await;
    assert_eq!(markdown["success"], true);
    let output = markdown["output"].as_str().unwrap();
    assert!(output.contains("**Session export:** exported"), "{output}");
    assert!(output.contains("## User"), "{output}");
    assert!(output.contains("please fix the flaky test"), "{output}");
    assert!(output.contains("## Assistant"), "{output}");
    assert!(output.contains("I found the race"), "{output}");
    assert!(!output.contains("hidden system policy"), "{output}");
    assert!(!output.contains("hidden clock"), "{output}");
    assert!(!output.contains("private chain of thought"), "{output}");
    assert!(!output.contains("Tool calls:"), "{output}");
    assert!(!output.contains("cargo test output"), "{output}");

    let json = execute_slash_command(app, "/export exported --json").await;
    assert_eq!(json["success"], true);
    let output = json["output"].as_str().unwrap();
    assert!(output.contains(r#""sessionId": "exported""#), "{output}");
    assert!(output.contains(r#""$kind": "user""#), "{output}");
    assert!(output.contains("please fix the flaky test"), "{output}");
    assert!(output.contains("hidden clock"), "{output}");
    assert!(output.contains("cargo test output"), "{output}");
}

#[tokio::test]
async fn slash_export_uses_active_session_context() {
    let state = DaemonState::new("");
    state.create_session("active-export", None, "/tmp", "Exported", None);
    let live = state.get_session("active-export").unwrap();
    lock_recover(&live.transcript, "session.transcript").push(TranscriptMessage::User {
        id: "u1".to_string(),
        created_at: "2026-06-06T00:00:00Z".to_string(),
        content: "export this active transcript".to_string(),
        trust: None,
        pinned: false,
        attachments: Vec::new(),
    });
    let app = build_router(state);

    let export = execute_slash_command_for_session(app, "/export --json", "active-export").await;

    assert_eq!(export["success"], true);
    let output = export["output"].as_str().unwrap();
    assert!(
        output.contains(r#""sessionId": "active-export""#),
        "{output}"
    );
    assert!(output.contains("export this active transcript"), "{output}");
}

#[tokio::test]
async fn slash_summary_returns_deterministic_transcript_summary() {
    let state = DaemonState::new("");
    state.create_session("summarized", None, "/tmp", "Summarized", None);
    let live = state.get_session("summarized").unwrap();
    {
        let mut transcript = lock_recover(&live.transcript, "session.transcript");
        transcript.push(TranscriptMessage::User {
            id: "u1".to_string(),
            created_at: "2026-06-06T00:00:00Z".to_string(),
            content: "please investigate the failing remote smoke".to_string(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        });
        transcript.push(TranscriptMessage::Tool {
            id: "tr1".to_string(),
            created_at: "2026-06-06T00:00:01Z".to_string(),
            tool_call_id: "tc1".to_string(),
            summary: "cargo test --workspace passed".to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        });
        transcript.push(TranscriptMessage::Assistant {
            id: "a1".to_string(),
            created_at: "2026-06-06T00:00:02Z".to_string(),
            content: "The remote smoke is green after the Telegram allowlist fix.".to_string(),
            reasoning_content: None,
            tool_calls: vec![],
            pinned: false,
        });
    }
    let app = build_router(state);

    let summary = execute_slash_command(app, "/summary summarized").await;

    assert_eq!(summary["success"], true);
    let output = summary["output"].as_str().unwrap();
    assert!(
        output.contains("**Session summary:** summarized"),
        "{output}"
    );
    assert!(output.contains("Messages: 3"), "{output}");
    assert!(output.contains("- User: 1"), "{output}");
    assert!(output.contains("- Assistant: 1"), "{output}");
    assert!(output.contains("- Tool results: 1 (0 errors)"), "{output}");
    assert!(
        output.contains("please investigate the failing remote smoke"),
        "{output}"
    );
    assert!(
        output.contains("remote smoke is green after the Telegram allowlist fix"),
        "{output}"
    );
    assert!(output.contains("`tc1` ok"), "{output}");
}

#[tokio::test]
async fn slash_summary_uses_active_session_context() {
    let state = DaemonState::new("");
    state.create_session("active-summary", None, "/tmp", "Summarized", None);
    let live = state.get_session("active-summary").unwrap();
    lock_recover(&live.transcript, "session.transcript").push(TranscriptMessage::User {
        id: "u1".to_string(),
        created_at: "2026-06-06T00:00:00Z".to_string(),
        content: "summarize this active session".to_string(),
        trust: None,
        pinned: false,
        attachments: Vec::new(),
    });
    let app = build_router(state);

    let summary = execute_slash_command_for_session(app, "/summary", "active-summary").await;

    assert_eq!(summary["success"], true);
    let output = summary["output"].as_str().unwrap();
    assert!(
        output.contains("**Session summary:** active-summary"),
        "{output}"
    );
    assert!(output.contains("summarize this active session"), "{output}");
}

#[tokio::test]
async fn slash_handoff_returns_session_attach_payload() {
    let state = state_with_session("s1").with_runtime_info(61234, "/tmp/cr.db");
    let app = build_router(state);
    let handoff = execute_slash_command(app, "/handoff s1").await;
    assert_eq!(handoff["success"], true);
    let output = handoff["output"].as_str().unwrap();
    assert!(output.contains("**Session handoff:** s1"), "{output}");
    assert!(
        output.contains(r#""kind": "coderight.session_handoff.v1""#),
        "{output}"
    );
    assert!(
        output.contains(r#""webUrl": "http://127.0.0.1:61234/?pane=session&session=s1""#),
        "{output}"
    );
    assert!(
        output.contains(r#""eventsHistory": "/sessions/s1/events/history?after=0""#),
        "{output}"
    );
}

#[tokio::test]
async fn slash_handoff_uses_active_session_context() {
    let state = state_with_session("active-handoff").with_runtime_info(61234, "/tmp/cr.db");
    let app = build_router(state);

    let handoff = execute_slash_command_for_session(app, "/handoff", "active-handoff").await;

    assert_eq!(handoff["success"], true);
    let output = handoff["output"].as_str().unwrap();
    assert!(
        output.contains("**Session handoff:** active-handoff"),
        "{output}"
    );
    assert!(
        output
            .contains(r#""webUrl": "http://127.0.0.1:61234/?pane=session&session=active-handoff""#),
        "{output}"
    );
}

#[tokio::test]
async fn slash_handoff_aliases_return_session_attach_payload() {
    let state = state_with_session("s1").with_runtime_info(61234, "/tmp/cr.db");
    let app = build_router(state);
    for alias in [
        "/bridge s1",
        "/ide s1",
        "/desktop s1",
        "/mobile s1",
        "/teleport s1",
    ] {
        let handoff = execute_slash_command(app.clone(), alias).await;
        assert_eq!(handoff["success"], true, "{alias}: {handoff}");
        let output = handoff["output"].as_str().unwrap();
        assert!(
            output.contains("**Session handoff:** s1"),
            "{alias}: {output}"
        );
        assert!(
            output.contains(r#""kind": "coderight.session_handoff.v1""#),
            "{alias}: {output}"
        );
    }
}

#[tokio::test]
async fn slash_handoff_redacts_daemon_access_token() {
    let state = DaemonState::new("secret-token").with_runtime_info(61234, "/tmp/cr.db");
    state.create_session("s1", None, "/tmp", "Test", None);
    let app = build_router(state);
    let handoff = execute_slash_command_with_token(app, "/handoff s1", Some("secret-token")).await;
    assert_eq!(handoff["success"], true);
    let output = handoff["output"].as_str().unwrap();
    assert!(!output.contains("secret-token"), "{output}");
    assert!(
        output.contains(r#""accessToken": "[redacted]""#),
        "{output}"
    );
}
