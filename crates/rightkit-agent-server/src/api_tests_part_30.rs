#[test]
fn post_message_accepts_camel_case_attachment_mime_type() {
    let body: PostMessage = serde_json::from_value(serde_json::json!({
        "text": "inspect this",
        "attachments": [
            {
                "type": "image",
                "data": "ZmFrZQ==",
                "mimeType": "image/png"
            }
        ]
    }))
    .unwrap();

    let attachments = body.attachments.unwrap();
    assert_eq!(attachments[0].attachment_type, "image");
    assert_eq!(attachments[0].mime_type, "image/png");
}

#[test]
fn save_settings_accepts_camel_case_tauri_payload() {
    let body: SaveSettings = serde_json::from_value(serde_json::json!({
        "workspaceRoot": "D:/Claude/coderight",
        "primaryOrchestratorModelId": "openrouter/anthropic/claude-sonnet-4",
        "budget": { "maxIterations": 42 },
        "remote": {
            "telegramToken": "bot-token",
            "clearTelegramToken": false,
            "telegramAllowedUsers": [12345, 67890],
            "webEnabled": true
        }
    }))
    .unwrap();

    assert_eq!(body.workspace_root.as_deref(), Some("D:/Claude/coderight"));
    assert_eq!(
        body.primary_orchestrator_model_id.as_deref(),
        Some("openrouter/anthropic/claude-sonnet-4")
    );
    assert_eq!(body.budget.unwrap()["maxIterations"], 42);
    let remote = body.remote.unwrap();
    assert_eq!(remote.telegram_token.as_deref(), Some("bot-token"));
    assert_eq!(remote.clear_telegram_token, Some(false));
    assert_eq!(remote.telegram_allowed_users.unwrap(), vec![12345, 67890]);
    assert_eq!(remote.web_enabled, Some(true));
}

#[tokio::test]
async fn permission_rules_routes_add_list_and_delete_session_rules() {
    let app = build_router(state_with_session("s1"));
    let add = Request::builder()
        .method("POST")
        .uri("/sessions/s1/permission-rules")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "pattern": "shell",
                "effect": "deny"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(add).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["rule"]["index"], 0);
    assert_eq!(v["rule"]["pattern"], "shell");
    assert_eq!(v["rule"]["effect"], "deny");
    assert_eq!(v["rule"]["source"], "session");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sessions/s1/permission-rules")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["rules"].as_array().unwrap().len(), 1);

    let delete = Request::builder()
        .method("DELETE")
        .uri("/sessions/s1/permission-rules/0")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(delete).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions/s1/permission-rules")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["rules"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn workspace_trust_defaults_untrusted_and_session_uses_persisted_choice() {
    let root = unique_temp_dir("workspace-trust");
    let workspace = root.join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace_query = workspace.to_string_lossy().replace('\\', "/");
    let trust_store = root.join("workspace-trust.json");
    let state = DaemonState::new("").with_workspace_trust_store_path(trust_store.clone());
    let app = build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/workspace/trust?workspaceRoot={workspace_query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["trust"], "untrusted");
    assert_eq!(v["trusted"], false);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/workspace/trust")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceRoot": workspace.to_string_lossy(),
                        "trust": "sandboxed",
                        "approvedBy": "test"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(trust_store.exists());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "trusted-session",
                        "workspaceRoot": workspace.to_string_lossy(),
                        "title": "Trust check"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["workspaceTrust"], "sandboxed");

    let live = state.get_session("trusted-session").unwrap();
    assert_eq!(
        live.workspace_trust,
        rightkit_agent_policy::WorkspaceTrust::Sandboxed
    );
}

#[tokio::test]
async fn away_summary_returns_visible_recent_activity_digest() {
    let state = state_with_session("away-session");
    let live = state.get_session("away-session").unwrap();
    {
        let mut transcript = lock_recover(&live.transcript, "session.transcript");
        transcript.push(TranscriptMessage::System {
            id: "sys-1".into(),
            created_at: "2026-06-09T00:00:00Z".into(),
            content: "hidden system setup".into(),
            pinned: false,
        });
        transcript.push(TranscriptMessage::User {
            id: "u-1".into(),
            created_at: "2026-06-09T00:01:00Z".into(),
            content: "please run the audit and fix the risky paths".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        });
        transcript.push(TranscriptMessage::Assistant {
            id: "a-1".into(),
            created_at: "2026-06-09T00:02:00Z".into(),
            content: "Audited the session routes and fixed the risky paths.".into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            pinned: false,
        });
        transcript.push(TranscriptMessage::Tool {
            id: "t-1".into(),
            created_at: "2026-06-09T00:03:00Z".into(),
            tool_call_id: "tool-1".into(),
            summary: "cargo test passed".into(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        });
    }
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions/away-session/away-summary?sinceIndex=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["messageCount"], 3);
    assert_eq!(v["userMessages"], 1);
    assert_eq!(v["assistantMessages"], 1);
    assert_eq!(v["toolResults"], 1);
    assert_eq!(v["hasActivity"], true);
    let bullets = v["bullets"].as_array().unwrap();
    assert_eq!(bullets.len(), 3);
    assert!(!bullets[0].as_str().unwrap().contains("hidden system"));
    assert!(bullets[0]
        .as_str()
        .unwrap()
        .contains("User: please run the audit"));
    assert!(v["latestAt"]
        .as_str()
        .unwrap()
        .contains("2026-06-09T00:03:00Z"));
}
