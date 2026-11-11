//! `mcp_auth` — store credentials for an MCP server.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_fs::{ensure_private_dir, write_private_file};
use serde_json::{json, Value};
use std::path::Path;

pub fn validate_mcp_registry_identities<'a, I>(identities: I) -> Result<(), String>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut seen = std::collections::BTreeMap::<String, (String, String)>::new();
    for (server, tool) in identities {
        let callable = rightkit_mcp::agent_mcp::sanitize_tool_name(server, tool);
        if callable.trim().is_empty() {
            return Err(format!(
                "MCP tool identity is empty after sanitization: server='{server}', tool='{tool}'"
            ));
        }
        if let Some((prior_server, prior_tool)) =
            seen.insert(callable.clone(), (server.to_string(), tool.to_string()))
        {
            return Err(format!("MCP tool name collision for '{callable}': server='{prior_server}', tool='{prior_tool}' conflicts with server='{server}', tool='{tool}'"));
        }
    }
    Ok(())
}

pub struct McpAuthTool;

#[async_trait::async_trait]
impl Tool for McpAuthTool {
    fn name(&self) -> &str {
        "mcp_auth"
    }

    fn description(&self) -> &str {
        "Authorize an MCP server by storing its credentials in \
         ~/.host/mcp-auth/<server>.json (owner-only via rightkit-fs). \
         Supported auth_type values: 'bearer', 'api_key'. \
         Returns 'Authorized MCP server: {server}' on success."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["server", "auth_type", "credential"],
            "properties": {
                "server": {
                    "type": "string",
                    "description": "MCP server name/identifier."
                },
                "auth_type": {
                    "type": "string",
                    "description": "Auth scheme: 'bearer' or 'api_key'."
                },
                "credential": {
                    "type": "string",
                    "description": "The credential value (token or API key)."
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let server = match args["server"].as_str() {
            Some(s) => s,
            None => return ToolResult::error("mcp_auth: 'server' is required"),
        };
        let auth_type = match args["auth_type"].as_str() {
            Some(a) => a,
            None => return ToolResult::error("mcp_auth: 'auth_type' is required"),
        };
        let credential = match args["credential"].as_str() {
            Some(c) => c,
            None => return ToolResult::error("mcp_auth: 'credential' is required"),
        };

        let root = match rightkit_agent_config::state_root() {
            Some(root) => root,
            None => return ToolResult::error("mcp_auth: neither HOME nor USERPROFILE is set"),
        };
        let dir = root.join("mcp-auth");

        if let Err(e) = ensure_private_dir(&dir) {
            return ToolResult::error(format!("mcp_auth: failed to create auth dir: {e}"));
        }

        let filename = match credential_filename(server) {
            Some(filename) => filename,
            None => {
                return ToolResult::error(
                    "mcp_auth: 'server' must contain at least one ASCII letter or digit",
                )
            }
        };
        let path = dir.join(&filename);
        let payload = json!({
            "server": server,
            "auth_type": auth_type,
            "credential": credential,
        });

        // Atomic private write: write to a sibling temp file, fsync,
        // rename onto the destination, then narrow to owner-only.
        // A crash mid-write leaves either the previous file or an
        // orphan temp; never a half-written credential.
        if let Err(e) = write_private_file(&path, payload.to_string().as_bytes()) {
            return ToolResult::error(format!("mcp_auth: failed to write auth file: {e}"));
        }

        ToolResult::ok(format!("Authorized MCP server: {server}"))
    }
}

fn credential_filename(server: &str) -> Option<String> {
    let mut out = String::with_capacity(server.len() + 5);
    let mut saw_alnum = false;
    for ch in server.chars() {
        if ch.is_ascii_alphanumeric() {
            saw_alnum = true;
            out.push(ch);
        } else if matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if !saw_alnum {
        return None;
    }
    while out.contains("..") {
        out = out.replace("..", ".");
    }
    let out = out
        .trim_matches(|ch| ch == '.' || ch == '_' || ch == '-')
        .to_string();
    if out.is_empty() {
        None
    } else {
        Some(format!("{out}.json"))
    }
}

/// Thin wrapper kept for backward-compat with the existing tests in
/// this module. The actual permission narrowing is owned by
/// [`rightkit_fs::restrict_owner_only`].
#[allow(dead_code)]
fn restrict_credential_file_permissions(path: &Path) -> Result<(), String> {
    rightkit_fs::restrict_owner_only(path).map_err(|e| e.to_string())
}
