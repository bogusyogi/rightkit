#[tokio::test]
async fn live_session_row_reports_distinct_updated_at() {
    let state = DaemonState::new("");
    let live = state.create_session("s1", None, "D:\\tmp", "S1", None);
    *lock_recover(&live.updated_at, "session.updated_at") = "2026-06-08T00:00:01Z".to_string();

    let row = session_row(&live);

    assert_eq!(row["createdAt"], live.created_at);
    assert_eq!(row["updatedAt"], "2026-06-08T00:00:01Z");
}

#[tokio::test]
async fn blueprint_only_session_does_not_qualify_for_recents() {
    let state = DaemonState::new("");
    let live = state.create_session("blueprint-only", None, "D:\\tmp", "coderight", None);
    lock_recover(&live.transcript, "session.transcript").push(TranscriptMessage::Assistant {
        id: "blueprint-init-1".to_string(),
        created_at: "t".to_string(),
        content: "Blueprint verified".to_string(),
        reasoning_content: None,
        tool_calls: vec![],
        pinned: false,
    });

    assert_eq!(session_row(&live)["hasHistory"], false);
    lock_recover(&live.transcript, "session.transcript").push(TranscriptMessage::Compact {
        id: "compact-1".to_string(),
        created_at: "t".to_string(),
        summary: "earlier chat".to_string(),
        dropped_count: 2,
        pinned: false,
    });
    assert_eq!(session_row(&live)["hasHistory"], true);
    lock_recover(&live.transcript, "session.transcript")
        .retain(|message| !matches!(message, TranscriptMessage::Compact { .. }));
    lock_recover(&live.transcript, "session.transcript").push(TranscriptMessage::User {
        id: "user-1".to_string(),
        created_at: "t".to_string(),
        content: "hello".to_string(),
        trust: None,
        pinned: false,
        attachments: Vec::new(),
    });
    assert_eq!(session_row(&live)["hasHistory"], true);
}

#[tokio::test]
async fn session_row_recovers_from_poisoned_metadata_mutexes() {
    let state = DaemonState::new("");
    let live = state.create_session("metadata-poison", None, "D:\\tmp", "Poison Row", None);
    *lock_recover(&live.spec_id, "session.spec_id") = Some("spec-poison".to_string());

    let title = live.title.clone();
    let model = live.model.clone();
    let updated_at = live.updated_at.clone();
    let spec_id = live.spec_id.clone();
    for poison in [
        Box::new(move || {
            let _guard = lock_recover(&title, "session.title");
            panic!("poison title");
        }) as Box<dyn FnOnce() + Send>,
        Box::new(move || {
            let _guard = lock_recover(&model, "session.model");
            panic!("poison model");
        }),
        Box::new(move || {
            let _guard = lock_recover(&updated_at, "session.updated_at");
            panic!("poison updated_at");
        }),
        Box::new(move || {
            let _guard = lock_recover(&spec_id, "session.spec_id");
            panic!("poison spec_id");
        }),
    ] {
        let _ = std::thread::spawn(poison).join();
    }

    let row = session_row(&live);

    assert_eq!(row["id"], "metadata-poison");
    assert_eq!(row["title"], "Poison Row");
    assert_eq!(row["specId"], "spec-poison");
}

#[tokio::test]
async fn touch_session_recovers_from_poisoned_updated_at_mutex() {
    let state = DaemonState::new("");
    let live = state.create_session("touch-poison", None, "D:\\tmp", "Touch Poison", None);
    *lock_recover(&live.updated_at, "session.updated_at") = "2000-01-01T00:00:00Z".to_string();
    let updated_at = live.updated_at.clone();
    let _ = std::thread::spawn(move || {
        let _guard = lock_recover(&updated_at, "session.updated_at");
        panic!("poison updated_at");
    })
    .join();

    touch_session(&live);

    let row = session_row(&live);
    assert_ne!(row["updatedAt"], "2000-01-01T00:00:00Z");
}

#[tokio::test]
async fn live_session_row_matches_fixture_contract() {
    let state = DaemonState::new("");
    // Real temp workspace: a Windows-style literal is a relative path off
    // Windows, and a child session (parent-1) in the default Bypass mode forces
    // worktree isolation, which creates directories and a git worktree.
    let workspace = tempfile::tempdir().expect("temp workspace");
    let workspace_root = workspace.path().to_string_lossy().into_owned();
    let live = state.create_session_with_options(
        "session-fixture",
        Some("reviewer"),
        &workspace_root,
        "Fixture Session",
        Some("parent-1"),
        SessionRuntimeOptions {
            safe_mode: true,
            forked_from_session_id: Some("parent-1".to_string()),
            allowed_tools: vec!["read_file".to_string()],
            disallowed_tools: vec!["shell".to_string()],
            toolset: Some("review".to_string()),
            active_skills: vec!["reviewer".to_string()],
            execution_backend: Some("local".to_string()),
            execution_workspace_id: Some("workspace-1".to_string()),
            execution_network_profile: Some("locked".to_string()),
            mcp_channel: "cli".to_string(),
            spec_id: Some("spec-1".to_string()),
            effort: Some("high".to_string()),
            thinking_budget_tokens: Some(1024),
            disable_thinking: true,
            // Ask keeps the session in place; Bypass would isolate it.
            permission_mode: Some(rightkit_agent_policy::PermissionMode::Ask),
            ..SessionRuntimeOptions::default()
        },
    );
    *lock_recover(&live.updated_at, "session.updated_at") = "2026-06-24T01:02:04Z".to_string();
    let mut row = session_row(&live);
    // Compare as paths: the row may use `/` where the temp dir uses `\` on Windows.
    assert_eq!(
        std::path::Path::new(row["workspaceRoot"].as_str().expect("workspaceRoot string")),
        workspace.path()
    );
    // The fixture pins a portable placeholder for the machine-specific root.
    row["workspaceRoot"] = serde_json::json!("D:/work/.coderight/worktrees/session-fixture");
    row["createdAt"] = serde_json::json!("2026-06-24T01:02:03Z");
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/cli-json/session-row.v1.json"
    ))
    .expect("parse session row fixture");

    assert_eq!(row, fixture);
}

#[tokio::test]
async fn create_session_returns_session_row() {
    let app = build_router(DaemonState::new(""));
    let req = Request::builder()
        .method("POST")
        .uri("/sessions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"workspaceRoot":"/tmp","title":"my session"}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["id"].as_str().is_some());
    assert_eq!(v["title"], "my session");
    assert_eq!(v["workspaceRoot"], "/tmp");
}

#[tokio::test]
async fn session_spec_binding_roundtrips_through_sessions_api() {
    let app = build_router(DaemonState::new(""));
    let create_spec = Request::builder()
        .method("POST")
        .uri("/specs")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support"}"#,
        ))
        .unwrap();
    let spec_resp = app.clone().oneshot(create_spec).await.unwrap();
    assert_eq!(spec_resp.status(), StatusCode::CREATED);
    let body = spec_resp.into_body().collect().await.unwrap().to_bytes();
    let spec: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = spec["id"].as_str().unwrap();

    let create_session = Request::builder()
        .method("POST")
        .uri("/sessions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"workspaceRoot":"D:/work","title":"spec session"}"#,
        ))
        .unwrap();
    let session_resp = app.clone().oneshot(create_session).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::OK);
    let body = session_resp.into_body().collect().await.unwrap().to_bytes();
    let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let session_id = session["id"].as_str().unwrap();

    let bind = Request::builder()
        .method("POST")
        .uri(format!("/sessions/{session_id}/spec"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"specId":"{spec_id}"}}"#)))
        .unwrap();
    let bind_resp = app.clone().oneshot(bind).await.unwrap();
    assert_eq!(bind_resp.status(), StatusCode::OK);
    let body = bind_resp.into_body().collect().await.unwrap().to_bytes();
    let bound: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(bound["specId"], spec_id);

    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = list_resp.into_body().collect().await.unwrap().to_bytes();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    let row = rows.iter().find(|row| row["id"] == session_id).unwrap();
    assert_eq!(row["specId"], spec_id);

    let clear = Request::builder()
        .method("DELETE")
        .uri(format!("/sessions/{session_id}/spec"))
        .body(Body::empty())
        .unwrap();
    let clear_resp = app.oneshot(clear).await.unwrap();
    assert_eq!(clear_resp.status(), StatusCode::OK);
    let body = clear_resp.into_body().collect().await.unwrap().to_bytes();
    let cleared: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(cleared["specId"].is_null());
}

#[tokio::test]
async fn bound_spec_blocks_session_done_until_completion_gate_passes() {
    let app = build_router(DaemonState::new(""));
    let create_spec = Request::builder()
        .method("POST")
        .uri("/specs")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support"}"#,
        ))
        .unwrap();
    let spec_resp = app.clone().oneshot(create_spec).await.unwrap();
    assert_eq!(spec_resp.status(), StatusCode::CREATED);
    let body = spec_resp.into_body().collect().await.unwrap().to_bytes();
    let spec: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = spec["id"].as_str().unwrap();

    let create_session = Request::builder()
        .method("POST")
        .uri("/sessions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"workspaceRoot":"D:/work","title":"spec session"}"#,
        ))
        .unwrap();
    let session_resp = app.clone().oneshot(create_session).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::OK);
    let body = session_resp.into_body().collect().await.unwrap().to_bytes();
    let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let session_id = session["id"].as_str().unwrap();

    let bind = Request::builder()
        .method("POST")
        .uri(format!("/sessions/{session_id}/spec"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"specId":"{spec_id}"}}"#)))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(bind).await.unwrap().status(),
        StatusCode::OK
    );

    let completion = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/sessions/{session_id}/spec/completion"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(completion.status(), StatusCode::OK);
    let body = completion.into_body().collect().await.unwrap().to_bytes();
    let completion: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(completion["status"], "block");
    assert_eq!(completion["canComplete"], false);
    assert!(completion["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["kind"] == "requirement_without_task"));

    let mark_done = Request::builder()
        .method("PATCH")
        .uri(format!("/sessions/{session_id}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"status":"done"}"#))
        .unwrap();
    let done_resp = app.oneshot(mark_done).await.unwrap();
    assert_eq!(done_resp.status(), StatusCode::CONFLICT);
}

#[test]
fn should_auto_title_covers_bootstrap_and_default_titles() {
    assert!(should_auto_title(""));
    assert!(should_auto_title("  New Session  "));
    assert!(should_auto_title("Default session"));
    assert!(should_auto_title("default session"));
    assert!(!should_auto_title("Fix Rust guardrails"));
    assert!(should_auto_title_for_workspace(
        "coderight",
        "/Users/example/coderight/"
    ));
    assert!(should_auto_title_for_workspace(
        "RightKit",
        r"C:\\work\\coderight"
    ));
    assert!(!should_auto_title_for_workspace(
        "Fix Rust guardrails",
        "/Users/example/coderight"
    ));
}

#[test]
fn generated_session_title_is_sanitized() {
    assert_eq!(
        sanitize_generated_session_title("\"Title: Fix Rust Guardrails.\"").as_deref(),
        Some("Fix Rust Guardrails")
    );
    assert_eq!(sanitize_generated_session_title("New session"), None);
    assert_eq!(sanitize_generated_session_title("ok"), None);
}

#[test]
fn session_title_prompt_scrubs_system_reminders() {
    let transcript = vec![
        TranscriptMessage::User {
            id: "u".into(),
            created_at: "t".into(),
            content: "Fix tests\n<system-reminder>secret clock</system-reminder>".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        },
        TranscriptMessage::Assistant {
            id: "a".into(),
            created_at: "t".into(),
            content: "I fixed the failing Rust tests.".into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            pinned: false,
        },
    ];

    let prompt = session_title_prompt(&transcript).unwrap();

    assert!(prompt.contains("Fix tests"));
    assert!(prompt.contains("I fixed the failing Rust tests."));
    assert!(!prompt.contains("system-reminder"));
    assert!(!prompt.contains("secret clock"));
}

#[tokio::test]
async fn session_handoff_returns_attach_payload() {
    let state = state_with_session("s1").with_runtime_info(61234, "/tmp/cr.db");
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions/s1/handoff")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["kind"], "coderight.session_handoff.v1");
    assert_eq!(v["session"]["id"], "s1");
    assert_eq!(v["session"]["status"], "running");
    assert_eq!(v["daemon"]["baseUrl"], "http://127.0.0.1:61234");
    assert_eq!(v["daemon"]["tokenHeader"], "Authorization");
    assert_eq!(v["daemon"]["tokenScheme"], "Bearer");
    assert_eq!(v["endpoints"]["transcript"], "/sessions/s1/transcript");
    assert_eq!(v["endpoints"]["eventsSse"], "/sessions/s1/events?after=0");
    assert_eq!(
        v["endpoints"]["eventsHistory"],
        "/sessions/s1/events/history?after=0"
    );
    assert_eq!(v["endpoints"]["messages"], "/sessions/s1/messages");
    assert_eq!(
        v["webUrl"],
        "http://127.0.0.1:61234/?pane=session&session=s1"
    );
}
