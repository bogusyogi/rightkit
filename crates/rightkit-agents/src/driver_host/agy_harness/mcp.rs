//! User-level MCP registration for `agy`.
//!
//! Unlike Claude Code (which accepts a per-invocation `--mcp-config`), `agy`
//! only reads MCP servers from its own user-level config (`agy mcp
//! add|list|remove`). Exposing the the host app gateway therefore means writing
//! shared user state, so the registration is:
//!
//! - **uniquely named** (`host app`) and registered **idempotently**: a row
//!   named `host app` in `agy mcp list` is left untouched, never rewritten;
//! - **argument-minimal**: `agy mcp add --type stdio host app <host app
//!   exe> -- mcp-server`. No `--workspace` is baked in — `agy` starts MCP
//!   children in its own working directory, so the stdio server projects the
//!   turn's cwd — and no `--allow-mutate`, because a user-level entry cannot
//!   vary per turn or per permission mode. Vendor-native tools cover
//!   mutations under the vendor's own permission policy; the the host app
//!   server contributes its governed read-only tool surface.
//!
//! Consequence to be aware of: the registration is shared user config. An
//! `agy` invoked outside the host app will also see the `host app` server, and
//! the server's workspace follows whatever cwd that `agy` runs in.

use std::path::Path;
use std::time::Duration;

/// The MCP server name the host app registers inside agy's user config.
pub const HOST_MCP_SERVER_NAME: &str = "rightkit-agent";

const MCP_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Ensure `agy`'s user-level MCP config contains a `host app` stdio server
/// launching `host_exe mcp-server`.
///
/// Idempotent: if `agy mcp list` already shows a `host app` row the config
/// is left alone (it may have been customized deliberately). Any probe
/// failure — binary missing, timeout, nonzero exit, add rejection — is a
/// typed error; the caller decides whether MCP-less operation is acceptable.
pub fn ensure_host_mcp_registered(agy_path: &Path, host_exe: &Path) -> Result<(), String> {
    if host_mcp_is_registered(agy_path)? {
        return Ok(());
    }
    let argv =
        super::launch::mcp_add_argv(HOST_MCP_SERVER_NAME, host_exe, &["mcp-server".to_string()]);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let output = crate::discovery::run_bounded(agy_path, &argv, MCP_PROBE_TIMEOUT)
        .ok_or_else(|| "agy mcp add timed out or could not be launched".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "agy mcp add rightkit-agent failed ({}): {}",
            output.status,
            stderr.trim()
        ))
    }
}

/// Whether `agy mcp list` already names a `host app` server. The table is
/// whitespace-columned (`NAME TYPE STATUS COMMAND/URL`); the name column is
/// the first token of each non-header row.
fn host_mcp_is_registered(agy_path: &Path) -> Result<bool, String> {
    let output = crate::discovery::run_bounded(agy_path, &["mcp", "list"], MCP_PROBE_TIMEOUT)
        .ok_or_else(|| "agy mcp list timed out or could not be launched".to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "agy mcp list failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .any(|line| line.split_whitespace().next() == Some(HOST_MCP_SERVER_NAME)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn mcp_list_name_column_matching() {
        let table = "NAME      TYPE   STATUS   COMMAND/URL\n\
                     membrane  stdio  enabled  C:\\path\\membrane-client.exe stdio-mcp\n\
                     host stdio  enabled  C:\\path\\host.exe mcp-server\n";
        let registered = table
            .lines()
            .any(|line| line.split_whitespace().next() == Some(super::HOST_MCP_SERVER_NAME));
        assert!(registered);
        let absent = "NAME      TYPE   STATUS   COMMAND/URL\n\
                      membrane  stdio  enabled  x\n";
        assert!(!absent
            .lines()
            .any(|line| { line.split_whitespace().next() == Some(super::HOST_MCP_SERVER_NAME) }));
    }
}
