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
/// Present only in `host-env` builds.
#[cfg(feature = "host-env")]
pub const WORKSPACE_TRUST_STORE_ENV: &str = "RIGHTKIT_WORKSPACE_TRUST_STORE";

static NEAREST_ANCESTOR_TRUST: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Select the nearest-ancestor rule set for this process (once per process; returns `false` if it
/// was already selected). Until it is called every function here behaves as it always did. After
/// it, for the whole process:
///
/// - [`workspace_trust_key`] trims, uses forward slashes, drops trailing separators (a bare root
///   such as `/` or `C:/` keeps its one) and folds to lower case on Windows;
/// - [`is_workspace_trusted_in`] reads the store with normalised keys and the nearest recorded
///   ancestor decides, so an `untrusted` record on a subfolder wins over a trusted parent;
/// - [`is_workspace_trusted`] also honours [`trust_workspace_for_this_process`], uses the store
///   [`workspace_trust_store_file`] names (no environment override, no working-directory
///   fallback), and a missing state root means not trusted.
///
/// A rule set with [`set_workspace_trust_rule`] still replaces [`is_workspace_trusted`] entirely.
pub fn set_nearest_ancestor_trust() -> bool {
    NEAREST_ANCESTOR_TRUST.set(()).is_ok()
}

fn nearest_ancestor_trust_selected() -> bool {
    NEAREST_ANCESTOR_TRUST.get().is_some()
}

/// Canonical key for a workspace root: trimmed, forward-slash separators.
/// Must stay identical to the daemon's `workspace_trust_key`.
///
/// After [`set_nearest_ancestor_trust`] the key is [`normalize_trust_key`] with Windows case
/// folding: no trailing separator (a bare root keeps its one) and lower case on Windows, whose
/// paths are case-insensitive.
pub fn workspace_trust_key(workspace_root: &str) -> String {
    if nearest_ancestor_trust_selected() {
        return normalize_trust_key(workspace_root, cfg!(windows));
    }
    workspace_root.trim().replace('\\', "/")
}

/// [`workspace_trust_key`] under the nearest-ancestor rules, with the case rule explicit so both
/// behaviours are testable on any host: trimmed, forward-slash separators, no trailing separator
/// (a bare root such as `/` or `C:/` keeps its one), folded to lower case when `fold_case`.
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

/// Keys to try, nearest first, for a workspace: its own normalised key, then each ancestor up to
/// the root (a folder is covered by the nearest ancestor with a record). The first key with a
/// record decides, so an explicit `untrusted` record on a subfolder wins over a trusted ancestor.
/// Empty for an empty root.
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

/// Location of the persisted workspace-trust store.
pub fn workspace_trust_store_path() -> PathBuf {
    #[cfg(feature = "host-env")]
    {
        if let Some(path) = std::env::var_os(WORKSPACE_TRUST_STORE_ENV) {
            return PathBuf::from(path);
        }
    }
    crate::state_root()
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(crate::state_dir_name())
        })
        .join("workspace-trust.json")
}

/// Location of the persisted workspace-trust store with no fallback: `<state root>/workspace-trust.json`,
/// or `None` when the state root does not resolve (a missing store means NOT trusted). It reads no
/// environment override. [`workspace_trust_store_path`] keeps its own signature and fallback.
pub fn workspace_trust_store_file() -> Option<PathBuf> {
    crate::state_root().map(|root| root.join("workspace-trust.json"))
}

/// The trust label of the nearest recorded ancestor of `workspace_root` in the store at
/// `store_path`: `None` when the store is missing, unreadable or malformed, or no record covers
/// the folder; `Some(true)` when that record is `trusted`. Stored keys are compared normalised
/// (see [`normalize_trust_key`]).
fn nearest_record_trusted(store_path: &Path, workspace_root: &str) -> Option<bool> {
    let raw = std::fs::read_to_string(store_path).ok()?;
    let store = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    let fold_case = cfg!(windows);
    let projects = store.get("projects").and_then(|p| p.as_object())?;
    let record = nearest_trust_record(workspace_root, |candidate| {
        projects
            .iter()
            .find(|(stored, _)| normalize_trust_key(stored, fold_case) == candidate)
            .map(|(_, record)| record)
    })?;
    Some(
        record
            .get("trust")
            .and_then(|trust| trust.as_str())
            .is_some_and(|trust| trust.trim().eq_ignore_ascii_case("trusted")),
    )
}

/// Whether `workspace_root` is recorded as `trusted` in the store at
/// `store_path`. Fails closed on every error. After [`set_nearest_ancestor_trust`] the nearest
/// recorded ancestor decides.
pub fn is_workspace_trusted_in(store_path: &Path, workspace_root: &str) -> bool {
    if nearest_ancestor_trust_selected() {
        return nearest_record_trusted(store_path, workspace_root) == Some(true);
    }
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

/// A host-supplied replacement for the default persisted-store lookup.
pub type WorkspaceTrustRule = fn(&Path) -> bool;

static HOST_TRUST_RULE: std::sync::OnceLock<WorkspaceTrustRule> = std::sync::OnceLock::new();

/// Set the host's workspace-trust rule, which replaces the default persisted-store lookup in
/// [`is_workspace_trusted`] (for a host with no trust-store file, such as an iOS app that keeps
/// the decision in its own settings). Call once at startup; returns `false` if a rule was
/// already set. Until it is called the persisted store decides, as before. The rule runs on
/// every check and must fail closed. [`is_workspace_trusted_in`] still reads the store it is given.
pub fn set_workspace_trust_rule(rule: WorkspaceTrustRule) -> bool {
    HOST_TRUST_RULE.set(rule).is_ok()
}

/// Whether `workspace_root` is trusted: by the host's rule when one was set with
/// [`set_workspace_trust_rule`], otherwise in the default persisted store.
///
/// After [`set_nearest_ancestor_trust`] (and with no host rule) the nearest-ancestor rules apply:
/// a workspace trusted with [`trust_workspace_for_this_process`] is trusted, and otherwise the
/// store [`workspace_trust_store_file`] names decides; with no state root nothing is trusted.
pub fn is_workspace_trusted(workspace_root: &Path) -> bool {
    if let Some(rule) = HOST_TRUST_RULE.get() {
        return rule(workspace_root);
    }
    if nearest_ancestor_trust_selected() {
        let text = workspace_root.to_string_lossy();
        let key = workspace_trust_key(&text);
        if !key.is_empty()
            && PROCESS_TRUSTED
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .contains(&key)
        {
            return true;
        }
        return workspace_trust_store_file()
            .is_some_and(|store| is_workspace_trusted_in(&store, &text));
    }
    is_workspace_trusted_in(
        &workspace_trust_store_path(),
        &workspace_root.to_string_lossy(),
    )
}

/// Workspaces trusted for THIS process only (a headless `--trust-workspace` run): never persisted,
/// never inherited by a child process or a daemon.
static PROCESS_TRUSTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Trust `workspace_root` for the rest of this process. The flag must also govern which config
/// layers load: an untrusted workspace may not supply provider routes, models, toolsets and the
/// like, so a run that was explicitly told to trust the workspace has to load them or it cannot
/// start. Honoured by [`is_workspace_trusted`] and [`is_workspace_registered`] once
/// [`set_nearest_ancestor_trust`] was called; before that [`is_workspace_trusted`] ignores it.
pub fn trust_workspace_for_this_process(workspace_root: &str) {
    let key = normalize_trust_key(workspace_root, cfg!(windows));
    if key.is_empty() {
        return;
    }
    let mut trusted = PROCESS_TRUSTED
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if !trusted.contains(&key) {
        trusted.push(key);
    }
}

/// Whether the user has made any trust decision (trusted or untrusted) for `workspace_root` or an
/// ancestor of it, in the persisted store, or trusted it for this process only. A folder nobody
/// ever registered with this host is not a workspace of it. Fails closed on every error. It reads
/// the store [`workspace_trust_store_file`] names and always applies the nearest-ancestor rules.
pub fn is_workspace_registered(workspace_root: &Path) -> bool {
    let text = workspace_root.to_string_lossy();
    let key = normalize_trust_key(&text, cfg!(windows));
    if key.is_empty() {
        return false;
    }
    if PROCESS_TRUSTED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .any(|trusted| trust_lookup_keys(&text).contains(trusted))
    {
        return true;
    }
    workspace_trust_store_file()
        .is_some_and(|store| nearest_record_trusted(&store, &text).is_some())
}
