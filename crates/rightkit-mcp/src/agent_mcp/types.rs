//! Core data types for the MCP client layer.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Config for a single MCP server — supports HTTP (streamable), SSE, and stdio.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServerConfig {
    /// Streamable HTTP transport (stateful — each host app session sends Mcp-Session-Id).
    Http(HttpConfig),
    /// SSE transport (single-client per session; per-call mini-sessions for concurrency).
    Sse(SseConfig),
    /// stdio transport — spawn once per daemon, share across sessions.
    Stdio(StdioConfig),
}

/// Streamable HTTP server config.
///
/// Only `Authorization: Bearer <value>` is honoured. Any other header in
/// `http_headers` / `env_http_headers` is refused at load, because the HTTP
/// transport in this build sets no custom headers on `initialize`.
///
/// Credential sources are read from the environment of the process that loads
/// the config, which is the daemon. That environment is fixed when the daemon
/// starts. The resolved bearer is part of the pool key, so a new value gets a
/// new connection, but a running daemon still holds the old value. After
/// rotating a credential, run the host's stop command so the next command starts a
/// daemon with the new value.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpConfig {
    /// Full URL to the streamable HTTP endpoint, e.g. `http://127.0.0.1:9105/mcp`.
    pub url: String,
    /// Name of an environment variable holding the bearer credential. Wins over
    /// `http_headers` / `env_http_headers` and a stored OAuth credential.
    ///
    /// The variable is read from the daemon's environment, fixed at daemon
    /// start. After rotating the value, run the host's stop command so the next
    /// command starts a daemon that sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env_var: Option<String>,
    /// A helper command whose first output line is the bearer token, for a short-lived credential
    /// (a string split on whitespace, or an array of program and arguments). It runs directly,
    /// never through a shell, for at most ten seconds, in the daemon's environment; the token is
    /// trimmed, never logged and never part of an error. It runs again each time the server's
    /// configuration is loaded (every session start and `mcp test`), so a new token is picked up
    /// without a daemon restart. User config only: a repository config may not name one.
    /// Precedence: `bearer_token_env_var`, then this, then `Authorization` headers, then OAuth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_command: Option<BearerTokenCommand>,
    /// Static request headers. Only `Authorization: Bearer <value>` is supported;
    /// the value may use `${VAR}` placeholders, which are expanded from the
    /// daemon's environment at daemon start (see the type docs on rotation).
    #[serde(default, alias = "headers", skip_serializing_if = "HashMap::is_empty")]
    pub http_headers: HashMap<String, String>,
    /// Header name -> environment variable name supplying the header value.
    ///
    /// Each variable is read from the daemon's environment, fixed at daemon
    /// start. After rotating a value, run the host's stop command so the next command
    /// starts a daemon that sees it.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env_http_headers: HashMap<String, String>,
    #[serde(flatten)]
    pub options: McpServerOptions,
}

/// `http_headers` values may hold a bearer secret, so only header names print.
impl std::fmt::Debug for HttpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpConfig")
            .field("url", &self.url)
            .field("bearer_token_env_var", &self.bearer_token_env_var)
            .field("bearer_token_command", &self.bearer_token_command.is_some())
            .field(
                "http_headers",
                &self.http_headers.keys().collect::<Vec<_>>(),
            )
            .field("env_http_headers", &self.env_http_headers)
            .field("options", &self.options)
            .finish()
    }
}

/// The helper command of [`HttpConfig::bearer_token_command`].
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum BearerTokenCommand {
    /// `"python3 /path/token.py"`: split on whitespace into the program and its arguments.
    Line(String),
    /// `["python3", "/path/token.py"]`: the program and its arguments exactly.
    Argv(Vec<String>),
}

impl BearerTokenCommand {
    /// The program followed by its arguments; empty when the command names nothing.
    pub fn argv(&self) -> Vec<String> {
        match self {
            Self::Line(line) => line.split_whitespace().map(str::to_string).collect(),
            Self::Argv(argv) => argv.clone(),
        }
    }
}

/// Arguments can hold a secret, so nothing of the command prints.
impl std::fmt::Debug for BearerTokenCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BearerTokenCommand(..)")
    }
}

/// Per-server options shared by every transport kind.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct McpServerOptions {
    /// Bound on connecting and listing tools, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_timeout_sec: Option<f64>,
    /// Bound on one tool call, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_timeout_sec: Option<f64>,
    /// A session should not run when this server cannot start.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// When set, only these tools (exact names or `*` globs) are exposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,
    /// Tools never exposed; wins over `enabled_tools`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    /// How the model meets this server's tools: `direct` (each tool is in the model's tool
    /// list under its qualified name), `deferred` (the default: found with `tool_search` or
    /// `list_mcp_tools`, called with `mcp_call`) or `hidden` (not offered, not listed, not
    /// callable).
    #[serde(default, skip_serializing_if = "McpExpose::is_default")]
    pub expose: McpExpose,
}

/// How a server's tools are exposed to the model (see [`McpServerOptions::expose`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpExpose {
    Direct,
    #[default]
    Deferred,
    Hidden,
}

impl McpExpose {
    fn is_default(&self) -> bool {
        *self == Self::Deferred
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn timeout_from_secs(option: &str, value: Option<f64>) -> Option<std::time::Duration> {
    let secs = value?;
    if !secs.is_finite() || secs <= 0.0 {
        tracing::warn!(option, "ignoring non-positive or non-finite MCP timeout");
        return None;
    }
    match std::time::Duration::try_from_secs_f64(secs) {
        Ok(duration) => Some(duration),
        Err(_) => {
            tracing::warn!(option, "ignoring out-of-range MCP timeout");
            None
        }
    }
}

impl McpServerOptions {
    pub fn startup_timeout(&self) -> Option<std::time::Duration> {
        timeout_from_secs("startup_timeout_sec", self.startup_timeout_sec)
    }

    pub fn tool_timeout(&self) -> Option<std::time::Duration> {
        timeout_from_secs("tool_timeout_sec", self.tool_timeout_sec)
    }

    /// Filter over raw (unqualified) tool names; `None` when nothing is restricted.
    pub fn tool_filter(&self) -> Option<crate::agent_mcp::McpToolNameFilter> {
        // A hidden server offers nothing, and an explicit empty `enabled_tools` means "no tools",
        // not "all tools".
        if self.expose == McpExpose::Hidden
            || matches!(&self.enabled_tools, Some(list) if list.is_empty())
        {
            return Some(crate::agent_mcp::McpToolNameFilter {
                include: Vec::new(),
                exclude: vec!["*".to_string()],
            });
        }
        let filter = crate::agent_mcp::McpToolNameFilter {
            include: self.enabled_tools.clone().unwrap_or_default(),
            exclude: self.disabled_tools.clone(),
        };
        (!filter.is_empty()).then_some(filter)
    }
}

impl McpServerConfig {
    pub fn options(&self) -> &McpServerOptions {
        match self {
            Self::Http(c) => &c.options,
            Self::Sse(c) => &c.options,
            Self::Stdio(c) => &c.options,
        }
    }
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
    #[serde(flatten)]
    pub options: McpServerOptions,
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
    crate::agent_mcp::host::client_name().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StdioConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(flatten)]
    pub options: McpServerOptions,
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
#[derive(Clone, Serialize, Default)]
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
    /// Resolved HTTP bearer (or the reason it could not be resolved), per server.
    /// Filled by the loader; never read from or written to a config file.
    #[serde(skip)]
    pub http_bearer: HashMap<String, Result<Option<String>, String>>,
    #[serde(skip)]
    pub mcp_oauth_tokens: HashMap<String, crate::agent_mcp::oauth::McpOAuthTokens>,
}

/// Manual `Debug`: credential-bearing maps print their server names only.
impl std::fmt::Debug for McpConfigFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConfigFile")
            // Server configs can hold a literal Authorization header or stdio env secret.
            .field("mcp_servers", &self.mcp_servers.keys().collect::<Vec<_>>())
            .field("mcp_trust", &self.mcp_trust)
            .field("mcp_channel_permissions", &self.mcp_channel_permissions)
            .field("mcp_oauth", &self.mcp_oauth)
            .field(
                "mcp_oauth_tokens",
                &self.mcp_oauth_tokens.keys().collect::<Vec<_>>(),
            )
            .field("http_bearer", &self.http_bearer.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl<'de> Deserialize<'de> for McpConfigFile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        Self::from_value(raw, None).map_err(serde::de::Error::custom)
    }
}

impl McpConfigFile {
    /// Parse a config file, skipping a server entry that does not parse instead of failing the
    /// whole file. Returns the config and one message per skipped entry, naming it.
    pub fn from_json_lenient(text: &str) -> Result<(Self, Vec<String>), String> {
        let raw = serde_json::from_str::<Value>(text).map_err(|e| e.to_string())?;
        let mut skipped = Vec::new();
        let file = Self::from_value(raw, Some(&mut skipped))?;
        Ok((file, skipped))
    }

    fn from_value(mut raw: Value, mut skipped: Option<&mut Vec<String>>) -> Result<Self, String> {
        let mut mcp_oauth = raw
            .get("mcpOAuth")
            .cloned()
            .map(serde_json::from_value::<HashMap<String, McpOAuthConfig>>)
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();

        let mut mcp_servers = HashMap::new();
        if let Some(servers) = raw.get_mut("mcpServers").and_then(Value::as_object_mut) {
            for (name, server) in servers.iter_mut() {
                let entry = (|| -> Result<McpServerConfig, String> {
                    if let Some(oauth) = server.get("oauth").cloned() {
                        let parsed = serde_json::from_value::<McpOAuthConfig>(oauth)
                            .map_err(|e| e.to_string())?;
                        mcp_oauth.insert(name.clone(), parsed);
                        if let Some(server_obj) = server.as_object_mut() {
                            server_obj.remove("oauth");
                        }
                    }
                    // Other tools' files omit `type`: `command` means stdio, `url` means HTTP.
                    if let Some(obj) = server.as_object_mut() {
                        if !obj.contains_key("type") {
                            let inferred = if obj.contains_key("command") {
                                Some("stdio")
                            } else if obj.contains_key("url") {
                                Some("http")
                            } else {
                                None
                            };
                            if let Some(kind) = inferred {
                                obj.insert("type".to_string(), Value::String(kind.to_string()));
                            }
                        }
                    }
                    serde_json::from_value::<McpServerConfig>(server.take())
                        .map_err(|e| e.to_string())
                })();
                match (entry, skipped.as_deref_mut()) {
                    (Ok(config), _) => {
                        mcp_servers.insert(name.clone(), config);
                    }
                    (Err(e), Some(skipped)) => {
                        mcp_oauth.remove(name);
                        skipped.push(format!("server '{name}' skipped: {e}"));
                    }
                    (Err(e), None) => return Err(format!("server '{name}': {e}")),
                }
            }
        }

        #[derive(Deserialize)]
        struct RawConfig {
            #[serde(rename = "mcpTrust", default)]
            mcp_trust: HashMap<String, McpTrustPolicy>,
            #[serde(rename = "mcpChannelPermissions", default)]
            mcp_channel_permissions: HashMap<String, McpChannelPermissions>,
        }

        let raw = serde_json::from_value::<RawConfig>(raw).map_err(|e| e.to_string())?;
        Ok(Self {
            mcp_servers,
            mcp_trust: raw.mcp_trust,
            mcp_channel_permissions: raw.mcp_channel_permissions,
            http_bearer: HashMap::new(),
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

/// A prompt template an MCP server offers (`prompts/list`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpPromptDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Argument names, each with whether the server marks it required.
    #[serde(default)]
    pub arguments: Vec<(String, bool)>,
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
