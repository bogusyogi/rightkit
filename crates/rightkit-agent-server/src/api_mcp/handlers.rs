#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpManagementQuery {
    #[serde(default)]
    workspace_root: String,
    #[serde(default)]
    scope: Option<String>,
    /// Opt-in browser launch for the OAuth login route. Defaults to false:
    /// this route is reachable from agent-driven MCP connects, and spawning
    /// a browser (via `cmd` on Windows) from a background agent flashes
    /// console windows and can re-fire endlessly against unreachable auth
    /// servers. Interactive surfaces (desktop login button) pass
    /// `?openBrowser=true`; everyone else gets the authorizationUrl in the
    /// response to open deliberately.
    #[serde(default)]
    open_browser: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveMcpServer {
    scope: String,
    #[serde(default)]
    workspace_root: String,
    name: String,
    config: rightkit_mcp::agent_mcp::McpServerConfig,
    #[serde(default)]
    trust_preset: Option<String>,
    #[serde(default)]
    approved_by: Option<String>,
    #[serde(default)]
    channel_permissions: Option<rightkit_mcp::agent_mcp::McpChannelPermissions>,
}

async fn list_mcp_handler(
    State(state): State<DaemonState>,
    Query(query): Query<McpManagementQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // One malformed entry must not hide the good ones: list those that parse and name the rest.
    let mut invalid_entries: Vec<serde_json::Value> = Vec::new();
    let (user, skipped) = read_mcp_config_file_lenient(&state.mcp_user_config_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for message in skipped {
        invalid_entries.push(serde_json::json!({ "scope": "user", "message": message }));
    }
    let workspace_path = workspace_mcp_config_path(&query.workspace_root);
    let workspace = match &workspace_path {
        Some(path) => {
            let (file, skipped) = read_mcp_config_file_lenient(path)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            for message in skipped {
                invalid_entries
                    .push(serde_json::json!({ "scope": "workspace", "message": message }));
            }
            file
        }
        None => rightkit_mcp::agent_mcp::McpConfigFile::default(),
    };

    let mut servers = Vec::new();
    for (name, config) in user.mcp_servers.iter() {
        servers.push(mcp_server_json(
            "user",
            name,
            config,
            user.mcp_trust.get(name),
            user.mcp_channel_permissions.get(name),
        ));
    }
    for (name, config) in workspace.mcp_servers.iter() {
        servers.push(mcp_server_json(
            "workspace",
            name,
            config,
            workspace.mcp_trust.get(name),
            workspace.mcp_channel_permissions.get(name),
        ));
    }
    servers.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or_default();
        let bn = b["name"].as_str().unwrap_or_default();
        let ascope = a["scope"].as_str().unwrap_or_default();
        let bscope = b["scope"].as_str().unwrap_or_default();
        an.cmp(bn).then_with(|| ascope.cmp(bscope))
    });

    Ok(Json(serde_json::json!({
        "userConfigPath": state.mcp_user_config_path,
        "workspaceConfigPath": workspace_path,
        "trustPresets": mcp_trust_presets_json(),
        "servers": servers,
        "invalidEntries": invalid_entries,
    })))
}

async fn mcp_server_status_handler() -> Json<serde_json::Value> {
    let mut read_only_tools = Vec::new();
    let mut mutating_tools = Vec::new();
    for capability in rightkit_agent_coding_tools::default_tool_capabilities() {
        if capability.read_only {
            read_only_tools.push(capability.name);
        } else {
            mutating_tools.push(capability.name);
        }
    }
    read_only_tools.sort();
    mutating_tools.sort();

    Json(serde_json::json!({
        "command": format!("{app} mcp-server", app = crate::host_identity::app()),
        "readOnlyCommand": format!("{app} mcp-server", app = crate::host_identity::app()),
        "mutatingCommand": format!("{app} mcp-server --allow-mutate", app = crate::host_identity::app()),
        "defaultMode": "read_only",
        "mutatingRequiresOptIn": true,
        "readOnlyToolCount": read_only_tools.len(),
        "mutatingToolCount": mutating_tools.len(),
        "totalToolCount": read_only_tools.len() + mutating_tools.len(),
        "readOnlyTools": read_only_tools,
        "mutatingTools": mutating_tools,
    }))
}

async fn save_mcp_server_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<SaveMcpServer>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    validate_mcp_server_name(&body.name)?;
    let path = match body.scope.as_str() {
        "user" => state.mcp_user_config_path.clone(),
        "workspace" => {
            workspace_mcp_config_path(&body.workspace_root).ok_or(StatusCode::BAD_REQUEST)?
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let mut file = read_mcp_config_file(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let preset = match body.trust_preset.as_deref() {
        Some(raw) => normalize_mcp_trust_preset(raw).ok_or(StatusCode::BAD_REQUEST)?,
        None => inferred_mcp_trust(&body.config).preset,
    };
    file.mcp_servers
        .insert(body.name.clone(), body.config.clone());
    file.mcp_trust.insert(
        body.name.clone(),
        rightkit_mcp::agent_mcp::McpTrustPolicy {
            preset,
            approved_by: body.approved_by.filter(|s| !s.trim().is_empty()),
            approved_at: Some(crate::api::now_iso()),
        },
    );
    if let Some(permissions) = normalize_mcp_channel_permissions(body.channel_permissions) {
        file.mcp_channel_permissions
            .insert(body.name.clone(), permissions);
    } else {
        file.mcp_channel_permissions.remove(&body.name);
    }
    write_mcp_config_file(&path, &file).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    publish_mcp_lifecycle(&state, &body.name, "saved").await;

    Ok(Json(serde_json::json!({
        "saved": true,
        "scope": body.scope,
        "name": body.name,
        "path": path,
        "trust": file.mcp_trust.get(&body.name),
        "channelPermissions": file.mcp_channel_permissions.get(&body.name),
    })))
}

async fn delete_mcp_server_handler(
    State(state): State<DaemonState>,
    Path(name): Path<String>,
    Query(query): Query<McpManagementQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    validate_mcp_server_name(&name)?;
    let scope = query.scope.unwrap_or_else(|| "user".to_string());
    let path = match scope.as_str() {
        "user" => state.mcp_user_config_path.clone(),
        "workspace" => {
            workspace_mcp_config_path(&query.workspace_root).ok_or(StatusCode::BAD_REQUEST)?
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let mut file = read_mcp_config_file(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let removed = file.mcp_servers.remove(&name).is_some();
    file.mcp_trust.remove(&name);
    file.mcp_channel_permissions.remove(&name);
    write_mcp_config_file(&path, &file).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Some(store) =
        rightkit_agent_config::definition_pins::pin_store_path("mcp-tool-definitions.json")
    {
        let _ = rightkit_agent_config::definition_pins::clear_scope(
            &store,
            &format!("mcp:{}", name.to_ascii_lowercase()),
        );
    }
    publish_mcp_lifecycle(&state, &name, if removed { "removed" } else { "unchanged" }).await;

    Ok(Json(serde_json::json!({
        "removed": removed,
        "scope": scope,
        "name": name,
        "path": path,
    })))
}

/// MCP configuration is host state. Publish its replacement at the provider
/// boundary so in-flight requests retain their immutable contract.
async fn publish_mcp_lifecycle(state: &DaemonState, server: &str, action: &str) {
    let sessions = state
        .sessions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let launcher = match crate::main_runtime::governed_mcp_process_launcher() {
        Ok(launcher) => launcher,
        Err(error) => {
            tracing::warn!(error = %error, "MCP lifecycle reload denied");
            return;
        }
    };
    // FM-100 lease epochs: advance every pooled key's epoch exactly once per reload (not once
    // per session). The connection and lease count are retained; only the generation advances.
    // Each session then runs load_for_session, whose get_or_init_pooled drops the stale shared
    // entry and builds a fresh transport object that is discarded, because
    // McpTransportPool::acquire returns the existing leased transport, and record_epoch
    // stores lease.generation(). The stdio server process is not restarted. Never evict or
    // invalidate here: that would kill the retained process.
    let mut bumped = 0usize;
    for entry in state.mcp_pool.snapshot() {
        state.mcp_pool.bump_epoch_on_reload(&entry.key);
        bumped += 1;
    }
    tracing::debug!(
        server,
        action,
        bumped,
        "MCP lifecycle reload advanced pool epochs"
    );
    for session in sessions {
        // A203: a reload keeps the session's own trust posture; project MCP
        // config reloads only for a workspace the session holds as Trusted.
        let project_trust =
            if session.workspace_trust == rightkit_agent_policy::WorkspaceTrust::Trusted {
                rightkit_mcp::agent_mcp::ProjectTrust::Trusted
            } else {
                rightkit_mcp::agent_mcp::ProjectTrust::Untrusted
            };
        let manager = rightkit_mcp::agent_mcp::load_for_session(
            Some(std::path::Path::new(&session.workspace_root)),
            None,
            &state.mcp_pool,
            launcher.clone(),
            rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: session.workspace_root.clone(),
                sandbox_profile: Some("mcp".to_string()),
                sandbox_required: session.safe_mode,
                network_required: false,
            },
            project_trust,
        )
        .await;
        let Some(tools) = session.tool_catalog.snapshot() else {
            tracing::warn!(session_id = %session.id, "MCP lifecycle reload has no bound session catalog");
            continue;
        };
        session.publish_mcp_manager(std::sync::Arc::new(manager), tools);
        session.publish_boundary_text(
            "prompt",
            format!("MCP server '{server}' configuration {action}"),
        );
        session.publish_boundary_text(
            "context",
            format!("MCP server '{server}' configuration {action}"),
        );
        session.publish_boundary_text("world", format!("MCP lifecycle: {server} {action}"));
    }
}

/// POST /mcp/servers/{name}/login — trigger OAuth login for a named server.
async fn login_mcp_server_handler(
    State(state): State<DaemonState>,
    Path(name): Path<String>,
    Query(query): Query<McpManagementQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Contract R16 / D11: a login opens a browser and a listener against a URL the config
    // chose, so it needs the same not-lower-trust source as save and delete.
    enforce_trusted_source_authority(&headers)?;
    validate_mcp_server_name(&name)?;
    let scope = query.scope.unwrap_or_else(|| "user".to_string());
    let path = match scope.as_str() {
        "user" => state.mcp_user_config_path.clone(),
        "workspace" => {
            workspace_mcp_config_path(&query.workspace_root).ok_or(StatusCode::BAD_REQUEST)?
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let file = read_mcp_config_file(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let config = match file.mcp_servers.get(&name) {
        Some(c) => c.clone(),
        None => {
            return Ok(Json(serde_json::json!({
                "ok": false,
                "error": format!("no such server configured: {name}"),
            })))
        }
    };

    match file.mcp_oauth.get(&name) {
        Some(oauth) => {
            // Contract R2: the token is bound to the server's own origin, so the OAuth block
            // must name that server as its resource.
            let server_url = match &config {
                rightkit_mcp::agent_mcp::McpServerConfig::Http(http) => Some(http.url.as_str()),
                rightkit_mcp::agent_mcp::McpServerConfig::Sse(sse) => Some(sse.url.as_str()),
                rightkit_mcp::agent_mcp::McpServerConfig::Stdio(_) => None,
            };
            if let Some(server_url) = server_url {
                let same = matches!(
                    (
                        rightkit_mcp::agent_mcp::url_origin(&oauth.resource),
                        rightkit_mcp::agent_mcp::url_origin(server_url)
                    ),
                    (Some(a), Some(b)) if a == b
                );
                if !same {
                    return Ok(Json(serde_json::json!({
                        "ok": false,
                        "error": format!(
                            "login refused: the oauth resource for '{name}' must be on the same origin as the server URL"
                        ),
                    })));
                }
            }
            let options = rightkit_mcp::agent_mcp::McpOAuthLoginOptions {
                open_browser: query.open_browser.unwrap_or(false),
            };
            match rightkit_mcp::agent_mcp::login_oauth(oauth, &name, options).await {
                Ok(result) => Ok(Json(serde_json::json!({
                    "ok": true,
                    "message": format!("login successful for '{name}'"),
                    "authorizationUrl": result.authorization_url,
                }))),
                Err(e) => Ok(Json(serde_json::json!({
                    "ok": false,
                    "error": format!("login failed: {e}"),
                }))),
            }
        }
        None => Ok(Json(serde_json::json!({
            "ok": true,
            "message": format!("server '{name}' does not require login (no auth configured)"),
        }))),
    }
}

/// The stdio servers a trusted workspace declares, merged the way the loader merges them
/// (`.mcp.json`, then `<state folder>/mcp.json`; later files win by name).
fn workspace_stdio_servers(
    workspace_root: &str,
) -> Result<std::collections::HashMap<String, rightkit_mcp::agent_mcp::StdioConfig>, StatusCode> {
    let root = std::path::PathBuf::from(workspace_root.trim());
    if workspace_root.trim().is_empty() || !root.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut servers = std::collections::HashMap::new();
    for path in [
        root.join(".mcp.json"),
        root.join(rightkit_agent_config::state_dir_name())
            .join("mcp.json"),
    ] {
        let file = read_mcp_config_file(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        for (name, config) in file.mcp_servers {
            match config {
                rightkit_mcp::agent_mcp::McpServerConfig::Stdio(stdio) => {
                    servers.insert(name, stdio);
                }
                _ => {
                    servers.remove(&name);
                }
            }
        }
    }
    Ok(servers)
}

fn mcp_trust_store_path() -> std::path::PathBuf {
    rightkit_mcp::agent_mcp::default_trust_store_path(
        rightkit_agent_config::os_home_dir().as_deref(),
    )
}

/// GET /mcp/approvals — workspace stdio servers waiting for the user's approval (contract R3).
async fn list_mcp_approvals_handler(
    Query(query): Query<McpManagementQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !rightkit_agent_config::is_workspace_trusted(std::path::Path::new(
        query.workspace_root.trim(),
    )) {
        return Ok(Json(
            serde_json::json!({"workspaceTrusted": false, "pending": []}),
        ));
    }
    let servers = workspace_stdio_servers(&query.workspace_root)?;
    let key = std::path::Path::new(query.workspace_root.trim())
        .to_string_lossy()
        .to_string();
    let pending =
        rightkit_mcp::agent_mcp::pending_workspace_stdio(&mcp_trust_store_path(), &key, &servers);
    Ok(Json(serde_json::json!({
        "workspaceTrusted": true,
        "pending": pending
            .iter()
            .map(|p| serde_json::json!({
                "server": p.server,
                "command": p.command,
                "args": p.args,
                "envKeys": p.env_keys,
                "fingerprint": p.fingerprint,
            }))
            .collect::<Vec<_>>(),
    })))
}

/// POST /mcp/servers/{name}/approve — approve a workspace stdio server, bound to exactly its
/// command, args and environment (contract R3). An untrusted workspace cannot be approved.
async fn approve_mcp_server_handler(
    State(state): State<DaemonState>,
    Path(name): Path<String>,
    Query(query): Query<McpManagementQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    validate_mcp_server_name(&name)?;
    let root = std::path::Path::new(query.workspace_root.trim());
    if !rightkit_agent_config::is_workspace_trusted(root) {
        return Err(StatusCode::FORBIDDEN);
    }
    let servers = workspace_stdio_servers(&query.workspace_root)?;
    let Some(config) = servers.get(&name) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let key = root.to_string_lossy().to_string();
    rightkit_mcp::agent_mcp::approve_workspace_stdio(&mcp_trust_store_path(), &key, &name, config)
        .map_err(|error| {
            tracing::warn!(error = %error, "MCP approval was not saved");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    tracing::info!(event = "mcp.trust", server = %name, outcome = "approved");
    publish_mcp_lifecycle(&state, &name, "approved").await;
    Ok(Json(serde_json::json!({"approved": true, "name": name})))
}
