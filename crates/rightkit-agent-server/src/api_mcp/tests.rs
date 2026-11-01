mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    fn temp_mcp_config_path(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "coderight-mcp-trust-gate-{label}-{}",
            crate::api::now_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.join("mcp.json")
    }

    // ── ra-001: ad hoc workspace MCP transport construction must route
    // through the same persisted first-use trust gate the canonical session
    // loader applies (crates/mcp/src/config.rs::load_with_trust_gate_pooled_impl).
    // `mcp_transport_from_config` (config_trust.rs) is the single choke point
    // both the management probe handler (channel_probe.rs) and the
    // slash-command paths (api_command_mcp.rs) now call through.
    #[tokio::test]
    async fn mcp_transport_from_config_gates_workspace_stdio_by_trust_decision() {
        // Share the lock with provider-key tests that also change HOME.
        let _env_guard = crate::test_support::process_env_lock();

        let home = std::env::temp_dir().join(format!(
            "coderight-mcp-transport-trust-{}",
            crate::api::now_millis()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        let prev_coderight_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&home);
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);

        let workspace_root = home.join("workspace").to_string_lossy().into_owned();
        let stdio = rightkit_mcp::agent_mcp::StdioConfig {
            command: "coderight-does-not-exist-binary".to_string(),
            args: vec!["-y".to_string()],
            env: std::collections::HashMap::new(),
        };
        let config = rightkit_mcp::agent_mcp::McpServerConfig::Stdio(stdio.clone());
        let spec = rightkit_mcp::agent_mcp::stdio_server_spec(&stdio);
        let store_path = rightkit_mcp::agent_mcp::default_trust_store_path(Some(home.as_path()));

        // 1. No trust record yet -> Pending -> a non-spawning stub, not the
        //    workspace-declared command.
        let transport = mcp_transport_from_config("workspace", &workspace_root, "evil", &config);
        let tools = transport
            .list_tools()
            .await
            .expect("pending gate must yield a stub, not attempt a real spawn");
        assert!(
            tools.is_empty(),
            "pending workspace stdio server must not spawn"
        );

        // 1b. A203: per-server approval alone is not enough. While the
        //     workspace itself is untrusted, even an MCP-approved server (and
        //     any HTTP server) stays a non-spawning stub.
        let mut store = rightkit_mcp::agent_mcp::TrustStore::new();
        store.approve(&workspace_root, "evil", &spec);
        store.save(&store_path).unwrap();
        let transport = mcp_transport_from_config("workspace", &workspace_root, "evil", &config);
        assert!(
            transport
                .list_tools()
                .await
                .expect("untrusted workspace must yield a stub")
                .is_empty(),
            "approved stdio server in an untrusted workspace must not spawn"
        );
        let http = rightkit_mcp::agent_mcp::McpServerConfig::Http(rightkit_mcp::agent_mcp::HttpConfig {
            url: "http://evil-host.invalid/mcp".to_string(),
        });
        let transport = mcp_transport_from_config("workspace", &workspace_root, "web", &http);
        assert!(
            transport
                .list_tools()
                .await
                .expect("untrusted workspace must yield a stub")
                .is_empty(),
            "workspace HTTP server in an untrusted workspace must not be contacted"
        );
        // Trust the workspace the way `coderight trust` does (persisted store).
        let ws_trust_store = rightkit_app_settings::agent_config::workspace_trust_store_path();
        std::fs::create_dir_all(ws_trust_store.parent().unwrap()).unwrap();
        std::fs::write(
            &ws_trust_store,
            serde_json::json!({"projects": {
                rightkit_app_settings::agent_config::workspace_trust_key(&workspace_root): {
                    "trust": "trusted", "approvedBy": "user", "updatedAt": "t"
                }
            }})
            .to_string(),
        )
        .unwrap();

        // 2. Approved -> a real endpoint, never an empty-success stub.
        //    Discovery fails at host admission or at the deliberately
        //    nonexistent binary; approval does not bypass process authority.
        let mut store = rightkit_mcp::agent_mcp::TrustStore::new();
        store.approve(&workspace_root, "evil", &spec);
        store.save(&store_path).unwrap();
        let transport = mcp_transport_from_config("workspace", &workspace_root, "evil", &config);
        assert_eq!(
            transport.endpoint_identity(),
            Some(format!("stdio://{} {:?}", stdio.command, stdio.args)),
            "approved workspace stdio server retains its configured endpoint"
        );
        let result = transport.list_tools().await;
        assert!(
            result.is_err(),
            "approved workspace stdio server must construct a real transport"
        );

        // 3. Denied -> back to a non-spawning stub.
        let mut store = rightkit_mcp::agent_mcp::load_trust_store(&store_path);
        store.deny(&workspace_root, "evil", &spec);
        store.save(&store_path).unwrap();
        let transport = mcp_transport_from_config("workspace", &workspace_root, "evil", &config);
        let tools = transport
            .list_tools()
            .await
            .expect("denied gate must yield a stub, not attempt a real spawn");
        assert!(
            tools.is_empty(),
            "denied workspace stdio server must not spawn"
        );

        // User-scoped (non-workspace) stdio servers are exempt from the gate,
        // matching the canonical loader's exemption for global/default config.
        let user_transport = mcp_transport_from_config("user", "", "evil", &config);
        assert!(
            user_transport.list_tools().await.is_err(),
            "user-scoped stdio servers bypass the workspace trust gate entirely"
        );

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_coderight_home {
            Some(v) => std::env::set_var("RIGHTKIT_SUITE_ROOT", v),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }
        match prev_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn mcp_server_row_matches_fixture_contract() {
        let mut env = std::collections::HashMap::new();
        env.insert("MODE".to_string(), "test".to_string());
        let config = rightkit_mcp::agent_mcp::McpServerConfig::Stdio(rightkit_mcp::agent_mcp::StdioConfig {
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            env,
        });
        let trust = rightkit_mcp::agent_mcp::McpTrustPolicy {
            preset: "local_process".to_string(),
            approved_by: Some("tester".to_string()),
            approved_at: Some("2026-06-24T01:02:03Z".to_string()),
        };
        let permissions = rightkit_mcp::agent_mcp::McpChannelPermissions {
            allowed_channels: vec!["cli".to_string(), "desktop".to_string()],
            allow_mutating: false,
        };
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/cli-json/mcp-server-row.v1.json"
        ))
        .expect("parse mcp server row fixture");

        assert_eq!(
            mcp_server_json(
                "workspace",
                "docs",
                &config,
                Some(&trust),
                Some(&permissions)
            ),
            fixture
        );
    }

    async fn lower_trust_mcp_request_status(
        app: axum::Router,
        method: Method,
        uri: &str,
        body: serde_json::Value,
    ) -> axum::http::StatusCode {
        app.oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "toolUntrusted")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    #[tokio::test]
    async fn mcp_governance_routes_reject_lower_trust_sources() {
        let config_path = temp_mcp_config_path("routes");
        let app = crate::api::build_router(
            DaemonState::new("").with_mcp_user_config_path(config_path.clone()),
        );

        let save_body = serde_json::json!({
            "scope": "user",
            "name": "blocked",
            "config": {
                "type": "stdio",
                "command": "coderight-blocked-mcp"
            }
        });
        let cases = [
            (Method::POST, "/mcp/servers", save_body),
            (
                Method::DELETE,
                "/mcp/servers/blocked",
                serde_json::json!({}),
            ),
            (
                Method::POST,
                "/mcp/servers/blocked/probe",
                serde_json::json!({}),
            ),
        ];

        for (method, uri, body) in cases {
            let status = lower_trust_mcp_request_status(app.clone(), method, uri, body).await;
            assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{uri}");
        }
        assert!(
            !config_path.exists(),
            "lower-trust MCP management must fail before writing config"
        );

        if let Some(root) = config_path.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    fn write_mcp_config_json(path: &std::path::Path, value: &serde_json::Value) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn mcp_login_no_auth_returns_ok_message() {
        let config_path = temp_mcp_config_path("login-noauth");
        write_mcp_config_json(
            &config_path,
            &serde_json::json!({
                "mcpServers": {
                    "my-server": {
                        "type": "stdio",
                        "command": "node",
                        "args": ["server.js"],
                    }
                }
            }),
        );
        let app = crate::api::build_router(
            DaemonState::new("").with_mcp_user_config_path(config_path.clone()),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp/servers/my-server/login")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);

        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["ok"], true);
        assert!(
            body["message"]
                .as_str()
                .unwrap_or("")
                .contains("does not require login"),
            "expected 'does not require login' message, got: {:?}",
            body["message"]
        );

        if let Some(root) = config_path.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn mcp_login_with_oauth_takes_oauth_path() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let config_path = temp_mcp_config_path("login-oauth");
            write_mcp_config_json(
                &config_path,
                &serde_json::json!({
                    "mcpServers": {
                        "oauth-server": {
                            "type": "stdio",
                            "command": "node",
                            "args": ["server.js"],
                        }
                    },
                    "mcpOAuth": {
                        "oauth-server": {
                            "authorizationUrl": "https://auth.example.com/authorize",
                            "tokenUrl": "https://auth.example.com/token",
                            "resource": "https://api.example.com",
                            "clientId": "coderight"
                        }
                    }
                }),
            );
            let app = crate::api::build_router(
                DaemonState::new("").with_mcp_user_config_path(config_path.clone()),
            );

            // The handler calls login_oauth() headless by default (no browser
            // spawn -- openBrowser=true is an explicit opt-in for interactive
            // surfaces); against the fixture's unreachable auth server it returns
            // an error response. We assert it does NOT return the "does not
            // require login" no-auth fallback, proving the OAuth branch was taken.
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                app.oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/mcp/servers/oauth-server/login")
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                ),
            )
            .await;

            match result {
                Ok(Ok(response)) => {
                    let body: serde_json::Value = serde_json::from_slice(
                        &axum::body::to_bytes(response.into_body(), 8192)
                            .await
                            .unwrap(),
                    )
                    .unwrap();
                    let msg = body["message"].as_str().unwrap_or("");
                    let err = body["error"].as_str().unwrap_or("");
                    assert!(
                        !msg.contains("does not require login")
                            && !err.contains("does not require login"),
                        "OAuth server should NOT get the no-auth response"
                    );
                    if body["ok"] == false {
                        assert!(
                            err.contains("login failed") || err.contains("failed to open browser"),
                            "expected login-failure error, got: {err}"
                        );
                    }
                }
                Ok(Err(e)) => {
                    panic!("OAuth login request failed: {e}");
                }
                Err(_elapsed) => {
                    // Timeout means the handler is stuck waiting for an OAuth
                    // callback -- which still proves it took the OAuth branch
                    // (the no-auth path returns instantly).
                }
            }

            if let Some(root) = config_path.parent() {
                let _ = std::fs::remove_dir_all(root);
            }
        });

        // Abandon runtime to avoid blocking on orphaned spawn_blocking task
        // (the OAuth loopback listener blocks on accept() until a callback arrives).
        std::mem::forget(rt);
    }
}
