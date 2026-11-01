#[test]
fn record_message_history_recovers_from_poisoned_mutex() {
    let root = unique_temp_dir("message-history-poison");
    let state = DaemonState::new("").with_message_store_path(root.join("messages.json"));
    let message_history = state.message_history.clone();
    let _ = std::thread::spawn(move || {
        let mut history = lock_recover(&message_history, "message_history");
        history.push(MessageHistoryRow {
            id: "seed-message".to_string(),
            created_at: "2026-06-28T00:00:00Z".to_string(),
            from_session_id: "seed".to_string(),
            target_session_id: None,
            channel: "seed".to_string(),
            message: "seed".to_string(),
            delivery_status: "seeded".to_string(),
            delivered_session_ids: Vec::new(),
            error: None,
        });
        panic!("poison message history");
    })
    .join();

    state
        .record_message_history(
            rightkit_agent_tools::coding_tools::MessageDispatch {
                from_session_id: "s1".to_string(),
                target_session_id: Some("s2".to_string()),
                channel: "handoff".to_string(),
                message: "recover history".to_string(),
            },
            "delivered",
            vec!["s2".to_string()],
            None,
        )
        .expect("message history write should recover");

    let reloaded = DaemonState::new("").with_message_store_path(root.join("messages.json"));
    let history = lock_recover(&reloaded.message_history, "message_history");
    assert_eq!(history.len(), 2);
    assert!(history
        .iter()
        .any(|row| row.message == "recover history" && row.delivery_status == "delivered"));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn record_permission_denial_recovers_from_poisoned_mutex() {
    let root = unique_temp_dir("permission-denials-poison");
    let state = DaemonState::new("").with_permission_denial_store_path(root.join("denials.json"));
    let permission_denials = state.permission_denials.clone();
    let _ = std::thread::spawn(move || {
        let mut denials = lock_recover(&permission_denials, "permission_denials");
        denials.push(PermissionDenialRow {
            id: "seed-denial".to_string(),
            created_at: "2026-06-28T00:00:00Z".to_string(),
            session_id: "seed".to_string(),
            tool_call_id: "seed-call".to_string(),
            correlation: "corr-seed".to_string(),
            reason: "permission denied: seeded".to_string(),
        });
        panic!("poison permission denials");
    })
    .join();

    let result = TranscriptMessage::Tool {
        id: "tr1".to_string(),
        created_at: "2026-06-28T00:00:01Z".to_string(),
        tool_call_id: "tc1".to_string(),
        summary: "permission denied: denied by test".to_string(),
        output: None,
        trust: None,
        is_error: true,
        process_id: None,
        pinned: false,
    };

    state
        .record_permission_denial_from_tool_result("s1", "2026-06-28T00:00:01Z", &result)
        .expect("permission-denial write should recover");

    let reloaded =
        DaemonState::new("").with_permission_denial_store_path(root.join("denials.json"));
    let denials = lock_recover(&reloaded.permission_denials, "permission_denials");
    assert_eq!(denials.len(), 2);
    assert!(denials
        .iter()
        .any(|row| row.session_id == "s1" && row.tool_call_id == "tc1"));
    assert!(denials.iter().any(|row| {
        row.session_id == "s1"
            && row.correlation == rightkit_agent_policy::denial_tracking::correlation_token("s1", "tc1")
    }));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn live_team_directory_registers_and_deletes_definitions() {
    let root = unique_temp_dir("teams-live");
    let state = DaemonState::new("").with_team_store_path(root.join("teams.json"));

    let summary = rightkit_agent_tools::coding_tools::TeamDirectory::create_team(
        &state,
        rightkit_agent_tools::coding_tools::TeamDefinition {
            name: "review-squad".to_string(),
            description: Some("review changes".to_string()),
            members: vec!["architect".to_string(), "tester".to_string()],
            max_workers: None,
            budget_usd: None,
        },
    )
    .await
    .unwrap();

    assert!(summary.contains("review-squad"));
    let team = state.live_team("review-squad").expect("team registered");
    assert_eq!(team.members, vec!["architect", "tester"]);

    let summary = rightkit_agent_tools::coding_tools::TeamDirectory::delete_team(&state, "review-squad".to_string())
        .await
        .unwrap();
    assert!(summary.contains("review-squad"));
    assert!(state.live_team("review-squad").is_none());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn live_team_directory_persists_definitions_across_state_instances() {
    let root = unique_temp_dir("teams-persist");
    let team_store = root.join("teams.json");
    let state = DaemonState::new("").with_team_store_path(team_store.clone());

    rightkit_agent_tools::coding_tools::TeamDirectory::create_team(
        &state,
        rightkit_agent_tools::coding_tools::TeamDefinition {
            name: "review-squad".to_string(),
            description: Some("review changes".to_string()),
            members: vec!["architect".to_string(), "tester".to_string()],
            max_workers: Some(2),
            budget_usd: Some(1.25),
        },
    )
    .await
    .unwrap();

    let reloaded = DaemonState::new("").with_team_store_path(team_store);
    let team = reloaded.live_team("review-squad").expect("team reloaded");
    assert_eq!(team.description.as_deref(), Some("review changes"));
    assert_eq!(team.members, vec!["architect", "tester"]);
    assert_eq!(team.max_workers, Some(2));
    assert_eq!(team.budget_usd, Some(1.25));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn teams_routes_list_and_delete_persisted_definitions() {
    let root = unique_temp_dir("teams-routes");
    let state = DaemonState::new("").with_team_store_path(root.join("teams.json"));
    rightkit_agent_tools::coding_tools::TeamDirectory::create_team(
        &state,
        rightkit_agent_tools::coding_tools::TeamDefinition {
            name: "review-squad".to_string(),
            description: Some("review changes".to_string()),
            members: vec!["architect".to_string(), "tester".to_string()],
            max_workers: Some(2),
            budget_usd: Some(1.25),
        },
    )
    .await
    .unwrap();
    let app = build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/teams")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["teams"].as_array().unwrap().len(), 1);
    assert_eq!(v["teams"][0]["name"], "review-squad");
    assert_eq!(v["teams"][0]["maxWorkers"], 2);
    assert_eq!(v["teams"][0]["budgetUsd"], 1.25);

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/teams/review-squad")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.live_team("review-squad").is_none());

    let reloaded = DaemonState::new("").with_team_store_path(root.join("teams.json"));
    assert!(reloaded.live_team("review-squad").is_none());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn messages_route_lists_inter_session_history() {
    let root = unique_temp_dir("messages-route");
    let state = DaemonState::new("").with_message_store_path(root.join("messages.json"));
    state.create_session("s1", None, "/tmp", "Sender", None);
    state.create_session("s2", None, "/tmp", "Target", None);
    rightkit_agent_tools::coding_tools::MessageDispatcher::dispatch(
        &state,
        rightkit_agent_tools::coding_tools::MessageDispatch {
            from_session_id: "s1".to_string(),
            target_session_id: Some("s2".to_string()),
            channel: "handoff".to_string(),
            message: "please inspect the diff".to_string(),
        },
    )
    .await
    .unwrap();

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/messages")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["messageStorePath"]
        .as_str()
        .unwrap()
        .ends_with("messages.json"));
    assert_eq!(v["messages"].as_array().unwrap().len(), 1);
    assert_eq!(v["messages"][0]["fromSessionId"], "s1");
    assert_eq!(v["messages"][0]["targetSessionId"], "s2");
    assert_eq!(v["messages"][0]["deliveryStatus"], "delivered");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn mcp_route_lists_user_and_workspace_servers() {
    let root = unique_temp_dir("mcp-list");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let user_config = root.join("user-mcp.json");
    write_mcp_config(
        &user_config,
        serde_json::json!({
            "mcpServers": {
                "user-http": { "type": "http", "url": "http://127.0.0.1:9555/mcp" }
            },
            "mcpTrust": {
                "user-http": {
                    "preset": "remote_read_only",
                    "approvedBy": "test",
                    "approvedAt": "2026-06-06T00:00:00Z"
                }
            },
            "mcpChannelPermissions": {
                "user-http": {
                    "allowedChannels": ["desktop", "telegram"],
                    "allowMutating": false
                }
            }
        }),
    );
    write_mcp_config(
        &workspace.join(".mcp.json"),
        serde_json::json!({
            "mcpServers": {
                "workspace-stdio": { "type": "stdio", "command": "node", "args": ["server.js"] }
            }
        }),
    );

    let app = build_router(DaemonState::new("").with_mcp_user_config_path(user_config));
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/mcp?workspaceRoot={}",
                    query_encode(&workspace.to_string_lossy())
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let servers = v["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 2);
    assert!(v["trustPresets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|preset| preset["id"] == "remote_unreviewed"));
    let user_http = servers
        .iter()
        .find(|s| s["name"] == "user-http" && s["scope"] == "user")
        .unwrap();
    assert_eq!(user_http["trust"]["preset"], "remote_read_only");
    assert_eq!(user_http["trust"]["inferred"], false);
    assert_eq!(
        user_http["channelPermissions"]["allowedChannels"],
        serde_json::json!(["desktop", "telegram"])
    );
    assert_eq!(user_http["channelPermissions"]["allowMutating"], false);
    assert!(user_http["policyDiagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| diagnostic["code"] == "mcp_mutating_requires_read_only_hint"));
    let workspace_stdio = servers
        .iter()
        .find(|s| s["name"] == "workspace-stdio" && s["scope"] == "workspace")
        .unwrap();
    assert_eq!(workspace_stdio["trust"]["preset"], "local_process");
    assert_eq!(workspace_stdio["trust"]["inferred"], true);
    assert!(workspace_stdio["channelPermissions"].is_null());
    assert!(workspace_stdio["policyDiagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| diagnostic["code"] == "mcp_channel_permissions_unscoped"));

    let _ = std::fs::remove_dir_all(root);
}
