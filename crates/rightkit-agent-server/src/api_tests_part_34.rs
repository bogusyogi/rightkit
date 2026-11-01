#[tokio::test(flavor = "multi_thread")]
async fn permission_mode_resumes_a_persisted_session_before_updating_it() {
    let storage = Arc::new(tokio::sync::Mutex::new(
        rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
    ));
    let state = DaemonState::new("").with_storage(storage.clone());
    state.create_session("persisted-permission", None, "/tmp", "Persisted", None);
    wait_for_session(&storage, "persisted-permission").await;
    lock_recover(&state.sessions, "sessions").remove("persisted-permission");
    let inspection = state.clone();
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/persisted-permission/permission-mode")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"mode":"accept-edits"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let resumed = inspection
        .get_session("persisted-permission")
        .expect("permission update should resume the persisted session");
    assert_eq!(live_session_permission_mode(&resumed), "AcceptEdits");
}

#[tokio::test]
async fn permission_policy_mutations_reject_lower_trust_sources() {
    let root = unique_temp_dir("permission-policy-untrusted");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let state = DaemonState::new("")
        .with_permission_rule_store_path(root.join("permission-rules.json"))
        .with_workspace_trust_store_path(root.join("workspace-trust.json"));
    state.create_session("s1", None, &workspace.to_string_lossy(), "Session", None);
    let app = build_router(state);

    let permission_mode = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/permission-mode")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "remoteAgentUntrusted")
                .body(Body::from(r#"{"mode":"bypass"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(permission_mode.status(), StatusCode::FORBIDDEN);

    let session_rule = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/permission-rules")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "toolUntrusted")
                .body(Body::from(r#"{"pattern":"shell","effect":"allow"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_rule.status(), StatusCode::FORBIDDEN);

    let session_rule_ok = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions/s1/permission-rules")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"pattern":"read_file","effect":"deny"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_rule_ok.status(), StatusCode::OK);

    let session_rule_delete = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/sessions/s1/permission-rules/0")
                .header("x-coderight-source-trust", "remoteAgentUntrusted")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_rule_delete.status(), StatusCode::FORBIDDEN);

    let persistent_rule = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/permission-rules")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "remote-agent-untrusted")
                .body(Body::from(
                    r#"{"scope":"user","pattern":"shell","effect":"allow"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(persistent_rule.status(), StatusCode::FORBIDDEN);

    let persistent_rule_ok = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/permission-rules")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"scope":"user","pattern":"write_file","effect":"ask"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(persistent_rule_ok.status(), StatusCode::OK);

    let persistent_rule_delete = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/permission-rules/0?scope=user")
                .header("x-coderight-source-trust", "toolUntrusted")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(persistent_rule_delete.status(), StatusCode::FORBIDDEN);

    let import_rules = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/permission-rules/import")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "untrusted")
                .body(Body::from(r#"{"user":[],"projects":{}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(import_rules.status(), StatusCode::FORBIDDEN);

    let workspace_trust = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/workspace/trust")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "remoteAgentUntrusted")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceRoot": workspace,
                        "trust": "trusted",
                        "approvedBy": "remote-agent"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(workspace_trust.status(), StatusCode::FORBIDDEN);

    let rules = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/sessions/s1/permission-rules")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = rules.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let session_rules = value["rules"].as_array().unwrap();
    assert_eq!(session_rules.len(), 1);
    assert_eq!(session_rules[0]["pattern"], "read_file");

    let trust = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/workspace/trust?workspaceRoot={}",
                    encode_query_value(&workspace.to_string_lossy())
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = trust.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["trust"], "untrusted");
    assert_eq!(value["requiresTrust"], true);

    let persistent_rules = app
        .oneshot(
            Request::builder()
                .uri("/permission-rules?scope=user")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = persistent_rules
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let rules = value["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["pattern"], "write_file");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn persistent_permission_rules_apply_to_new_sessions() {
    let root = unique_temp_dir("permission-rules-persist");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store_path = root.join("permission-rules.json");
    let state = DaemonState::new("").with_permission_rule_store_path(store_path.clone());
    let app = build_router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/permission-rules")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"scope":"user","pattern":"shell","effect":"deny"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/permission-rules")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "scope": "project",
                        "workspaceRoot": workspace,
                        "pattern": "write_file",
                        "effect": "ask"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let reloaded = DaemonState::new("").with_permission_rule_store_path(store_path);
    reloaded.create_session("s1", None, &workspace.to_string_lossy(), "Session", None);
    let app = build_router(reloaded);
    let resp = app
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
    let rules = v["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2);
    assert!(rules.iter().any(|r| r["source"] == "user"));
    assert!(rules.iter().any(|r| r["source"] == "project"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn permission_rules_export_and_import_roundtrip() {
    let root = unique_temp_dir("permission-rules-import");
    let source = DaemonState::new("").with_permission_rule_store_path(root.join("source.json"));
    let app = build_router(source);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/permission-rules")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"scope":"user","pattern":"mcp*","effect":"allow"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/permission-rules/export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();

    let target = DaemonState::new("").with_permission_rule_store_path(root.join("target.json"));
    let target_app = build_router(target);
    let resp = target_app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/permission-rules/import")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = target_app
        .oneshot(
            Request::builder()
                .uri("/permission-rules?scope=user")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["rules"][0]["pattern"], "mcp*");
    assert_eq!(v["rules"][0]["effect"], "allow");

    let _ = std::fs::remove_dir_all(root);
}
