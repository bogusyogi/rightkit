//! Per-agent MCP server configuration — `<state-dir>/agents/<name>.toml`.
//!
//! Each file declares the agent's role, optional model override, and a list of
//! MCP servers that should be available exclusively to that agent.
//!
//! ```toml
//! [agent]
//! role = "coder"
//! model = "deepseek/deepseek-v4-pro"
//!
//! [[mcp_server]]
//! name = "filesystem"
//! transport = "stdio"
//! command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
//!
//! [[mcp_server]]
//! name = "github"
//! transport = "sse"
//! url = "https://api.example.com/mcp"
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentMcpConfig {
    pub role: String,
    pub model: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<AgentMcpServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentMcpServer {
    pub name: String,
    /// Transport type: "stdio" | "sse" | "http"
    pub transport: String,
    /// Command argv for stdio transport, e.g. `["npx", "-y", "@mcp/github"]`.
    pub command: Option<Vec<String>>,
    /// URL for sse or http transports.
    pub url: Option<String>,
}

/// TOML file shape — wraps the fields under `[agent]` and `[[mcp_server]]`.
#[derive(Debug, Deserialize)]
struct AgentConfigFile {
    agent: AgentSection,
    #[serde(default, rename = "mcp_server")]
    mcp_server: Vec<AgentMcpServer>,
}

#[derive(Debug, Deserialize)]
struct AgentSection {
    role: String,
    model: Option<String>,
}

impl AgentMcpConfig {
    /// Load from `<state-dir>/agents/<role>.toml`. Returns `None` if the file
    /// does not exist; propagates parse errors.
    pub fn load(agents_dir: &Path, role: &str) -> Option<Self> {
        let path = agents_dir.join(format!("{role}.toml"));
        if !path.exists() {
            return None;
        }
        let raw = std::fs::read_to_string(&path).ok()?;
        let file: AgentConfigFile = toml::from_str(&raw).ok()?;
        Some(AgentMcpConfig {
            role: file.agent.role,
            model: file.agent.model,
            mcp_servers: file.mcp_server,
        })
    }

    /// Load and return a detailed error so callers can surface parse failures.
    pub fn load_or_err(agents_dir: &Path, role: &str) -> Result<Option<Self>, String> {
        let path = agents_dir.join(format!("{role}.toml"));
        if !path.exists() {
            return Ok(None);
        }
        let raw =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let file: AgentConfigFile =
            toml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        Ok(Some(AgentMcpConfig {
            role: file.agent.role,
            model: file.agent.model,
            mcp_servers: file.mcp_server,
        }))
    }
}

/// Resolve the agents directory for a workspace.
/// Prefers `<workspace>/<state-dir>/agents/`, falls back to `~/<state-dir>/agents/`.
pub fn find_agents_dir(workspace: &Path) -> PathBuf {
    let local = workspace.join(".rightkit").join("agents");
    if local.is_dir() {
        return local;
    }
    crate::state_root::state_root()
        .unwrap_or_else(|| PathBuf::from("/tmp").join(".rightkit"))
        .join("agents")
}
