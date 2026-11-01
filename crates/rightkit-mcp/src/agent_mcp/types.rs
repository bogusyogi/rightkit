//! Core data types for the MCP client layer.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Config for a single MCP server — supports HTTP (streamable), SSE, and stdio.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServerConfig {
    /// Streamable HTTP transport (stateful — each the host app session sends Mcp-Session-Id).
    Http(HttpConfig),
    /// SSE transport (single-client per session; per-call mini-sessions for concurrency).
    Sse(SseConfig),
    /// stdio transport — spawn once per daemon, share across sessions.
    Stdio(StdioConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpConfig {
    /// Full URL to the streamable HTTP endpoint, e.g. `http://127.0.0.1:9105/mcp`.
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SseConfig {
    /// Full URL to the SSE endpoint, e.g. `http://127.0.0.1:9100/sse`.
    /// The message endpoint is derived as `{base}/message`.
    pub url: String,
    /// Explicit opt-in required for a server whose `endpoint` event legitimately
    /// points at a different origin than `url`. Defaults to `false`: a
    /// cross-origin endpoint is rejected (blocks a malicious/compromised MCP
    /// server from redirecting the bearer token to an attacker-controlled host).
    #[serde(default)]
    pub allow_cross_origin_endpoint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConfig {
    pub authorization_url: String,
    pub token_url: String,
    #[serde(default)]
    pub revocation_url: Option<String>,
    #[serde(default = "default_oauth_client_id")]
    pub client_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// RFC 8707 resource indicator for authorization, token, and refresh requests.
    pub resource: String,
}

fn default_oauth_client_id() -> String {
    "rightkit-agent".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StdioConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpTrustPolicy {
    pub preset: String,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub approved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpChannelPermissions {
    #[serde(default)]
    pub allowed_channels: Vec<String>,
    #[serde(default)]
    pub allow_mutating: bool,
}

/// Top-level structure of a `.mcp.json` config file (Claude desktop compatible).
#[derive(Debug, Clone, Serialize, Default)]
pub struct McpConfigFile {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(rename = "mcpTrust", default)]
    pub mcp_trust: HashMap<String, McpTrustPolicy>,
    #[serde(rename = "mcpChannelPermissions", default)]
    pub mcp_channel_permissions: HashMap<String, McpChannelPermissions>,
    #[serde(
        rename = "mcpOAuth",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub mcp_oauth: HashMap<String, McpOAuthConfig>,
    #[serde(skip)]
    pub mcp_oauth_tokens: HashMap<String, crate::agent_mcp::oauth::McpOAuthTokens>,
}

impl<'de> Deserialize<'de> for McpConfigFile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut raw = Value::deserialize(deserializer)?;
        let mut mcp_oauth = raw
            .get("mcpOAuth")
            .cloned()
            .map(serde_json::from_value::<HashMap<String, McpOAuthConfig>>)
            .transpose()
            .map_err(serde::de::Error::custom)?
            .unwrap_or_default();

        if let Some(servers) = raw.get_mut("mcpServers").and_then(Value::as_object_mut) {
            for (name, server) in servers.iter_mut() {
                if let Some(oauth) = server.get("oauth").cloned() {
                    let parsed = serde_json::from_value::<McpOAuthConfig>(oauth)
                        .map_err(serde::de::Error::custom)?;
                    mcp_oauth.insert(name.clone(), parsed);
                    if let Some(server_obj) = server.as_object_mut() {
                        server_obj.remove("oauth");
                    }
                }
            }
        }

        #[derive(Deserialize)]
        struct RawConfig {
            #[serde(rename = "mcpServers", default)]
            mcp_servers: HashMap<String, McpServerConfig>,
            #[serde(rename = "mcpTrust", default)]
            mcp_trust: HashMap<String, McpTrustPolicy>,
            #[serde(rename = "mcpChannelPermissions", default)]
            mcp_channel_permissions: HashMap<String, McpChannelPermissions>,
        }

        let raw = serde_json::from_value::<RawConfig>(raw).map_err(serde::de::Error::custom)?;
        Ok(Self {
            mcp_servers: raw.mcp_servers,
            mcp_trust: raw.mcp_trust,
            mcp_channel_permissions: raw.mcp_channel_permissions,
            mcp_oauth,
            mcp_oauth_tokens: HashMap::new(),
        })
    }
}

/// A tool exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub annotations: McpToolAnnotations,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub read_only_hint: Option<bool>,
    #[serde(default)]
    pub destructive_hint: Option<bool>,
    #[serde(default)]
    pub idempotent_hint: Option<bool>,
    #[serde(default)]
    pub open_world_hint: Option<bool>,
}

impl McpToolDef {
    pub fn is_read_only_hint(&self) -> bool {
        self.annotations.read_only_hint == Some(true)
    }
}

/// The result of invoking an MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolResult {
    pub content: String,
    pub is_error: bool,
}

/// A resource exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResourceDef {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
}

/// The content returned when reading an MCP resource.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResourceContent {
    pub uri: String,
    /// Plain text content (for text/* MIME types).
    pub text: Option<String>,
    /// Base64-encoded content (for binary MIME types).
    pub blob: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
}
