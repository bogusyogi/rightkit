async fn cli_info(json: bool) -> i32 {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let mut body = serde_json::json!({
        "schema_version": crate::host_identity::schema("cli.info.v1"),
        "version": env!("CARGO_PKG_VERSION"),
        "cwd": cwd,
        "daemon": {
            "running": false,
        },
    });

    if let Some((base_url, token)) = resolve_daemon() {
        body["daemon"] = serde_json::json!({
            "detected": true,
            "running": false,
            "baseUrl": base_url,
        });
        match rightkit_http::http_client::daemon_get(&format!("{base_url}/health"), &token).await {
            Ok(resp) => {
                let status = resp.status;
                body["daemon"]["healthStatus"] = serde_json::json!(status);
                body["daemon"]["running"] =
                    serde_json::json!(rightkit_http::http_client::is_success(status));
                if rightkit_http::http_client::is_success(status) {
                    if let Ok(health) = resp.json::<serde_json::Value>() {
                        body["daemon"]["health"] = health;
                    }
                }
            }
            Err(e) => {
                body["daemon"]["error"] = serde_json::json!(e);
            }
        }
    }

    if json {
        print_json_value(body);
    } else {
        println!("RightKit {}", env!("CARGO_PKG_VERSION"));
        println!("cwd: {}", body["cwd"].as_str().unwrap_or("."));
        if body["daemon"]["running"].as_bool().unwrap_or(false) {
            println!(
                "daemon: running at {}",
                body["daemon"]["baseUrl"].as_str().unwrap_or("unknown")
            );
            if let Some(status) = body["daemon"]["healthStatus"].as_u64() {
                println!("health: HTTP {status}");
            }
        } else {
            println!("daemon: not running");
        }
    }
    0
}

// ── Pure helpers (extracted for testability) ──────────────────────────────────

fn mcp_test_probe_format(name: &str, value: &serde_json::Value) -> (String, i32) {
    let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let tool_count = value.get("toolCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let latency = value.get("latencyMs").and_then(|v| v.as_i64()).unwrap_or(0);
    if ok {
        (
            format!("PASS | {name} | {tool_count} tool(s) | {latency}ms"),
            0,
        )
    } else {
        let error = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        (format!("FAIL | {name} | {error}"), 1)
    }
}

fn mcp_login_result(value: &serde_json::Value) -> (String, i32) {
    let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("");
    if ok {
        (message.to_string(), 0)
    } else {
        let error = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        (error.to_string(), 1)
    }
}

async fn cli_app(command: AppCommand) -> i32 {
    match command {
        AppCommand::Handoff { session_id } => daemon_handoff(&session_id).await,
        AppCommand::Open { session_id } => daemon_app_open(&session_id).await,
    }
}

async fn cli_mcp(command: McpCommand) -> i32 {
    match command {
        McpCommand::List {
            workspace_root,
            json: _json,
        } => {
            let path = format!(
                "/mcp{}",
                mcp_management_query(None, workspace_root.as_deref())
            );
            daemon_get_pretty_json(&path, "mcp servers").await
        }
        McpCommand::Status => daemon_get_pretty_json("/mcp/server/status", "mcp status").await,
        McpCommand::Get {
            name,
            workspace_root,
        } => daemon_mcp_get(&name, workspace_root.as_deref()).await,
        McpCommand::AddJson { path } => match read_json_file(&path) {
            Ok(body) => daemon_post_pretty_json("/mcp/servers", body, "mcp add").await,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        McpCommand::Remove {
            name,
            scope,
            workspace_root,
        } => {
            let query = mcp_management_query(Some(&scope), workspace_root.as_deref());
            daemon_delete_pretty_json(
                &format!("/mcp/servers/{}{}", url_encode_component(&name), query),
                "mcp remove",
            )
            .await
        }
        McpCommand::Probe {
            name,
            scope,
            workspace_root,
        } => {
            let query = mcp_management_query(Some(&scope), workspace_root.as_deref());
            daemon_post_pretty_json(
                &format!("/mcp/servers/{}/probe{query}", url_encode_component(&name)),
                serde_json::json!({}),
                "mcp probe",
            )
            .await
        }
        McpCommand::Test {
            name,
            scope,
            workspace_root,
            json,
        } => {
            let query = mcp_management_query(Some(&scope), workspace_root.as_deref());
            let result = daemon_post_raw(
                &format!("/mcp/servers/{}/probe{query}", url_encode_component(&name)),
                serde_json::json!({}),
            )
            .await;
            match result {
                Ok(value) if json => {
                    print_json_value(value);
                    0
                }
                Ok(value) => {
                    let (line, code) = mcp_test_probe_format(&name, &value);
                    println!("{line}");
                    code
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        McpCommand::Login {
            name,
            scope,
            workspace_root,
        } => {
            let query = mcp_management_query(Some(&scope), workspace_root.as_deref());
            let result = daemon_post_raw(
                &format!("/mcp/servers/{}/login{query}", url_encode_component(&name)),
                serde_json::json!({}),
            )
            .await;
            match result {
                Ok(value) => {
                    let (msg, code) = mcp_login_result(&value);
                    if code == 0 {
                        println!("{msg}");
                    } else {
                        eprintln!("{msg}");
                    }
                    code
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        McpCommand::Catalog { json } => cli_mcp_catalog(json),
        McpCommand::Install {
            name,
            scope,
            workspace_root,
        } => cli_mcp_install(&name, &scope, workspace_root.as_deref()).await,
    }
}

async fn cli_plugin(command: PluginCommand) -> i32 {
    match command {
        PluginCommand::List { json: _json } => daemon_get_pretty_json("/plugins", "plugins").await,
        PluginCommand::Marketplace => {
            daemon_get_pretty_json("/plugins/marketplace", "plugin marketplace").await
        }
        PluginCommand::Reload => {
            daemon_post_pretty_json("/plugins/reload", serde_json::json!({}), "plugin reload").await
        }
        PluginCommand::Install {
            source_path,
            overwrite,
        } => {
            let body = serde_json::json!({
                "sourcePath": source_path,
                "trustAccepted": true,
                "overwrite": overwrite,
            });
            daemon_post_pretty_json("/plugins/install", body, "plugin install").await
        }
        PluginCommand::InstallMarketplace {
            id,
            expected_sha256,
            overwrite,
            allow_file_urls,
        } => {
            let body = serde_json::json!({
                "trustAccepted": true,
                "expectedSha256": expected_sha256,
                "overwrite": overwrite,
                "allowFileUrls": allow_file_urls,
                "fileUrlTrustAccepted": allow_file_urls,
            });
            daemon_post_pretty_json(
                &format!("/plugins/marketplace/{}/install", url_encode_component(&id)),
                body,
                "plugin marketplace install",
            )
            .await
        }
        PluginCommand::MarketplaceSync {
            catalog_url,
            expected_sha256,
            allow_unsigned,
            allow_file_urls,
        } => {
            let body = serde_json::json!({
                "catalogUrl": catalog_url,
                "expectedSha256": expected_sha256,
                "trustAccepted": true,
                "requireSigned": !allow_unsigned,
                "allowFileUrls": allow_file_urls,
                "fileUrlTrustAccepted": allow_file_urls,
            });
            daemon_post_pretty_json("/plugins/marketplace/sync", body, "plugin marketplace sync")
                .await
        }
        PluginCommand::Enable { id } => {
            daemon_post_pretty_json(
                &format!("/plugins/{}/enable", url_encode_component(&id)),
                serde_json::json!({}),
                "plugin enable",
            )
            .await
        }
        PluginCommand::Disable { id } => {
            daemon_post_pretty_json(
                &format!("/plugins/{}/disable", url_encode_component(&id)),
                serde_json::json!({}),
                "plugin disable",
            )
            .await
        }
    }
}
