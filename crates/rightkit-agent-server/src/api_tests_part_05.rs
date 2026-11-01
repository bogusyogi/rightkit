#[tokio::test]
async fn create_session_uses_workspace_execution_backend_default() {
    let root = unique_temp_dir("session-backend-default");
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("execution-backends.json"),
        serde_json::json!({
            "defaultBackend": "local",
            "backendOverrides": [],
            "updatedAt": "2026-06-10T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "s-backend-default",
                        "workspaceRoot": root.to_string_lossy()
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
    assert_eq!(value["executionBackend"], "local");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_session_rejects_disabled_execution_backend_override() {
    let root = unique_temp_dir("session-backend-disabled");
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("execution-backends.json"),
        serde_json::json!({
            "defaultBackend": "local",
            "backendOverrides": [
                {
                    "id": "disabled-backend",
                    "enabled": false
                }
            ],
            "updatedAt": "2026-06-10T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "s-backend-disabled",
                        "workspaceRoot": root.to_string_lossy(),
                        "executionBackend": "missing-backend"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_session_binds_existing_execution_workspace_id() {
    let root = unique_temp_dir("session-execution-workspace");
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("execution-workspaces.json"),
        serde_json::json!([
            {
                "id": "audit-run",
                "backendId": "local",
                "name": "Audit run",
                "status": "ready",
                "paths": {
                    "root": ".coderight/workspaces/audit-run",
                    "uploads": ".coderight/workspaces/audit-run/uploads",
                    "workspace": ".coderight/workspaces/audit-run/workspace",
                    "outputs": ".coderight/workspaces/audit-run/outputs",
                    "logs": ".coderight/workspaces/audit-run/logs",
                    "skills": ".coderight/workspaces/audit-run/skills",
                    "metadata": ".coderight/workspaces/audit-run/metadata.json"
                },
                "createdAt": "2026-06-10T00:00:00Z",
                "updatedAt": "2026-06-10T00:00:00Z"
            }
        ])
        .to_string(),
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
                        "id": "s-execution-workspace",
                        "workspaceRoot": root.to_string_lossy(),
                        "executionBackend": "local",
                        "executionWorkspaceId": "audit-run"
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
    assert_eq!(value["executionWorkspaceId"], "audit-run");
    let live = state
        .get_session("s-execution-workspace")
        .expect("live session should be registered");
    let expected_workspace_path = root
        .join(".coderight")
        .join("workspaces")
        .join("audit-run")
        .join("workspace");
    assert_eq!(
        live.execution_workspace_path.as_deref(),
        Some(expected_workspace_path.as_path())
    );
    assert!(expected_workspace_path.is_dir());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_session_rejects_execution_workspace_backend_mismatch() {
    let root = unique_temp_dir("session-execution-workspace-mismatch");
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("execution-workspaces.json"),
        serde_json::json!([
            {
                "id": "other-run",
                "backendId": "other-backend",
                "name": "Other backend run",
                "status": "ready",
                "paths": {
                    "root": ".coderight/workspaces/other-run",
                    "uploads": ".coderight/workspaces/other-run/uploads",
                    "workspace": ".coderight/workspaces/other-run/workspace",
                    "outputs": ".coderight/workspaces/other-run/outputs",
                    "logs": ".coderight/workspaces/other-run/logs",
                    "skills": ".coderight/workspaces/other-run/skills",
                    "metadata": ".coderight/workspaces/other-run/metadata.json"
                },
                "createdAt": "2026-06-10T00:00:00Z",
                "updatedAt": "2026-06-10T00:00:00Z"
            }
        ])
        .to_string(),
    )
    .unwrap();

    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": "s-execution-workspace-mismatch",
                        "workspaceRoot": root.to_string_lossy(),
                        "executionBackend": "local",
                        "executionWorkspaceId": "other-run"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_session_preserves_execution_workspace_id() {
    let root = unique_temp_dir("session-execution-workspace-resume");
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    let live = state.create_session_with_options(
        "s-execution-workspace-resume",
        None,
        root.to_str().unwrap(),
        "Resume workspace",
        None,
        SessionRuntimeOptions {
            execution_backend: Some("local".to_string()),
            execution_workspace_id: Some("audit-run".to_string()),
            ..SessionRuntimeOptions::default()
        },
    );
    assert_eq!(live.execution_workspace_id.as_deref(), Some("audit-run"));
    wait_for_session(&storage, "s-execution-workspace-resume").await;
    lock_recover(&state.sessions, "sessions").remove("s-execution-workspace-resume");

    let resumed = state
        .resume_session("s-execution-workspace-resume")
        .await
        .expect("session should resume");
    assert_eq!(resumed.execution_workspace_id.as_deref(), Some("audit-run"));
    let row = session_row(&resumed);
    assert_eq!(row["executionWorkspaceId"], "audit-run");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_session_resolves_backend_network_profile_override() {
    let root = unique_temp_dir("session-backend-network-profile");
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("execution-backends.json"),
        serde_json::json!({
            "defaultBackend": "local",
            "backendOverrides": [
                {
                    "id": "local",
                    "networkProfile": "egress-restricted"
                }
            ],
            "updatedAt": now_iso()
        })
        .to_string(),
    )
    .unwrap();
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());

    let live = state.create_session_with_options(
        "s-network-profile",
        None,
        &root.to_string_lossy(),
        "Network profile",
        None,
        SessionRuntimeOptions {
            execution_backend: Some("local".to_string()),
            ..SessionRuntimeOptions::default()
        },
    );

    assert_eq!(
        live.execution_network_profile.as_deref(),
        Some("egress-restricted")
    );
    let row = wait_for_session(&storage, "s-network-profile").await;
    let config: serde_json::Value = serde_json::from_str(&row.config_json).unwrap();
    assert_eq!(config["executionNetworkProfile"], "egress-restricted");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_session_rejects_unknown_toolset_profile() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id":"s-bad-toolset","toolset":"nope"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
