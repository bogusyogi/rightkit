fn normalize_mcp_channel_permissions(
    permissions: Option<rightkit_mcp::agent_mcp::McpChannelPermissions>,
) -> Option<rightkit_mcp::agent_mcp::McpChannelPermissions> {
    let permissions = permissions?;
    let mut allowed_channels = permissions
        .allowed_channels
        .into_iter()
        .map(|channel| channel.trim().to_ascii_lowercase())
        .filter(|channel| {
            !channel.is_empty()
                && channel
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        })
        .collect::<Vec<_>>();
    allowed_channels.sort();
    allowed_channels.dedup();
    Some(rightkit_mcp::agent_mcp::McpChannelPermissions {
        allowed_channels,
        allow_mutating: permissions.allow_mutating,
    })
}

async fn probe_mcp_server_handler(
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

    let file = read_mcp_config_file(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let server = file.mcp_servers.get(&name).ok_or(StatusCode::NOT_FOUND)?;
    let started = std::time::Instant::now();
    let transport = mcp_transport_from_config(&scope, &query.workspace_root, &name, server);
    // stdio servers — especially the canonical `npx -y @scope/server` config —
    // cold-start slowly (npm resolve + download + node spawn) and routinely blew
    // past a flat 12s probe, so the most common MCP setup failed its first probe.
    // Remote http/sse respond fast, so keep their window tight.
    let probe_timeout = if matches!(server, rightkit_mcp::agent_mcp::McpServerConfig::Stdio(_)) {
        // Must exceed the stdio handshake budget (initialize 30s + tools/list 30s
        // in StdioTransport) so the outer timeout doesn't pre-empt a cold npx start.
        std::time::Duration::from_secs(75)
    } else {
        std::time::Duration::from_secs(12)
    };
    let result = tokio::time::timeout(probe_timeout, transport.list_tools())
        .await
        .map_err(|_| "probe timed out".to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    let latency_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(tools) => Ok(Json(serde_json::json!({
            "ok": true,
            "scope": scope,
            "name": name,
            "type": mcp_transport_label(server),
            "toolCount": tools.len(),
            "tools": tools,
            "latencyMs": latency_ms,
        }))),
        Err(error) => Ok(Json(serde_json::json!({
            "ok": false,
            "scope": scope,
            "name": name,
            "type": mcp_transport_label(server),
            "toolCount": 0,
            "tools": [],
            "latencyMs": latency_ms,
            "error": error,
        }))),
    }
}
