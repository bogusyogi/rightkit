//! host_mcp — Model Context Protocol client layer.
//!
//! Shared transports: one connection per MCP server, multiplexed across all
//! the host app sessions. Stateful HTTP servers receive a `Mcp-Session-Id` header
//! per the host app session for correct state isolation within the shared process.
//!
//! Three transports:
//! - [`HttpTransport`] — Streamable HTTP (supergateway `--outputTransport streamableHttp`)
//! - [`SseTransport`]  — SSE (supergateway default / single-client servers)
//! - [`StdioTransport`] — stdio (spawn once, share across sessions)
//!
//! Config is loaded from `~/.config/host app/mcp.json` (Claude desktop format).

pub mod config;
mod error;
mod host;
pub mod limits;
pub mod lsp_bridge;
mod manager;
pub mod oauth;
mod pool;
mod server_mode;
pub mod shared_pool;
mod stale;
mod transport;
pub mod trust_gate;
mod types;

pub use crate::{parse_qualified_name, sanitize_tool_name};
pub use crate::{McpError as RightKitMcpError, RequestContext, ToolDescriptor};
pub use config::{
    attach_stored_oauth_tokens, expand_config_env, expand_server_env, expand_server_env_in_scope,
    load_for_session, load_with_config_path_pool_and_process_launcher, load_with_pool,
    load_with_trust_gate, load_with_trust_gate_and_project_trust, load_with_trust_gate_pooled,
    resolve_http_bearer, resolve_http_bearer_with_command, PlaceholderScope, ProjectTrust,
};
pub use error::McpError;
pub use host::{
    client_name, client_version, keyring_service, set_client_name, set_client_version,
    set_credential_env_names, set_keyring_service, set_never_expanded_env_names,
    DEFAULT_CLIENT_NAME, DEFAULT_KEYRING_SERVICE,
};
pub use lsp_bridge::{
    lsp_mcp_tool_definitions, Diagnostic, GotoDefinitionResult, HoverResult, LspBridgeError,
    LspMcpBridge, LspPosition, LspServerConfig,
};
pub use manager::{
    tool_glob_matches, CanonicalMembraneBinding, CanonicalMembraneCaller, McpManager,
    McpToolNameFilter,
};
pub use oauth::{
    detect_headless_from_env, url_origin, McpOAuthLoginOptions, McpOAuthLoginResult, McpOAuthTokens,
};
#[cfg(feature = "agent_mcp-os-keyring")]
pub use oauth::{login_oauth, refresh_oauth_tokens, revoke_oauth_tokens, McpOAuthCredentialStore};
pub use pool::{pool_key, McpTransportPool, DEFAULT_MAX_POOLED_INSTANCES};
pub use server_mode::{
    McpServedInvocationAudit, McpServerModeGuard, McpServerModePolicy, McpServerToolSummary,
    RightKitMcpBackend, RightKitMcpServer,
};
pub use shared_pool::{get_or_init_shared_pool, get_shared_pool, init_shared_pool, SharedMcpPool};
pub use stale::stdio_unreachable_reason;
pub use transport::{
    initialize_params, map_rightkit_error, negotiate_version, rightkit_process_launcher,
    HttpTransport, McpTransport, SseTransport, StdioTransport, StubTransport, MCP_PROTOCOL_VERSION,
    MCP_SUPPORTED_VERSIONS,
};
pub use trust_gate::{
    approve_workspace_stdio, config_dir_segment, decide_workspace_stdio, default_trust_store_path,
    gate_workspace_stdio_servers, load_trust_store, pending_workspace_stdio,
    set_config_dir_segment, stdio_server_spec, PendingApproval, TrustDecision, TrustStore,
    DEFAULT_CONFIG_DIR_SEGMENT,
};
pub use types::{
    BearerTokenCommand, HttpConfig, McpChannelPermissions, McpConfigFile, McpExpose,
    McpOAuthConfig, McpPromptDef, McpResourceContent, McpResourceDef, McpServerConfig,
    McpServerOptions, McpToolAnnotations, McpToolDef, McpToolResult, McpTrustPolicy, SseConfig,
    StdioConfig,
};
