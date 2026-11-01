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

        let root = match rightkit_app_settings::agent_config::state_root() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::tool_trait::Tool;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use serde_json::json;
    use std::sync::{Mutex, MutexGuard};

    // Serializes HOME/USERPROFILE mutation across parallel #[tokio::test]; the
    // guard must stay held until the vars are restored (same pattern as
    // hooks::registry::ENV_LOCK).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_guard() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn mcp_registry_rejects_sanitized_collisions_deterministically() {
        let error =
            validate_mcp_registry_identities([("git-hub", "get.pr"), ("git--hub", "get..pr")])
                .expect_err("collision must fail closed");

        assert!(error.contains("mcp__git_hub__get_pr"));
        assert!(error.contains("git-hub"));
        assert!(error.contains("git--hub"));
    }

    #[test]
    fn mcp_registry_accepts_distinct_callable_names() {
        validate_mcp_registry_identities([("github", "get_pr"), ("linear", "get_issue")])
            .expect("distinct tools");
    }

    #[test]
    fn credential_filename_prevents_path_traversal() {
        assert_eq!(
            credential_filename("../GitHub MCP/server").as_deref(),
            Some("GitHub_MCP_server.json")
        );
        assert_eq!(credential_filename("...").as_deref(), None);
        assert_eq!(
            credential_filename("github").as_deref(),
            Some("github.json")
        );
    }

    #[test]
    fn restrict_permissions_accepts_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        std::fs::write(&path, "{}").unwrap();
        restrict_credential_file_permissions(&path).unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn mcp_auth_writes_sanitized_credential_file() {
        let _env = env_guard();
        let dir = tempfile::tempdir().unwrap();
        // Sibling home and suite root: on macOS the real data folder derives from HOME.
        let (home, suite) = (dir.path().join("home"), dir.path().join("suite"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&suite).unwrap();
        let previous_home = std::env::var("HOME").ok();
        let previous_userprofile = std::env::var("USERPROFILE").ok();
        let previous_host_home = std::env::var("RIGHTKIT_SUITE_ROOT").ok();
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&suite);

        let result = McpAuthTool
            .execute(
                json!({
                    "server": "../GitHub MCP/server",
                    "auth_type": "bearer",
                    "credential": "secret-token"
                }),
                &ToolContext::new(
                    "mcp-auth-test",
                    dir.path(),
                    PermissionMode::Bypass,
                    WorkspaceTrust::Trusted,
                ),
            )
            .await;

        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match previous_userprofile {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        match previous_host_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }

        assert!(!result.is_error, "{}", result.content);
        assert!(
            rightkit_app_settings::agent_config::test_isolation::app_root_in(&suite)
                .join("mcp-auth")
                .join("GitHub_MCP_server.json")
                .exists()
        );
    }

    /// A credential file written today must be owner-only on Unix. The
    /// previous mcp_auth implementation hand-rolled chmod; the new path
    /// uses rightkit_fs::write_private_file. This test asserts
    /// the on-disk result is identical (or stricter).
    #[cfg(unix)]
    #[tokio::test]
    async fn mcp_auth_credential_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let _env = env_guard();
        let dir = tempfile::tempdir().unwrap();
        // Sibling home and suite root: on macOS the real data folder derives from HOME.
        let (home, suite) = (dir.path().join("home"), dir.path().join("suite"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&suite).unwrap();
        let previous_home = std::env::var("HOME").ok();
        let previous_userprofile = std::env::var("USERPROFILE").ok();
        let previous_host_home = std::env::var("RIGHTKIT_SUITE_ROOT").ok();
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&suite);

        let result = McpAuthTool
            .execute(
                json!({
                    "server": "github",
                    "auth_type": "bearer",
                    "credential": "secret-token"
                }),
                &ToolContext::new(
                    "mcp-auth-test",
                    dir.path(),
                    PermissionMode::Bypass,
                    WorkspaceTrust::Trusted,
                ),
            )
            .await;

        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match previous_userprofile {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        match previous_host_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }

        assert!(!result.is_error, "{}", result.content);
        let path = rightkit_app_settings::agent_config::test_isolation::app_root_in(&suite)
            .join("mcp-auth")
            .join("github.json");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential must be owner-only");
    }

    /// If a credential file already exists with loose permissions, a
    /// re-authorize must still tighten it — the previous mcp_auth only
    /// chmod'd the freshly-written file. rightkit_fs::write_private_file
    /// tightens every write regardless of prior state.
    #[cfg(unix)]
    #[tokio::test]
    async fn mcp_auth_tightens_preexisting_credential_file() {
        use std::os::unix::fs::PermissionsExt;
        let _env = env_guard();
        let dir = tempfile::tempdir().unwrap();
        // Sibling home and suite root: on macOS the real data folder derives from HOME.
        let (home, suite) = (dir.path().join("home"), dir.path().join("suite"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&suite).unwrap();
        let previous_home = std::env::var("HOME").ok();
        let previous_userprofile = std::env::var("USERPROFILE").ok();
        let previous_host_home = std::env::var("RIGHTKIT_SUITE_ROOT").ok();
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&suite);

        // Pre-stage a credential file with loose perms as if a previous
        // (insecure) install had left it there.
        let cred_dir = rightkit_app_settings::agent_config::test_isolation::app_root_in(&suite)
            .join("mcp-auth");
        std::fs::create_dir_all(&cred_dir).unwrap();
        let target = cred_dir.join("github.json");
        std::fs::write(&target, "{\"server\":\"github\"}").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();

        let result = McpAuthTool
            .execute(
                json!({
                    "server": "github",
                    "auth_type": "bearer",
                    "credential": "secret-token"
                }),
                &ToolContext::new(
                    "mcp-auth-test",
                    dir.path(),
                    PermissionMode::Bypass,
                    WorkspaceTrust::Trusted,
                ),
            )
            .await;

        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match previous_userprofile {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        match previous_host_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }

        assert!(!result.is_error, "{}", result.content);
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "preexisting loose credential must be tightened"
        );
    }
}
