//! Error type for the MCP client layer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("transport error: {0}")]
    Transport(String),

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("resource not found: {0}")]
    ResourceNotFound(String),

    #[error("serde error: {0}")]
    Serde(String),

    #[error("keyring error: {0}")]
    Keyring(String),

    #[error("not connected")]
    NotConnected,

    #[error("MCP invocation cancelled")]
    Cancelled,

    /// The single total deadline for one tool call (lookup, listing, call and
    /// any transport retries) elapsed.
    #[error("MCP tool call '{tool}' timed out after {limit_ms} ms")]
    Timeout { tool: String, limit_ms: u64 },

    #[error(
        "SSE endpoint origin mismatch: server configured at '{configured_origin}' sent an endpoint URL on '{endpoint_origin}' — refusing to send credentials cross-origin. Set allow_cross_origin_endpoint=true in this server's MCP config if this is expected."
    )]
    CrossOriginEndpoint {
        configured_origin: String,
        endpoint_origin: String,
    },

    /// The tool exists on the server but the per-run `--tools` /
    /// `--exclude-tools` selection hides it.
    #[error("tool '{0}' is excluded by the tool-name filter")]
    ToolFiltered(String),

    #[error("child process exited: {0}")]
    ChildExited(String),

    #[error(
        "tool name collision for '{normalized}': servers {servers:?} collide on originals {originals:?}"
    )]
    Collision {
        normalized: String,
        servers: Vec<String>,
        originals: Vec<String>,
    },
}

impl McpError {
    /// Stable machine-readable kind for surfacing typed errors to the model.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Transport(_) => "transport",
            Self::ToolNotFound(_) => "tool_not_found",
            Self::ToolFiltered(_) => "tool_filtered",
            Self::ResourceNotFound(_) => "resource_not_found",
            Self::Serde(_) => "serde",
            Self::Keyring(_) => "keyring",
            Self::NotConnected => "not_connected",
            Self::Cancelled => "cancelled",
            Self::Timeout { .. } => "timeout",
            Self::CrossOriginEndpoint { .. } => "cross_origin_endpoint",
            Self::ChildExited(_) => "child_exited",
            Self::Collision { .. } => "collision",
        }
    }
}
