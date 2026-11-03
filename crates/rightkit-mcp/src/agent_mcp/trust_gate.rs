//! First-use trust gate for workspace-configured MCP servers (audit A-6).
//!
//! A workspace `.mcp.json` can declare a stdio server whose `command` is spawned as a normal
//! child process, so each `(workspace, server)` is gated on an approval bound to a fingerprint
//! of exactly what would run. The store, fingerprint and decision rules are RightKit's
//! (`crate::TrustStore`); this module only maps the host app's `StdioConfig` onto
//! `ServerSpec` and names the on-disk location.
//!
//! User-level and built-in servers are exempt; only workspace `.mcp.json` servers are gated.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::ServerSpec;
pub use crate::{PendingApproval, TrustDecision, TrustStore};

use crate::agent_mcp::types::StdioConfig;

/// The `ServerSpec` RightKit fingerprints for a workspace stdio server (no working directory:
/// the daemon spawns it in the workspace).
pub fn stdio_server_spec(config: &StdioConfig) -> ServerSpec {
    ServerSpec::Stdio {
        command: config.command.clone(),
        args: config.args.clone(),
        env: config
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
        cwd: None,
    }
}

/// Decide whether a workspace stdio server may spawn right now.
pub fn decide_workspace_stdio(
    store: &TrustStore,
    workspace: &str,
    server_name: &str,
    config: &StdioConfig,
) -> TrustDecision {
    store.gate_stdio(workspace, server_name, &stdio_server_spec(config))
}

/// Load the trust store. A missing file is an empty store; an unreadable or corrupt file also
/// yields an empty store here, so every workspace server stays gated (`Pending`). The caller
/// must not `save` such a store over the file.
pub fn load_trust_store(path: &Path) -> TrustStore {
    match TrustStore::try_load(path) {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "[mcp] trust store {} unreadable ({error}); workspace servers stay gated",
                path.display()
            );
            TrustStore::new()
        }
    }
}

/// Default on-disk location for trust decisions, next to `mcp.json`.
pub fn default_trust_store_path(home: Option<&Path>) -> PathBuf {
    let home = home.map(Path::to_path_buf).unwrap_or_default();
    home.join(".config")
        .join("rightkit-agent")
        .join("mcp_trust.json")
}

/// Decide every workspace stdio server against `store`, returning the names that may spawn and
/// the pending approvals. `workspace_stdio_servers` must hold ONLY servers sourced from the
/// workspace `.mcp.json`.
pub fn gate_workspace_stdio_servers(
    store: &TrustStore,
    workspace: &str,
    workspace_stdio_servers: &std::collections::HashMap<String, StdioConfig>,
) -> (Vec<String>, Vec<PendingApproval>) {
    let mut approved_names = Vec::new();
    let mut pending = Vec::new();
    let mut names: Vec<&String> = workspace_stdio_servers.keys().collect();
    names.sort();
    for name in names {
        let spec = stdio_server_spec(&workspace_stdio_servers[name]);
        match store.gate_stdio(workspace, name, &spec) {
            TrustDecision::Approved => approved_names.push(name.clone()),
            TrustDecision::Denied => {}
            TrustDecision::Pending => pending.push(store.pending_approval(workspace, name, &spec)),
        }
    }
    (approved_names, pending)
}
