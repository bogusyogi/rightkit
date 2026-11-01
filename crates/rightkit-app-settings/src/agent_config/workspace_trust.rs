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

/// Environment override for the trust store location (shared with the daemon). Read only in
/// `test-mode` and unit-test builds (storage contract rule 17).
pub const WORKSPACE_TRUST_STORE_ENV: &str = "RIGHTKIT_WORKSPACE_TRUST_STORE";

/// Canonical key for a workspace root: trimmed, forward-slash separators, no trailing separator
/// (a bare root such as `/` or `C:/` keeps its one), and folded to lower case on Windows, whose
/// paths are case-insensitive (rule 11). A record on a folder covers its subfolders: lookup walks
/// up to the nearest recorded ancestor ([`trust_lookup_keys`], rule 60). The daemon, the CLI, the
/// TUI and every config loader use this one function.
pub fn workspace_trust_key(workspace_root: &str) -> String {
    normalize_trust_key(workspace_root, cfg!(windows))
}

/// [`workspace_trust_key`] with the case rule explicit, so both behaviours are testable on any host.
pub fn normalize_trust_key(workspace_root: &str, fold_case: bool) -> String {
    let mut key = workspace_root.trim().replace('\\', "/");
    while key.len() > 1 && key.ends_with('/') && !is_drive_root(&key) {
        key.pop();
    }
    if fold_case {
        key = key.to_lowercase();
    }
    key
}

/// Keys to try, nearest first, for a workspace: its own key, then each ancestor up to the root
/// (rule 60: a folder is covered by the nearest ancestor with a record). The first key with a
/// record decides, so an explicit `untrusted` record on a subfolder wins over a trusted
/// ancestor. Empty for an empty root.
pub fn trust_lookup_keys(workspace_root: &str) -> Vec<String> {
    trust_lookup_keys_with(workspace_root, cfg!(windows))
}

/// [`trust_lookup_keys`] with the case rule explicit.
pub fn trust_lookup_keys_with(workspace_root: &str, fold_case: bool) -> Vec<String> {
    let mut key = normalize_trust_key(workspace_root, fold_case);
    let mut keys = Vec::new();
    if key.is_empty() {
        return keys;
    }
    loop {
        keys.push(key.clone());
        let Some(slash) = key.rfind('/') else { break };
        // A POSIX root keeps its one separator; a drive root keeps `C:/`.
        let parent = if slash == 0 {
            "/".to_string()
        } else if key.as_bytes().get(slash.wrapping_sub(1)) == Some(&b':') && slash == 2 {
            key[..=slash].to_string()
        } else {
            key[..slash].to_string()
        };
        if parent == key || parent.is_empty() {
            break;
        }
        key = parent;
    }
    keys
}

/// The record of the nearest ancestor (or the folder itself) for which `lookup` finds one.
/// `lookup` receives normalised keys, nearest first.
pub fn nearest_trust_record<V>(
    workspace_root: &str,
    mut lookup: impl FnMut(&str) -> Option<V>,
) -> Option<V> {
    trust_lookup_keys(workspace_root)
        .iter()
        .find_map(|key| lookup(key))
}

fn is_drive_root(key: &str) -> bool {
    let bytes = key.as_bytes();
    bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

/// Location of the persisted workspace-trust store. `None` when the state root does not resolve:
/// there is no fallback location, and a missing store means NOT trusted.
pub fn workspace_trust_store_path() -> Option<PathBuf> {
    if let Some(path) = crate::agent_config::qa_path_override(WORKSPACE_TRUST_STORE_ENV) {
        return Some(path);
    }
    crate::agent_config::state_root().map(|root| root.join("workspace-trust.json"))
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
    // Stored keys are compared in normalised form, so a record written with a different case or a
    // trailing separator still matches on Windows (rule 11). The nearest recorded ancestor decides
    // (rule 60): an `untrusted` record on a subfolder wins over a trusted parent.
    let fold_case = cfg!(windows);
    let Some(projects) = store.get("projects").and_then(|p| p.as_object()) else {
        return false;
    };
    nearest_trust_record(workspace_root, |candidate| {
        projects
            .iter()
            .find(|(stored, _)| normalize_trust_key(stored, fold_case) == candidate)
            .map(|(_, record)| record)
    })
    .and_then(|record| record.get("trust"))
    .and_then(|trust| trust.as_str())
    .is_some_and(|trust| trust.trim().eq_ignore_ascii_case("trusted"))
}

/// Whether `workspace_root` is trusted in the default persisted store.
pub fn is_workspace_trusted(workspace_root: &Path) -> bool {
    workspace_trust_store_path()
        .is_some_and(|store| is_workspace_trusted_in(&store, &workspace_root.to_string_lossy()))
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
