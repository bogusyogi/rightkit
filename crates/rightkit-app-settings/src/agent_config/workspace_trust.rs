//! Fail-closed reader for the persisted workspace-trust decision.
//!
//! The daemon, the TUI and `host app trust` all persist one JSON store
//! (`<state root>/workspace-trust.json`, or `RIGHTKIT_WORKSPACE_TRUST_STORE`).
//! Project-scoped configuration (`.mcp.json`, `<state-dir>/mcp.json`,
//! workspace instruction files) must not take effect until the workspace is
//! explicitly trusted, so every loader of such configuration asks this module.
//!
//! Any missing store, unreadable file, malformed JSON, absent record or
//! non-`trusted` label means NOT trusted. There is no ambient default of
//! trusted.

use std::path::{Path, PathBuf};

/// Environment override for the trust store location (shared with the daemon).
/// Present only in `host-env` and test builds.
#[cfg(any(test, feature = "agent_config-host-env"))]
pub const WORKSPACE_TRUST_STORE_ENV: &str = "RIGHTKIT_WORKSPACE_TRUST_STORE";

/// Canonical key for a workspace root: trimmed, forward-slash separators.
/// Must stay identical to the daemon's `workspace_trust_key`.
pub fn workspace_trust_key(workspace_root: &str) -> String {
    workspace_root.trim().replace('\\', "/")
}

/// Location of the persisted workspace-trust store.
pub fn workspace_trust_store_path() -> PathBuf {
    #[cfg(any(test, feature = "agent_config-host-env"))]
    {
        if let Some(path) = std::env::var_os(WORKSPACE_TRUST_STORE_ENV) {
            return PathBuf::from(path);
        }
    }
    crate::agent_config::state_root()
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".rightkit")
        })
        .join("workspace-trust.json")
}

/// Whether `workspace_root` is recorded as `trusted` in the store at
/// `store_path`. Fails closed on every error.
pub fn is_workspace_trusted_in(store_path: &Path, workspace_root: &str) -> bool {
    let key = workspace_trust_key(workspace_root);
    if key.is_empty() {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(store_path) else {
        return false;
    };
    let Ok(store) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    store
        .get("projects")
        .and_then(|projects| projects.get(&key))
        .and_then(|record| record.get("trust"))
        .and_then(|trust| trust.as_str())
        .is_some_and(|trust| trust.trim().eq_ignore_ascii_case("trusted"))
}

/// Whether `workspace_root` is trusted in the default persisted store.
pub fn is_workspace_trusted(workspace_root: &Path) -> bool {
    is_workspace_trusted_in(
        &workspace_trust_store_path(),
        &workspace_root.to_string_lossy(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("workspace-trust.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn missing_store_is_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_workspace_trusted_in(
            &dir.path().join("absent.json"),
            "C:/work/repo"
        ));
    }

    #[test]
    fn malformed_store_is_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_with(dir.path(), "{not json");
        assert!(!is_workspace_trusted_in(&path, "C:/work/repo"));
    }

    #[test]
    fn only_trusted_label_for_exact_key_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_with(
            dir.path(),
            r#"{"projects":{
                "C:/work/repo":{"trust":"trusted","approvedBy":"user","updatedAt":"x"},
                "C:/work/sandboxed":{"trust":"sandboxed","approvedBy":null,"updatedAt":"x"},
                "C:/work/locked":{"trust":"locked","approvedBy":null,"updatedAt":"x"},
                "C:/work/untrusted":{"trust":"untrusted","approvedBy":null,"updatedAt":"x"}
            }}"#,
        );
        // Backslash separators normalise to the stored key.
        assert!(is_workspace_trusted_in(&path, "C:\\work\\repo"));
        assert!(is_workspace_trusted_in(&path, "  C:/work/repo  "));
        assert!(!is_workspace_trusted_in(&path, "C:/work/sandboxed"));
        assert!(!is_workspace_trusted_in(&path, "C:/work/locked"));
        assert!(!is_workspace_trusted_in(&path, "C:/work/untrusted"));
        assert!(!is_workspace_trusted_in(&path, "C:/work/other"));
        assert!(!is_workspace_trusted_in(&path, ""));
    }
}
