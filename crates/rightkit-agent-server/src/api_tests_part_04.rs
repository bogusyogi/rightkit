#[test]
fn session_openapi_helpers_cover_daemon_session_routes() {
    for (method, path) in [
        ("post", "/sessions"),
        ("patch", "/sessions/{id}"),
        ("post", "/sessions/{id}/spec"),
        ("post", "/sessions/{id}/messages"),
        ("post", "/sessions/{id}/compact"),
        ("post", "/sessions/{id}/tool-approval"),
        ("post", "/sessions/{id}/answer-question"),
        ("post", "/sessions/{id}/resolve-plan"),
        ("post", "/sessions/{id}/steer"),
        ("post", "/sessions/{id}/goal"),
        ("post", "/sessions/{id}/goal/edit"),
        ("post", "/sessions/{id}/orchestrate"),
    ] {
        assert!(
            session_openapi_request_schema(method, path).is_some(),
            "missing request schema for {method} {path}"
        );
    }

    for (method, path) in [
        ("get", "/sessions"),
        ("post", "/sessions"),
        ("get", "/sessions/search"),
        ("delete", "/sessions/{id}"),
        ("patch", "/sessions/{id}"),
        ("post", "/sessions/{id}/spec"),
        ("delete", "/sessions/{id}/spec"),
        ("get", "/sessions/{id}/spec/completion"),
        ("get", "/sessions/{id}/transcript"),
        ("get", "/sessions/{id}/untrusted-content"),
        ("get", "/sessions/{id}/away-summary"),
        ("post", "/sessions/{id}/messages"),
        ("get", "/sessions/{id}/events"),
        ("get", "/sessions/{id}/events/history"),
        ("post", "/sessions/{id}/compact"),
        ("post", "/sessions/{id}/tool-approval"),
        ("post", "/sessions/{id}/answer-question"),
        ("post", "/sessions/{id}/resolve-plan"),
        ("post", "/sessions/{id}/resume"),
        ("post", "/sessions/{id}/interrupt"),
        ("post", "/sessions/{id}/steer"),
        ("post", "/sessions/{id}/goal"),
        ("delete", "/sessions/{id}/goal"),
        ("post", "/sessions/{id}/goal/pause"),
        ("post", "/sessions/{id}/goal/resume"),
        ("post", "/sessions/{id}/goal/edit"),
        ("post", "/sessions/{id}/rewind"),
        ("post", "/sessions/{id}/orchestrate"),
        ("get", "/sessions/{id}/runtime/artifacts"),
        ("get", "/sessions/{id}/runtime/taskgraph"),
        ("get", "/instructions"),
        ("get", "/tasks"),
    ] {
        assert!(
            session_openapi_response_schema(method, path).is_some(),
            "missing response schema for {method} {path}"
        );
    }
}

async fn fetch_transcript_json(app: &Router, session_id: &str) -> Vec<serde_json::Value> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/sessions/{session_id}/transcript"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

async fn wait_for_transcript<F>(
    app: &Router,
    session_id: &str,
    matches: F,
) -> Vec<serde_json::Value>
where
    F: Fn(&[serde_json::Value]) -> bool,
{
    for _ in 0..20 {
        let messages = fetch_transcript_json(app, session_id).await;
        if matches(&messages) {
            return messages;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    fetch_transcript_json(app, session_id).await
}

#[test]
fn create_session_request_accepts_agent_runtime_controls() {
    let body: CreateSession = serde_json::from_str(
        r#"{
                "id": "s-runtime",
                "allowedTools": ["read_file", "shell"],
                "disallowedTools": ["shell"],
                "toolset": "dev-safe",
                "executionWorkspaceId": "audit-run",
                "mcpChannel": "CLI",
                "effort": "high",
                "thinkingBudgetTokens": 2048,
                "disableThinking": false
            }"#,
    )
    .unwrap();

    assert_eq!(body.allowed_tools, vec!["read_file", "shell"]);
    assert_eq!(body.disallowed_tools, vec!["shell"]);
    assert_eq!(body.toolset.as_deref(), Some("dev-safe"));
    assert_eq!(body.execution_workspace_id.as_deref(), Some("audit-run"));
    assert_eq!(body.mcp_channel.as_deref(), Some("CLI"));
    assert_eq!(body.effort.as_deref(), Some("high"));
    assert_eq!(body.thinking_budget_tokens, Some(2048));
    assert_eq!(body.disable_thinking, Some(false));
}

#[test]
fn apply_toolset_profile_uses_profile_as_capability_ceiling() {
    let profile = resolve_toolset_profile("dev-safe").unwrap();
    let (allowed, disallowed) = apply_toolset_profile(
        Some(&profile),
        vec!["read_file".to_string(), "shell".to_string()],
        vec!["write_file".to_string()],
    );

    assert_eq!(allowed, vec!["read_file"]);
    assert!(disallowed.contains(&"shell".to_string()));
    assert!(disallowed.contains(&"write_file".to_string()));
}

#[test]
fn effective_tool_filter_intersects_role_ceiling_and_session_request() {
    let selected = effective_tool_filter(
        Some(&vec!["read_file".to_string(), "shell".to_string()]),
        &["read_file".to_string(), "web_search".to_string()],
        &["shell".to_string()],
    )
    .expect("expected filtered tool list");

    assert_eq!(selected, vec!["read_file"]);
}

#[test]
fn normalize_effort_rejects_unknown_values() {
    assert_eq!(
        normalize_effort(Some("MEDIUM".into())).unwrap().as_deref(),
        Some("medium")
    );
    assert!(normalize_effort(Some("maximum".into())).is_err());
}

#[tokio::test]
async fn create_session_response_surfaces_runtime_controls() {
    let state = DaemonState::new("");
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{
                            "id": "s-runtime-controls",
                            "allowedTools": ["read_file", "shell"],
                            "disallowedTools": ["shell"],
                            "mcpChannel": "CLI",
                            "effort": "HIGH",
                            "thinkingBudgetTokens": 4096
                        }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["allowedTools"][0], "read_file");
    assert_eq!(value["allowedTools"][1], "shell");
    assert_eq!(value["disallowedTools"][0], "shell");
    assert_eq!(value["mcpChannel"], "cli");
    assert_eq!(value["effort"], "high");
    assert_eq!(value["thinkingBudgetTokens"], 4096);
    assert_eq!(value["disableThinking"], false);
}

#[tokio::test]
async fn create_session_applies_named_toolset_profile() {
    let state = DaemonState::new("");
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{
                            "id": "s-toolset",
                            "toolset": "read-only",
                            "allowedTools": ["read_file", "shell"]
                        }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["toolset"], "read-only");
    assert_eq!(value["allowedTools"], serde_json::json!(["read_file"]));
    assert!(value["disallowedTools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool.as_str() == Some("shell")));
}

#[tokio::test]
async fn create_session_applies_workspace_configured_toolset_profile() {
    let root = unique_temp_dir("session-configured-toolset");
    std::fs::create_dir_all(root.join(".coderight")).unwrap();
    std::fs::write(
        root.join(".coderight").join("config.toml"),
        r#"
[toolsets.remote-safe]
name = "Remote safe"
description = "Workspace remote-safe profile"
allowed-tools = ["read_file", "shell"]
disallowed-tools = ["shell"]
read-only = true
default-behavioral-skills = ["cr-baseline-behavior"]
"#,
    )
    .unwrap();

    let state = DaemonState::new("");
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "s-config-toolset",
                        "workspaceRoot": root.to_string_lossy(),
                        "toolset": "remote-safe",
                        "allowedTools": ["read_file", "shell"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["toolset"], "remote-safe");
    assert_eq!(value["allowedTools"], serde_json::json!(["read_file"]));
    assert!(value["disallowedTools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool.as_str() == Some("shell")));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_session_validates_and_records_execution_backend() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{
                            "id": "s-backend",
                            "executionBackend": "local"
                        }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["executionBackend"], "local");

    let bad = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{
                            "id": "s-backend-bad",
                            "executionBackend": "missing-backend"
                        }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}
