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
    /// Command argv for stdio transport, e.g. ["npx", "-y", "@mcp/github"].
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
    crate::agent_config::state_root::state_root()
        .unwrap_or_else(|| PathBuf::from("/tmp").join(".rightkit"))
        .join("agents")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn write_toml(dir: &Path, name: &str, content: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn load_full_config() {
        let dir = tempdir().unwrap();
        write_toml(
            dir.path(),
            "coder.toml",
            r#"
[agent]
role = "coder"
model = "deepseek/deepseek-v4-pro"

[[mcp_server]]
name = "filesystem"
transport = "stdio"
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/workspace"]

[[mcp_server]]
name = "github"
transport = "sse"
url = "https://api.example.com/mcp"
"#,
        );

        let cfg = AgentMcpConfig::load(dir.path(), "coder").unwrap();
        assert_eq!(cfg.role, "coder");
        assert_eq!(cfg.model.as_deref(), Some("deepseek/deepseek-v4-pro"));
        assert_eq!(cfg.mcp_servers.len(), 2);

        let fs = &cfg.mcp_servers[0];
        assert_eq!(fs.name, "filesystem");
        assert_eq!(fs.transport, "stdio");
        assert_eq!(
            fs.command.as_ref().unwrap(),
            &[
                "npx",
                "-y",
                "@modelcontextprotocol/server-filesystem",
                "/workspace"
            ]
        );
        assert!(fs.url.is_none());

        let gh = &cfg.mcp_servers[1];
        assert_eq!(gh.name, "github");
        assert_eq!(gh.transport, "sse");
        assert_eq!(gh.url.as_deref(), Some("https://api.example.com/mcp"));
        assert!(gh.command.is_none());
    }

    #[test]
    fn load_no_model_no_servers() {
        let dir = tempdir().unwrap();
        write_toml(
            dir.path(),
            "planner.toml",
            r#"
[agent]
role = "planner"
"#,
        );
        let cfg = AgentMcpConfig::load(dir.path(), "planner").unwrap();
        assert_eq!(cfg.role, "planner");
        assert!(cfg.model.is_none());
        assert!(cfg.mcp_servers.is_empty());
    }

    #[test]
    fn load_missing_file_returns_none() {
        let dir = tempdir().unwrap();
        assert!(AgentMcpConfig::load(dir.path(), "nonexistent").is_none());
    }

    #[test]
    fn load_or_err_missing_file_returns_ok_none() {
        let dir = tempdir().unwrap();
        assert!(AgentMcpConfig::load_or_err(dir.path(), "nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn load_or_err_bad_toml_returns_err() {
        let dir = tempdir().unwrap();
        write_toml(dir.path(), "bad.toml", "this = = bad");
        let err = AgentMcpConfig::load_or_err(dir.path(), "bad");
        assert!(err.is_err());
    }

    #[test]
    fn find_agents_dir_local_workspace() {
        let dir = tempdir().unwrap();
        let agents = dir.path().join(".rightkit").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        assert_eq!(find_agents_dir(dir.path()), agents);
    }

    #[test]
    fn find_agents_dir_falls_back_to_home() {
        let dir = tempdir().unwrap();
        let result = find_agents_dir(dir.path());
        // Falls back to the RightKit-resolved state root (not a literal ~/<state-dir>).
        assert_eq!(
            result,
            crate::agent_config::state_root::state_root()
                .unwrap_or_else(|| PathBuf::from("/tmp").join(".rightkit"))
                .join("agents")
        );
    }

    #[test]
    fn http_transport_config() {
        let dir = tempdir().unwrap();
        write_toml(
            dir.path(),
            "worker.toml",
            r#"
[agent]
role = "worker"

[[mcp_server]]
name = "api"
transport = "http"
url = "http://127.0.0.1:9105/mcp"
"#,
        );
        let cfg = AgentMcpConfig::load(dir.path(), "worker").unwrap();
        assert_eq!(cfg.mcp_servers[0].transport, "http");
        assert_eq!(
            cfg.mcp_servers[0].url.as_deref(),
            Some("http://127.0.0.1:9105/mcp")
        );
    }
}
