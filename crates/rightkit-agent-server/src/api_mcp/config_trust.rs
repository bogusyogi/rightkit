use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

pub(crate) fn default_mcp_user_config_path() -> PathBuf {
    std::env::var_os("RIGHTKIT_MCP_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            rightkit_agent_config::os_home_dir()
                .map(|home| home.join(".config").join("coderight").join("mcp.json"))
        })
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".coderight")
                .join("mcp.json")
        })
}

pub(crate) fn workspace_mcp_config_path(workspace_root: &str) -> Option<PathBuf> {
    let trimmed = workspace_root.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed).join(".mcp.json"))
    }
}

pub(crate) fn read_mcp_config_file(
    path: &FsPath,
) -> Result<rightkit_mcp::agent_mcp::McpConfigFile, String> {
    if !path.exists() {
        return Ok(rightkit_mcp::agent_mcp::McpConfigFile::default());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str::<rightkit_mcp::agent_mcp::McpConfigFile>(&raw).map_err(|e| e.to_string())
}

pub(crate) fn write_mcp_config_file(
    path: &FsPath,
    config: &rightkit_mcp::agent_mcp::McpConfigFile,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    crate::store_write::write_store_file(path, raw.as_bytes()).map_err(|e| e.to_string())
}

pub(crate) fn validate_mcp_server_name(name: &str) -> Result<(), StatusCode> {
    let ok = !name.trim().is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.');
    if ok {
        Ok(())
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

pub(crate) fn mcp_transport_label(
    config: &rightkit_mcp::agent_mcp::McpServerConfig,
) -> &'static str {
    match config {
        rightkit_mcp::agent_mcp::McpServerConfig::Http(_) => "http",
        rightkit_mcp::agent_mcp::McpServerConfig::Sse(_) => "sse",
        rightkit_mcp::agent_mcp::McpServerConfig::Stdio(_) => "stdio",
    }
}

pub(crate) fn mcp_server_json(
    scope: &str,
    name: &str,
    config: &rightkit_mcp::agent_mcp::McpServerConfig,
    trust: Option<&rightkit_mcp::agent_mcp::McpTrustPolicy>,
    channel_permissions: Option<&rightkit_mcp::agent_mcp::McpChannelPermissions>,
) -> serde_json::Value {
    let inferred = trust.is_none();
    let trust = trust.cloned().unwrap_or_else(|| inferred_mcp_trust(config));
    serde_json::json!({
        "scope": scope,
        "name": name,
        "type": mcp_transport_label(config),
        "config": config,
        "trust": {
            "preset": trust.preset,
            "approvedBy": trust.approved_by,
            "approvedAt": trust.approved_at,
            "inferred": inferred,
        },
        "channelPermissions": channel_permissions,
        "policyDiagnostics": mcp_policy_diagnostics(config, &trust, channel_permissions),
    })
}

fn mcp_policy_diagnostics(
    config: &rightkit_mcp::agent_mcp::McpServerConfig,
    trust: &rightkit_mcp::agent_mcp::McpTrustPolicy,
    channel_permissions: Option<&rightkit_mcp::agent_mcp::McpChannelPermissions>,
) -> Vec<serde_json::Value> {
    let mut diagnostics = Vec::new();
    match channel_permissions {
        None => diagnostics.push(serde_json::json!({
            "severity": "warning",
            "code": "mcp_channel_permissions_unscoped",
            "message": "No channel permissions are configured; this server is available from every RightKit channel and mutating MCP tools are allowed for compatibility."
        })),
        Some(permissions) => {
            if permissions.allowed_channels.is_empty() {
                diagnostics.push(serde_json::json!({
                    "severity": "warning",
                    "code": "mcp_allowed_channels_empty",
                    "message": "Allowed channels is empty, so this server is available from every RightKit channel."
                }));
            }
            if permissions.allow_mutating {
                diagnostics.push(serde_json::json!({
                    "severity": "info",
                    "code": "mcp_mutating_allowed",
                    "message": "Mutating MCP tools may be called when channel policy allows this server."
                }));
            } else {
                diagnostics.push(serde_json::json!({
                    "severity": "info",
                    "code": "mcp_mutating_requires_read_only_hint",
                    "message": "Mutating MCP tools are disabled; RightKit only exposes/calls tools that declare annotations.readOnlyHint=true."
                }));
            }
        }
    }

    if matches!(
        config,
        rightkit_mcp::agent_mcp::McpServerConfig::Http(_)
            | rightkit_mcp::agent_mcp::McpServerConfig::Sse(_)
    ) && trust.preset == "remote_unreviewed"
    {
        diagnostics.push(serde_json::json!({
            "severity": "warning",
            "code": "mcp_remote_unreviewed",
            "message": "This remote MCP server is unreviewed; treat its tool annotations and outputs as untrusted until approved."
        }));
    }

    diagnostics
}

fn inferred_mcp_trust(
    config: &rightkit_mcp::agent_mcp::McpServerConfig,
) -> rightkit_mcp::agent_mcp::McpTrustPolicy {
    let preset = match config {
        rightkit_mcp::agent_mcp::McpServerConfig::Stdio(_) => "local_process",
        rightkit_mcp::agent_mcp::McpServerConfig::Http(c) if is_local_mcp_url(&c.url) => {
            "local_dev"
        }
        rightkit_mcp::agent_mcp::McpServerConfig::Sse(c) if is_local_mcp_url(&c.url) => "local_dev",
        rightkit_mcp::agent_mcp::McpServerConfig::Http(_)
        | rightkit_mcp::agent_mcp::McpServerConfig::Sse(_) => "remote_unreviewed",
    };
    rightkit_mcp::agent_mcp::McpTrustPolicy {
        preset: preset.to_string(),
        approved_by: None,
        approved_at: None,
    }
}

fn is_local_mcp_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://localhost")
        || lower.starts_with("http://[::1]")
        || lower.starts_with("https://127.0.0.1")
        || lower.starts_with("https://localhost")
        || lower.starts_with("https://[::1]")
}

fn normalize_mcp_trust_preset(preset: &str) -> Option<String> {
    match preset.trim().to_ascii_lowercase().as_str() {
        "local" | "local_dev" | "local-dev" => Some("local_dev".to_string()),
        "stdio" | "local_process" | "local-process" => Some("local_process".to_string()),
        "remote_read_only" | "remote-read-only" | "read_only" | "read-only" => {
            Some("remote_read_only".to_string())
        }
        "remote_full" | "remote-full" | "full" => Some("remote_full".to_string()),
        "remote_unreviewed" | "remote-unreviewed" | "unreviewed" => {
            Some("remote_unreviewed".to_string())
        }
        _ => None,
    }
}

fn mcp_trust_presets_json() -> serde_json::Value {
    serde_json::json!([
        {
            "id": "local_dev",
            "label": "Local dev",
            "description": "Localhost HTTP/SSE server for this machine."
        },
        {
            "id": "local_process",
            "label": "Local process",
            "description": "stdio server launched as a local subprocess."
        },
        {
            "id": "remote_read_only",
            "label": "Remote read-only",
            "description": "Reviewed remote server expected to expose only read-only tools."
        },
        {
            "id": "remote_full",
            "label": "Remote full",
            "description": "Reviewed remote server that may expose mutating tools."
        },
        {
            "id": "remote_unreviewed",
            "label": "Remote unreviewed",
            "description": "Remote server whose tool surface has not been approved yet."
        }
    ])
}

/// Home directory used to locate the persisted MCP trust-approval store,
/// mirroring `rightkit_mcp::agent_mcp::config::home_dir` (crates/mcp/src/config.rs).
fn mcp_trust_store_home_dir() -> Option<PathBuf> {
    rightkit_agent_config::os_home_dir()
}

/// Whether the workspace is explicitly trusted in the persisted workspace-trust
/// store (the store `coderight trust`, the TUI and `PATCH /workspace/trust`
/// all write). Project-scoped MCP servers of any transport may only be
/// contacted from a trusted workspace; an unknown, blank or non-`trusted`
/// workspace fails closed (A203).
pub(crate) fn workspace_project_config_trusted(workspace_root: &str) -> bool {
    rightkit_agent_config::is_workspace_trusted_in(
        &rightkit_agent_config::workspace_trust_store_path(),
        workspace_root,
    )
}

/// Decide whether a workspace-sourced stdio server may spawn right now,
/// consulting the same persisted first-use trust store the canonical session
/// loader uses (`gate_workspace_stdio_servers` in
/// crates/mcp/src/config.rs::load_with_trust_gate_pooled_impl). Ad hoc
/// surfaces (management probe, slash-command discovery) must apply this
/// identical gate before ever constructing a real `StdioTransport` for a
/// workspace server — see audit finding ra-001.
///
/// An empty/blank workspace root has no addressable trust record, so it is
/// treated as `Pending` (never spawns) rather than silently exempted.
pub(crate) fn workspace_stdio_trust_decision(
    workspace_root: &str,
    server_name: &str,
    stdio: &rightkit_mcp::agent_mcp::StdioConfig,
) -> rightkit_mcp::agent_mcp::TrustDecision {
    let workspace_root = workspace_root.trim();
    if workspace_root.is_empty() {
        return rightkit_mcp::agent_mcp::TrustDecision::Pending;
    }
    let store_path =
        rightkit_mcp::agent_mcp::default_trust_store_path(mcp_trust_store_home_dir().as_deref());
    let store = rightkit_mcp::agent_mcp::load_trust_store(&store_path);
    rightkit_mcp::agent_mcp::decide_workspace_stdio(&store, workspace_root, server_name, stdio)
}

/// Build the real transport for `(scope, name, config)`, OR — for a
/// workspace-scoped stdio server that has not been approved (or was denied)
/// by the persisted first-use trust gate — a non-spawning `StubTransport`.
/// This is the single choke point every ad hoc MCP transport-construction
/// call site (probe, `/mcp tools|resources|read|test`) must route through so
/// none of them can bypass the invariant the canonical config loader already
/// enforces at daemon/session boot (audit ra-001).
pub(crate) fn mcp_transport_from_config(
    scope: &str,
    workspace_root: &str,
    name: &str,
    config: &rightkit_mcp::agent_mcp::McpServerConfig,
) -> Arc<dyn rightkit_mcp::agent_mcp::McpTransport> {
    if scope == "workspace" {
        // A203: no project-scoped server of any transport is contacted until
        // the workspace itself is trusted.
        if !workspace_project_config_trusted(workspace_root) {
            return Arc::new(rightkit_mcp::agent_mcp::StubTransport::new(Vec::new()));
        }
        if let rightkit_mcp::agent_mcp::McpServerConfig::Stdio(stdio) = config {
            match workspace_stdio_trust_decision(workspace_root, name, stdio) {
                rightkit_mcp::agent_mcp::TrustDecision::Approved => {}
                rightkit_mcp::agent_mcp::TrustDecision::Pending
                | rightkit_mcp::agent_mcp::TrustDecision::Denied => {
                    return Arc::new(rightkit_mcp::agent_mcp::StubTransport::new(Vec::new()));
                }
            }
        }
    }

    let mut config = config.clone();
    let env_vars = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    rightkit_mcp::agent_mcp::expand_server_env(&mut config, &env_vars);

    match config {
        rightkit_mcp::agent_mcp::McpServerConfig::Http(c) => {
            Arc::new(rightkit_mcp::agent_mcp::HttpTransport::new(c.url))
        }
        rightkit_mcp::agent_mcp::McpServerConfig::Sse(c) => {
            Arc::new(rightkit_mcp::agent_mcp::SseTransport::new(c.url))
        }
        rightkit_mcp::agent_mcp::McpServerConfig::Stdio(c) => {
            let workspace_root = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_string_lossy()
                .into_owned();
            let launch_context = rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root,
                sandbox_profile: Some("mcp-management".to_string()),
                sandbox_required: true,
                network_required: false,
            };
            let transport = rightkit_mcp::agent_mcp::StdioTransport::new(c.command, c.args, c.env)
                .with_launch_context(launch_context);
            let transport = match crate::main_runtime::governed_mcp_process_launcher() {
                Ok(launcher) => transport.with_process_launch_adapter(launcher),
                Err(error) => {
                    tracing::warn!(server = %name, error = %error, "MCP management launch denied: process resource authority unavailable");
                    // Keep the approved endpoint real. Its required launch
                    // context rejects discovery without an adapter, instead
                    // of reporting a successful empty catalog via a stub.
                    transport
                }
            };
            Arc::new(transport)
        }
    }
}
