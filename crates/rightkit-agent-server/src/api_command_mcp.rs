use coderight_skills::{CommandAction, CommandResult};
use std::path::PathBuf;
use std::sync::Arc;

use crate::api::DaemonState;
use crate::api_mcp::{
    mcp_transport_label, read_mcp_config_file, validate_mcp_server_name, workspace_mcp_config_path,
    workspace_project_config_trusted, workspace_stdio_trust_decision,
};

#[derive(Clone)]
struct McpCommandServer {
    scope: &'static str,
    name: String,
    config: rightkit_mcp::agent_mcp::McpServerConfig,
    oauth: Option<rightkit_mcp::agent_mcp::McpOAuthConfig>,
    channel_permissions: Option<rightkit_mcp::agent_mcp::McpChannelPermissions>,
    trust_policy: Option<rightkit_mcp::agent_mcp::McpTrustPolicy>,
    /// Raw workspace root this server was sourced from (empty for `scope ==
    /// "user"`). Needed at transport-construction time so a workspace-sourced
    /// stdio server can be checked against the persisted first-use trust
    /// store — see [`workspace_stdio_trust_decision`] (audit ra-001).
    workspace_root: String,
}

fn command_ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: CommandAction::None,
    }
}

fn command_err(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: false,
        output: output.into(),
        action: CommandAction::None,
    }
}

fn command_mcp_workspace_path(args: &[String]) -> Option<PathBuf> {
    workspace_mcp_config_path(args.join(" ").trim())
}

fn mcp_command_servers(
    state: &DaemonState,
    workspace_args: &[String],
) -> Result<Vec<McpCommandServer>, String> {
    let mut servers = Vec::new();
    let user = read_mcp_config_file(&state.mcp_user_config_path)
        .map_err(|e| format!("Failed to read user MCP config: {e}"))?;
    let user_permissions = user.mcp_channel_permissions;
    let user_trust = user.mcp_trust;
    let user_oauth = user.mcp_oauth;
    for (name, config) in user.mcp_servers {
        let channel_permissions = user_permissions.get(&name).cloned();
        let trust_policy = user_trust.get(&name).cloned();
        let oauth = user_oauth.get(&name).cloned();
        servers.push(McpCommandServer {
            scope: "user",
            name,
            config,
            oauth,
            channel_permissions,
            trust_policy,
            workspace_root: String::new(),
        });
    }

    if let Some(path) = command_mcp_workspace_path(workspace_args) {
        let workspace_root = workspace_args.join(" ").trim().to_string();
        let workspace = read_mcp_config_file(&path)
            .map_err(|e| format!("Failed to read workspace MCP config: {e}"))?;
        let workspace_permissions = workspace.mcp_channel_permissions;
        let workspace_trust = workspace.mcp_trust;
        let workspace_oauth = workspace.mcp_oauth;
        for (name, config) in workspace.mcp_servers {
            let channel_permissions = workspace_permissions.get(&name).cloned();
            let trust_policy = workspace_trust.get(&name).cloned();
            let oauth = workspace_oauth.get(&name).cloned();
            servers.retain(|server| server.name != name);
            servers.push(McpCommandServer {
                scope: "workspace",
                name,
                config,
                oauth,
                channel_permissions,
                trust_policy,
                workspace_root: workspace_root.clone(),
            });
        }
    }

    servers.sort_by(|a, b| a.name.cmp(&b.name).then(a.scope.cmp(b.scope)));
    Ok(servers)
}

pub(crate) fn configured_mcp_server_count(state: &DaemonState) -> usize {
    mcp_command_servers(state, &[])
        .map(|servers| servers.len())
        .unwrap_or(0)
}

fn mcp_command_manager(servers: &[McpCommandServer]) -> rightkit_mcp::agent_mcp::McpManager {
    let mut manager = rightkit_mcp::agent_mcp::McpManager::new();
    for server in servers {
        manager.register(&server.name, mcp_command_transport(server));
        if let Some(permissions) = &server.channel_permissions {
            manager.register_channel_permissions(&server.name, permissions.clone());
        }
        if let Some(policy) = &server.trust_policy {
            manager.register_trust_policy(&server.name, policy.clone());
        }
    }
    manager
}

fn mcp_command_transport(
    server: &McpCommandServer,
) -> Arc<dyn rightkit_mcp::agent_mcp::McpTransport> {
    // Workspace-sourced stdio servers must pass the same persisted first-use
    // trust gate the canonical session loader applies
    // (crates/mcp/src/config.rs::load_with_trust_gate_pooled_impl) before a
    // real transport is ever constructed here. Pending/denied servers get a
    // non-spawning stub instead — see audit ra-001.
    if server.scope == "workspace" {
        // A203: project-scoped servers stay unconnected until the workspace is
        // trusted, whatever the transport.
        if !workspace_project_config_trusted(&server.workspace_root) {
            return Arc::new(rightkit_mcp::agent_mcp::StubTransport::new(Vec::new()));
        }
        if let rightkit_mcp::agent_mcp::McpServerConfig::Stdio(stdio) = &server.config {
            match workspace_stdio_trust_decision(&server.workspace_root, &server.name, stdio) {
                rightkit_mcp::agent_mcp::TrustDecision::Approved => {}
                rightkit_mcp::agent_mcp::TrustDecision::Pending
                | rightkit_mcp::agent_mcp::TrustDecision::Denied => {
                    return Arc::new(rightkit_mcp::agent_mcp::StubTransport::new(Vec::new()));
                }
            }
        }
    }

    let mut config = server.config.clone();
    let env_vars = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    rightkit_mcp::agent_mcp::expand_server_env(&mut config, &env_vars);
    let access_token = rightkit_mcp::agent_mcp::McpOAuthCredentialStore::get(&server.name)
        .ok()
        .flatten()
        .filter(|tokens| tokens.token_type.eq_ignore_ascii_case("bearer"))
        .map(|tokens| tokens.access_token);

    match config {
        rightkit_mcp::agent_mcp::McpServerConfig::Http(c) => Arc::new(
            rightkit_mcp::agent_mcp::HttpTransport::with_access_token(c.url, access_token),
        ),
        rightkit_mcp::agent_mcp::McpServerConfig::Sse(c) => Arc::new(
            rightkit_mcp::agent_mcp::SseTransport::with_access_token(c.url, access_token),
        ),
        rightkit_mcp::agent_mcp::McpServerConfig::Stdio(c) => {
            let workspace_root = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_string_lossy()
                .into_owned();
            let launch_context = rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root,
                sandbox_profile: Some("mcp-command".to_string()),
                sandbox_required: true,
                network_required: false,
            };
            let launcher = match crate::main_runtime::governed_mcp_process_launcher() {
                Ok(launcher) => launcher,
                Err(error) => {
                    tracing::warn!(server = %server.name, error = %error, "MCP slash-command launch denied: process resource authority unavailable");
                    return Arc::new(rightkit_mcp::agent_mcp::StubTransport::new(Vec::new()));
                }
            };
            Arc::new(
                rightkit_mcp::agent_mcp::StdioTransport::new(c.command, c.args, c.env)
                    .with_process_launch_adapter(launcher)
                    .with_launch_context(launch_context),
            )
        }
    }
}

fn command_channel_denial(
    manager: &rightkit_mcp::agent_mcp::McpManager,
    server: &str,
) -> Option<String> {
    if manager.is_channel_allowed(server, "desktop") {
        return None;
    }
    Some(format!(
        "MCP server '{server}' is not allowed for desktop slash commands. Update mcpChannelPermissions.allowedChannels to include desktop."
    ))
}

fn mcp_required_inputs(schema: &serde_json::Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(|required| required.as_array())
        .map(|required| {
            required
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn mcp_tool_command_row(tool: &rightkit_mcp::agent_mcp::McpToolDef) -> String {
    let required = mcp_required_inputs(&tool.input_schema);
    let mut row = format!("- {}", tool.name);
    if !tool.description.trim().is_empty() {
        row.push_str(&format!(" - {}", tool.description.trim()));
    }
    if !required.is_empty() {
        row.push_str(&format!(" [required: {}]", required.join(", ")));
    }
    row
}

fn mcp_resource_command_row(resource: &rightkit_mcp::agent_mcp::McpResourceDef) -> String {
    let mut row = format!("- {}", resource.uri);
    if !resource.name.trim().is_empty() {
        row.push_str(&format!(" - {}", resource.name.trim()));
    }
    if !resource.mime_type.trim().is_empty() {
        row.push_str(&format!(" ({})", resource.mime_type.trim()));
    }
    if !resource.description.trim().is_empty() {
        row.push_str(&format!(" - {}", resource.description.trim()));
    }
    row
}

fn mcp_server_mode_tool_counts() -> (usize, usize) {
    let capabilities = rightkit_agent_tools::coding_tools::default_tool_capabilities();
    let read_only = capabilities.iter().filter(|tool| tool.read_only).count();
    let total = capabilities.len();
    (read_only, total.saturating_sub(read_only))
}

pub(crate) async fn command_mcp(state: &DaemonState, args: &[String]) -> CommandResult {
    match args.first().map(|arg| arg.as_str()) {
        Some("tools") => command_mcp_tools(state, &args[1..]).await,
        Some("resources") => command_mcp_resources(state, &args[1..]).await,
        Some("read") => command_mcp_read(state, &args[1..]).await,
        Some("test") | Some("probe") => command_mcp_test(state, &args[1..]).await,
        Some("login") => command_mcp_login(state, &args[1..]).await,
        Some("logout") => command_mcp_logout(state, &args[1..]).await,
        Some("auth") => command_mcp_auth(state, &args[1..]),
        _ => command_mcp_summary(state, args),
    }
}

fn command_mcp_summary(state: &DaemonState, args: &[String]) -> CommandResult {
    let workspace_arg = args.join(" ");
    let workspace_path = workspace_mcp_config_path(workspace_arg.trim());
    let mut servers = Vec::new();
    match read_mcp_config_file(&state.mcp_user_config_path) {
        Ok(user) => {
            for (name, config) in user.mcp_servers {
                servers.push(format!("user/{name} ({})", mcp_transport_label(&config)));
            }
        }
        Err(e) => return command_err(format!("Failed to read user MCP config: {e}")),
    }
    if let Some(path) = workspace_path {
        match read_mcp_config_file(&path) {
            Ok(workspace) => {
                for (name, config) in workspace.mcp_servers {
                    servers.push(format!(
                        "workspace/{name} ({})",
                        mcp_transport_label(&config)
                    ));
                }
            }
            Err(e) => return command_err(format!("Failed to read workspace MCP config: {e}")),
        }
    }
    servers.sort();
    let count = servers.len();
    let (read_only_tools, mutating_tools) = mcp_server_mode_tool_counts();
    let mut output = format!(
        "**MCP:** {count} server{}\nUser config: {}\nServer mode: `coderight mcp-server` exposes {read_only_tools} read-only tools by default; `coderight mcp-server --allow-mutate` exposes {mutating_tools} mutating tools when the configured trust tier permits them. Served MCP invocations are audit-recorded.",
        if count == 1 { "" } else { "s" },
        state.mcp_user_config_path.display(),
    );
    if servers.is_empty() {
        output.push_str("\nNo configured MCP servers.");
    } else {
        output.push('\n');
        output.push_str(
            &servers
                .into_iter()
                .map(|server| format!("- {server}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    command_ok(output)
}

async fn command_mcp_tools(state: &DaemonState, workspace_args: &[String]) -> CommandResult {
    let servers = match mcp_command_servers(state, workspace_args) {
        Ok(servers) => servers,
        Err(e) => return command_err(e),
    };
    if servers.is_empty() {
        return command_ok(format!(
            "**MCP tools:** 0 tools\nUser config: {}\nNo configured MCP servers.",
            state.mcp_user_config_path.display()
        ));
    }

    let manager = mcp_command_manager(&servers);
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(12), manager.discover_all()).await;
    let mut tools = match result {
        Ok(tools) => tools,
        Err(_) => return command_err("MCP tools probe timed out."),
    };
    tools.retain(|tool| {
        rightkit_mcp::agent_mcp::parse_qualified_name(&tool.name)
            .map(|(server, _)| manager.is_channel_allowed(&server, "desktop"))
            .unwrap_or(true)
    });
    tools.retain(|tool| {
        rightkit_mcp::agent_mcp::parse_qualified_name(&tool.name)
            .map(|(server, _)| manager.is_tool_allowed_by_mutating_policy(&server, tool))
            .unwrap_or(true)
    });
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let mut output = format!(
        "**MCP tools:** {} tool{}\nServers: {}",
        tools.len(),
        if tools.len() == 1 { "" } else { "s" },
        servers
            .iter()
            .map(|server| format!("{}:{}", server.scope, server.name))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if tools.is_empty() {
        output.push_str("\nNo tools discovered.");
    } else {
        output.push('\n');
        output.push_str(
            &tools
                .iter()
                .map(mcp_tool_command_row)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    command_ok(output)
}

async fn command_mcp_resources(state: &DaemonState, args: &[String]) -> CommandResult {
    let Some(server) = args.first() else {
        return command_err("Usage: /mcp resources <server> [workspace-root]");
    };
    let servers = match mcp_command_servers(state, &args[1..]) {
        Ok(servers) => servers,
        Err(e) => return command_err(e),
    };
    if !servers.iter().any(|candidate| candidate.name == *server) {
        return command_err(format!("MCP server not found: {server}"));
    }
    let manager = mcp_command_manager(&servers);
    if let Some(denial) = command_channel_denial(&manager, server) {
        return command_err(denial);
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        manager.list_resources(server),
    )
    .await;
    let mut resources = match result {
        Ok(Ok(resources)) => resources,
        Ok(Err(e)) => return command_err(format!("MCP resources failed for {server}: {e}")),
        Err(_) => return command_err(format!("MCP resources timed out for {server}.")),
    };
    resources.sort_by(|a, b| a.uri.cmp(&b.uri));
    let mut output = format!(
        "**MCP resources:** {server} - {} resource{}",
        resources.len(),
        if resources.len() == 1 { "" } else { "s" }
    );
    if resources.is_empty() {
        output.push_str("\nNo resources discovered.");
    } else {
        output.push('\n');
        output.push_str(
            &resources
                .iter()
                .map(mcp_resource_command_row)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    command_ok(output)
}

async fn command_mcp_read(state: &DaemonState, args: &[String]) -> CommandResult {
    let Some(server) = args.first() else {
        return command_err("Usage: /mcp read <server> <uri> [workspace-root]");
    };
    let Some(uri) = args.get(1) else {
        return command_err("Usage: /mcp read <server> <uri> [workspace-root]");
    };
    let servers = match mcp_command_servers(state, &args[2..]) {
        Ok(servers) => servers,
        Err(e) => return command_err(e),
    };
    if !servers.iter().any(|candidate| candidate.name == *server) {
        return command_err(format!("MCP server not found: {server}"));
    }
    let manager = mcp_command_manager(&servers);
    if let Some(denial) = command_channel_denial(&manager, server) {
        return command_err(denial);
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        manager.read_resource(server, uri),
    )
    .await;
    let content = match result {
        Ok(Ok(content)) => content,
        Ok(Err(e)) => return command_err(format!("MCP read failed for {server}: {e}")),
        Err(_) => return command_err(format!("MCP read timed out for {server}.")),
    };

    let mut output = format!("**MCP resource:** {server} {}\n", content.uri);
    if !content.mime_type.trim().is_empty() {
        output.push_str(&format!("MIME: {}\n", content.mime_type.trim()));
    }
    if let Some(text) = content.text {
        output.push_str(&text);
    } else if let Some(blob) = content.blob {
        output.push_str(&format!(
            "Binary content returned as base64 ({} bytes).",
            blob.len()
        ));
    } else {
        output.push_str("Resource returned no text or blob content.");
    }
    command_ok(output)
}

async fn command_mcp_test(state: &DaemonState, args: &[String]) -> CommandResult {
    let Some(server) = args.first() else {
        return command_err("Usage: /mcp test <server> [workspace-root]");
    };
    let servers = match mcp_command_servers(state, &args[1..]) {
        Ok(servers) => servers,
        Err(e) => return command_err(e),
    };
    let Some(candidate) = servers.iter().find(|candidate| candidate.name == *server) else {
        return command_err(format!("MCP server not found: {server}"));
    };

    let started = std::time::Instant::now();
    let transport = mcp_command_transport(candidate);
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(12), transport.list_tools()).await;
    let latency_ms = started.elapsed().as_millis();
    match result {
        Ok(Ok(tools)) => command_ok(format!(
            "**MCP test:** {server}\nStatus: ok\nScope: {}\nTransport: {}\nTools: {}\nLatency: {latency_ms}ms",
            candidate.scope,
            mcp_transport_label(&candidate.config),
            tools.len()
        )),
        Ok(Err(e)) => command_err(format!(
            "**MCP test:** {server}\nStatus: failed\nScope: {}\nTransport: {}\nLatency: {latency_ms}ms\nError: {e}",
            candidate.scope,
            mcp_transport_label(&candidate.config)
        )),
        Err(_) => command_err(format!(
            "**MCP test:** {server}\nStatus: timed out\nScope: {}\nTransport: {}\nLatency: {latency_ms}ms",
            candidate.scope,
            mcp_transport_label(&candidate.config)
        )),
    }
}

async fn command_mcp_login(state: &DaemonState, args: &[String]) -> CommandResult {
    let Some(server) = args.first() else {
        return command_err("Usage: /mcp login <server> [workspace-root]");
    };
    if validate_mcp_server_name(server).is_err() {
        return command_err("Invalid MCP server name.");
    }
    let servers = match mcp_command_servers(state, &args[1..]) {
        Ok(servers) => servers,
        Err(e) => return command_err(e),
    };
    let Some(candidate) = servers.iter().find(|candidate| candidate.name == *server) else {
        return command_err(format!("MCP server not found: {server}"));
    };
    let Some(oauth) = candidate.oauth.as_ref() else {
        return command_err(format!(
            "MCP server '{server}' has no OAuth configuration. Add an oauth block with authorizationUrl, tokenUrl, clientId, scopes, and resource."
        ));
    };
    let env = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    let headless = rightkit_mcp::agent_mcp::detect_headless_from_env(&env);
    let result = rightkit_mcp::agent_mcp::login_oauth(
        oauth,
        server,
        rightkit_mcp::agent_mcp::McpOAuthLoginOptions {
            open_browser: !headless,
        },
    )
    .await;
    match result {
        Ok(result) => {
            let refresh = if result.tokens.refresh_token.is_some() {
                "yes"
            } else {
                "no"
            };
            let mut output = format!(
                "**MCP login: {server}**\nStatus: ok\nRefresh token: {refresh}\nResource: {}",
                result.tokens.resource.as_deref().unwrap_or(&oauth.resource)
            );
            if result.headless {
                output.push_str(&format!(
                    "\nHeadless mode detected; open this URL manually:\n{}",
                    result.authorization_url
                ));
            }
            command_ok(output)
        }
        Err(e) => command_err(format!("MCP login failed for {server}: {e}")),
    }
}

async fn command_mcp_logout(state: &DaemonState, args: &[String]) -> CommandResult {
    let Some(server) = args.first() else {
        return command_err("Usage: /mcp logout <server> [workspace-root]");
    };
    if validate_mcp_server_name(server).is_err() {
        return command_err("Invalid MCP server name.");
    }
    let servers = match mcp_command_servers(state, &args[1..]) {
        Ok(servers) => servers,
        Err(e) => return command_err(e),
    };
    let candidate = servers.iter().find(|candidate| candidate.name == *server);
    let stored = match rightkit_mcp::agent_mcp::McpOAuthCredentialStore::get(server) {
        Ok(tokens) => tokens,
        Err(e) => return command_err(format!("Failed to read MCP credentials for {server}: {e}")),
    };
    if let (Some(candidate), Some(tokens)) = (candidate, stored.as_ref()) {
        if let Some(oauth) = candidate.oauth.as_ref() {
            if let Err(e) = rightkit_mcp::agent_mcp::revoke_oauth_tokens(oauth, tokens).await {
                return command_err(format!("MCP logout revoke failed for {server}: {e}"));
            }
        }
    }
    match rightkit_mcp::agent_mcp::McpOAuthCredentialStore::delete(server) {
        Ok(()) => command_ok(format!(
            "**MCP logout: {server}**\nStatus: credentials revoked when supported and deleted."
        )),
        Err(e) => command_err(format!(
            "Failed to delete MCP credentials for {server}: {e}"
        )),
    }
}

#[derive(Clone, Copy)]
pub(crate) enum McpAuthCredentialStatus {
    PresentWithRefreshToken,
    PresentWithoutRefreshToken,
    Missing,
}

pub(crate) fn render_mcp_auth_status(
    server: &str,
    status: McpAuthCredentialStatus,
) -> CommandResult {
    let status = match status {
        McpAuthCredentialStatus::PresentWithRefreshToken => {
            "Keyring credential present with refresh token"
        }
        McpAuthCredentialStatus::PresentWithoutRefreshToken => {
            "Keyring credential present without refresh token"
        }
        McpAuthCredentialStatus::Missing => "No credential file; no keyring credential",
    };
    command_ok(format!(
        "**MCP auth: {server}**\nStatus: {status}\nCredential store: keyring mcp-auth\nSecrets are never displayed by this command."
    ))
}

fn command_mcp_auth(_state: &DaemonState, args: &[String]) -> CommandResult {
    let Some(server) = args.first() else {
        return command_err("Usage: /mcp auth <server>");
    };
    if validate_mcp_server_name(server).is_err() {
        return command_err("Invalid MCP server name.");
    }
    let status = match rightkit_mcp::agent_mcp::McpOAuthCredentialStore::get(server) {
        Ok(Some(tokens)) if tokens.refresh_token.is_some() => {
            McpAuthCredentialStatus::PresentWithRefreshToken
        }
        Ok(Some(_)) => McpAuthCredentialStatus::PresentWithoutRefreshToken,
        Ok(None) => McpAuthCredentialStatus::Missing,
        Err(e) => return command_err(format!("Failed to read MCP credentials: {e}")),
    };
    render_mcp_auth_status(server, status)
}
