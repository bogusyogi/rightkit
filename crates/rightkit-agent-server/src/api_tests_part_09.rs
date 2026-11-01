#[tokio::test]
async fn mcp_server_status_reports_readonly_default_and_mutating_opt_in() {
    let app = build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/mcp/server/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["defaultMode"], "read_only");
    assert_eq!(v["mutatingRequiresOptIn"], true);
    assert_eq!(v["readOnlyCommand"], "coderight mcp-server");
    assert_eq!(v["mutatingCommand"], "coderight mcp-server --allow-mutate");
    assert!(v["readOnlyToolCount"].as_u64().unwrap() > 0);
    assert!(v["mutatingToolCount"].as_u64().unwrap() > 0);
    assert!(v["readOnlyTools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool == "read_file"));
    assert!(v["mutatingTools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool == "shell"));
}

#[tokio::test]
async fn mcp_save_and_remove_persist_workspace_server() {
    let root = unique_temp_dir("mcp-save");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let user_config = root.join("user-mcp.json");

    let app = build_router(DaemonState::new("").with_mcp_user_config_path(user_config));
    let save = Request::builder()
        .method("POST")
        .uri("/mcp/servers")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "scope": "workspace",
                "workspaceRoot": workspace,
                "name": "docs",
                "trustPreset": "remote_read_only",
                "approvedBy": "settings-ui",
                "channelPermissions": {
                    "allowedChannels": ["desktop", "cli"],
                    "allowMutating": false
                },
                "config": { "type": "sse", "url": "http://127.0.0.1:9900/sse" }
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(save).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let raw = std::fs::read_to_string(workspace.join(".mcp.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(saved["mcpServers"]["docs"]["type"], "sse");
    assert_eq!(saved["mcpTrust"]["docs"]["preset"], "remote_read_only");
    assert_eq!(saved["mcpTrust"]["docs"]["approvedBy"], "settings-ui");
    assert!(saved["mcpTrust"]["docs"]["approvedAt"].as_str().is_some());
    assert_eq!(
        saved["mcpChannelPermissions"]["docs"]["allowedChannels"],
        serde_json::json!(["cli", "desktop"])
    );
    assert_eq!(
        saved["mcpChannelPermissions"]["docs"]["allowMutating"],
        false
    );

    let delete = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/mcp/servers/docs?scope=workspace&workspaceRoot={}",
            query_encode(&workspace.to_string_lossy())
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(delete).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let raw = std::fs::read_to_string(workspace.join(".mcp.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(saved["mcpServers"].as_object().unwrap().is_empty());
    assert!(saved["mcpTrust"].as_object().unwrap().is_empty());
    assert!(saved["mcpChannelPermissions"]
        .as_object()
        .unwrap()
        .is_empty());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn mcp_probe_http_server_reports_tool_count() {
    async fn mcp_handler(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
        let method = body["method"].as_str().unwrap_or("");
        if method == "initialize" {
            return Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": body["id"].clone(),
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": {"name": "test", "version": "0.1.0"}
                }
            }));
        }
        if method == "notifications/initialized" {
            return Json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {}
            }));
        }
        assert_eq!(method, "tools/list");
        Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": body["id"].clone(),
            "result": {
                "tools": [
                    {
                        "name": "search",
                        "description": "search docs",
                        "inputSchema": { "type": "object" }
                    }
                ]
            }
        }))
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = axum::Router::new().route("/mcp", post(mcp_handler));
    let server_task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });

    let root = unique_temp_dir("mcp-probe");
    let user_config = root.join("user-mcp.json");
    write_mcp_config(
        &user_config,
        serde_json::json!({
            "mcpServers": {
                "docs": {
                    "type": "http",
                    "url": format!("http://{addr}/mcp")
                }
            }
        }),
    );

    let app = build_router(DaemonState::new("").with_mcp_user_config_path(user_config));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/servers/docs/probe?scope=user")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["toolCount"], 1);
    assert_eq!(v["tools"][0]["name"], "search");
    assert!(v["latencyMs"].as_u64().unwrap() < 5_000);

    server_task.abort();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn mcp_probe_unknown_server_returns_not_found() {
    let root = unique_temp_dir("mcp-probe-missing");
    let user_config = root.join("user-mcp.json");
    write_mcp_config(&user_config, serde_json::json!({ "mcpServers": {} }));

    let app = build_router(DaemonState::new("").with_mcp_user_config_path(user_config));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/servers/nope/probe?scope=user")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn mcp_slash_commands_list_tools_resources_and_read_resource() {
    async fn mcp_handler(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
        let result = match body["method"].as_str().unwrap_or_default() {
            "tools/list" => serde_json::json!({
                "tools": [
                    {
                        "name": "search",
                        "description": "search docs",
                        "annotations": {"readOnlyHint": true},
                        "inputSchema": { "type": "object", "properties": { "q": {} }, "required": ["q"] }
                    },
                    {
                        "name": "write",
                        "description": "write docs",
                        "inputSchema": { "type": "object", "properties": { "body": {} } }
                    }
                ]
            }),
            "resources/list" => serde_json::json!({
                "resources": [
                    {
                        "uri": "docs://guide",
                        "name": "Guide",
                        "description": "Getting started",
                        "mimeType": "text/plain"
                    }
                ]
            }),
            "resources/read" => {
                assert_eq!(body["params"]["uri"], "docs://guide");
                serde_json::json!({
                    "contents": [
                        {
                            "uri": "docs://guide",
                            "text": "Guide body",
                            "mimeType": "text/plain"
                        }
                    ]
                })
            }
            other => serde_json::json!({ "error": format!("unexpected method {other}") }),
        };
        Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": body["id"].clone(),
            "result": result
        }))
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = axum::Router::new().route("/mcp", post(mcp_handler));
    let server_task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });

    let root = unique_temp_dir("mcp-command");
    let user_config = root.join("user-mcp.json");
    write_mcp_config(
        &user_config,
        serde_json::json!({
            "mcpServers": {
                "docs": {
                    "type": "http",
                    "url": format!("http://{addr}/mcp")
                }
            },
            "mcpChannelPermissions": {
                "docs": {
                    "allowedChannels": ["desktop"],
                    "allowMutating": false
                }
            }
        }),
    );

    let app = build_router(DaemonState::new("").with_mcp_user_config_path(user_config));
    let tools = execute_slash_command(app.clone(), "/mcp tools").await;
    assert_eq!(tools["success"], true);
    assert!(tools["output"]
        .as_str()
        .unwrap()
        .contains("mcp__docs__search"));
    assert!(!tools["output"]
        .as_str()
        .unwrap_or_default()
        .contains("mcp__docs__write"));
    assert!(tools["output"].as_str().unwrap().contains("required: q"));

    let resources = execute_slash_command(app.clone(), "/mcp resources docs").await;
    assert_eq!(resources["success"], true);
    assert!(resources["output"]
        .as_str()
        .unwrap()
        .contains("docs://guide"));

    let read = execute_slash_command(app, "/mcp read docs docs://guide").await;
    assert_eq!(read["success"], true);
    assert!(read["output"].as_str().unwrap().contains("Guide body"));

    server_task.abort();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn mcp_auth_status_reports_credential_status_without_secret_echo() {
    let result = crate::api_command_mcp::render_mcp_auth_status(
        "docs",
        crate::api_command_mcp::McpAuthCredentialStatus::Missing,
    );

    assert!(result.success);
    let output = result.output;
    assert!(output.contains("MCP auth: docs"));
    assert!(output.contains("mcp-auth"));
    assert!(output.contains("No credential file"));
    assert!(!output.contains("credential\":"));
}

#[tokio::test]
async fn plugins_route_discovers_local_manifests() {
    let plugin_dir = unique_temp_dir("plugins-list");
    write_test_plugin(&plugin_dir, "fixture", true);

    let app = build_router(DaemonState::new("").with_plugin_dir(plugin_dir.clone()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["plugins"].as_array().unwrap().len(), 1);
    assert_eq!(v["plugins"][0]["id"], "fixture");
    assert_eq!(v["plugins"][0]["enabled"], true);
    assert_eq!(v["plugins"][0]["providesTools"][0], "fixture_tool");

    let _ = std::fs::remove_dir_all(plugin_dir);
}
