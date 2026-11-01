use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};

fn coderight_state_file(name: &str) -> PathBuf {
    rightkit_app_settings::agent_config::state_root()
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".coderight")
        })
        .join(name)
}

pub(crate) fn default_team_store_path() -> PathBuf {
    std::env::var_os("RIGHTKIT_TEAM_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| coderight_state_file("teams.json"))
}

pub(crate) fn default_message_store_path() -> PathBuf {
    std::env::var_os("RIGHTKIT_MESSAGE_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| coderight_state_file("messages.json"))
}

pub(crate) fn default_instincts_store_path() -> PathBuf {
    std::env::var_os("RIGHTKIT_INSTINCTS_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| coderight_state_file("instincts.json"))
}

pub(crate) fn default_permission_rule_store_path() -> PathBuf {
    std::env::var_os("RIGHTKIT_PERMISSION_RULE_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| coderight_state_file("permission-rules.json"))
}

pub(crate) fn default_permission_denial_store_path() -> PathBuf {
    std::env::var_os("RIGHTKIT_PERMISSION_DENIAL_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| coderight_state_file("permission-denials.json"))
}

pub(crate) fn default_workspace_trust_store_path() -> PathBuf {
    std::env::var_os("RIGHTKIT_WORKSPACE_TRUST_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| coderight_state_file("workspace-trust.json"))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistentPermissionRuleStore {
    pub user: Vec<PersistentPermissionRule>,
    pub projects: HashMap<String, Vec<PersistentPermissionRule>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistentPermissionRule {
    pub pattern: String,
    pub effect: String,
}

/// Why the persisted permission rules could not be loaded. A rule file that exists but cannot
/// be read or parsed is never treated as an empty store: that would silently drop its denies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PermissionRuleLoadError {
    Unreadable { path: PathBuf, detail: String },
    Corrupt { path: PathBuf, detail: String },
}

impl std::fmt::Display for PermissionRuleLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, detail } => write!(
                f,
                "permission rule file {} is unreadable ({detail}); denying by default",
                path.display()
            ),
            Self::Corrupt { path, detail } => write!(
                f,
                "permission rule file {} is corrupt ({detail}); denying by default",
                path.display()
            ),
        }
    }
}

impl std::error::Error for PermissionRuleLoadError {}

impl PermissionRuleLoadError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Unreadable { .. } => "permission_rules_unreadable",
            Self::Corrupt { .. } => "permission_rules_corrupt",
        }
    }

    pub(crate) fn path(&self) -> &FsPath {
        match self {
            Self::Unreadable { path, .. } | Self::Corrupt { path, .. } => path,
        }
    }

    /// Typed `loadError` object for API responses.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code(),
            "message": self.to_string(),
            "path": self.path().display().to_string(),
        })
    }

    /// Human-readable degraded reason for status/health surfaces.
    pub(crate) fn degraded_reason(&self) -> String {
        format!(
            "permission rules unreadable: {}; all tools denied until repaired",
            self.path().display()
        )
    }
}

fn permission_rule_load_errors(
) -> &'static std::sync::Mutex<HashMap<PathBuf, PermissionRuleLoadError>> {
    static ERRORS: std::sync::OnceLock<
        std::sync::Mutex<HashMap<PathBuf, PermissionRuleLoadError>>,
    > = std::sync::OnceLock::new();
    ERRORS.get_or_init(Default::default)
}

fn record_permission_rule_load_result(path: &FsPath, error: Option<&PermissionRuleLoadError>) {
    let mut errors = permission_rule_load_errors()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match error {
        Some(error) => {
            errors.insert(path.to_path_buf(), error.clone());
        }
        None => {
            errors.remove(path);
        }
    }
}

/// The unresolved load error for `path`, if its last load failed. The daemon surfaces this
/// (status/diagnostics) so an operator learns that persisted rules are not in force.
pub(crate) fn permission_rule_store_load_error(path: &FsPath) -> Option<PermissionRuleLoadError> {
    permission_rule_load_errors()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        .cloned()
}

/// Strict load. A missing file is an empty store; anything unreadable or unparseable is an error.
pub(crate) fn try_load_permission_rule_store(
    path: &FsPath,
) -> Result<PersistentPermissionRuleStore, PermissionRuleLoadError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistentPermissionRuleStore::default());
        }
        Err(error) => {
            return Err(PermissionRuleLoadError::Unreadable {
                path: path.to_path_buf(),
                detail: error.to_string(),
            });
        }
    };
    serde_json::from_str(&raw).map_err(|error| PermissionRuleLoadError::Corrupt {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

/// Placeholder in force while the persisted rules cannot be loaded: everything is denied.
fn deny_all_permission_rule_store() -> PersistentPermissionRuleStore {
    PersistentPermissionRuleStore {
        user: vec![PersistentPermissionRule {
            pattern: "*".to_string(),
            effect: "deny".to_string(),
        }],
        projects: HashMap::new(),
    }
}

/// Startup load. Fails closed: on a corrupt or unreadable file the error is logged and recorded
/// (see [`permission_rule_store_load_error`]) and the returned store denies everything; the
/// corrupt content is never replaced by an empty (allow-by-default) store.
pub(crate) fn load_permission_rule_store(path: &FsPath) -> PersistentPermissionRuleStore {
    match try_load_permission_rule_store(path) {
        Ok(store) => {
            record_permission_rule_load_result(path, None);
            store
        }
        Err(error) => {
            tracing::error!(%error, "persisted permission rules failed to load");
            record_permission_rule_load_result(path, Some(&error));
            deny_all_permission_rule_store()
        }
    }
}

/// Reload over an already-loaded store: a corrupt file keeps `previous` (its last good denies)
/// and reports the error instead of replacing it.
pub(crate) fn reload_permission_rule_store(
    path: &FsPath,
    previous: &PersistentPermissionRuleStore,
) -> (
    PersistentPermissionRuleStore,
    Option<PermissionRuleLoadError>,
) {
    match try_load_permission_rule_store(path) {
        Ok(store) => {
            record_permission_rule_load_result(path, None);
            (store, None)
        }
        Err(error) => {
            tracing::error!(%error, "permission rule reload failed; keeping last good rules");
            record_permission_rule_load_result(path, Some(&error));
            (previous.clone(), Some(error))
        }
    }
}

/// If the last load of `path` failed, retry it: a repaired file replaces the held store and
/// clears the recorded error; a still-broken file keeps the held (last good or deny-all) store.
/// Returns the current unresolved error, if any. Cheap no-op when nothing is flagged.
pub(crate) fn refresh_permission_rule_store(
    path: &FsPath,
    store: &std::sync::Mutex<PersistentPermissionRuleStore>,
) -> Option<PermissionRuleLoadError> {
    permission_rule_store_load_error(path)?;
    let mut guard = store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (next, error) = reload_permission_rule_store(path, &guard);
    *guard = next;
    error
}

/// Writes a JSON store atomically (`rightkit-fs`: temp file, flush, rename; parents created), so a
/// crash mid-write leaves the previous file instead of a truncated one.
fn write_store_file(path: &FsPath, raw: &str) -> Result<(), String> {
    rightkit_fs::atomic_write_with(
        path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |file| std::io::Write::write_all(file, raw.as_bytes()),
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn write_permission_rule_store(
    path: &FsPath,
    store: &PersistentPermissionRuleStore,
) -> Result<(), String> {
    if let Some(error) = permission_rule_store_load_error(path) {
        // Do not overwrite a file the operator still needs to repair with a store derived
        // from the fail-closed placeholder.
        return Err(format!("refusing to write permission rules: {error}"));
    }
    let raw = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    write_store_file(path, &raw)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionDenialRow {
    pub id: String,
    pub created_at: String,
    pub session_id: String,
    pub tool_call_id: String,
    #[serde(default)]
    pub correlation: String,
    pub reason: String,
}

pub(crate) fn load_permission_denial_store(path: &FsPath) -> Vec<PermissionDenialRow> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub(crate) fn write_permission_denial_store(
    path: &FsPath,
    rows: &[PermissionDenialRow],
) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(rows).map_err(|e| e.to_string())?;
    write_store_file(path, &raw)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTrustStore {
    pub projects: HashMap<String, WorkspaceTrustRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTrustRecord {
    pub trust: String,
    pub approved_by: Option<String>,
    pub updated_at: String,
}

pub(crate) fn load_workspace_trust_store(path: &FsPath) -> WorkspaceTrustStore {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return WorkspaceTrustStore::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub(crate) fn write_workspace_trust_store(
    path: &FsPath,
    store: &WorkspaceTrustStore,
) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    write_store_file(path, &raw)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageHistoryRow {
    pub id: String,
    pub created_at: String,
    pub from_session_id: String,
    pub target_session_id: Option<String>,
    pub channel: String,
    pub message: String,
    pub delivery_status: String,
    pub delivered_session_ids: Vec<String>,
    pub error: Option<String>,
}

pub(crate) fn load_message_history_store(path: &FsPath) -> Vec<MessageHistoryRow> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub(crate) fn write_message_history_store(
    path: &FsPath,
    rows: &[MessageHistoryRow],
) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(rows).map_err(|e| e.to_string())?;
    write_store_file(path, &raw)
}

pub(crate) fn load_team_store(path: &FsPath) -> HashMap<String, rightkit_agent_tools::coding_tools::TeamDefinition> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub(crate) fn write_team_store(
    path: &FsPath,
    teams: &HashMap<String, rightkit_agent_tools::coding_tools::TeamDefinition>,
) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(teams).map_err(|e| e.to_string())?;
    write_store_file(path, &raw)
}

#[cfg(test)]
mod permission_rule_fail_closed_tests {
    use super::*;

    fn deny_store() -> PersistentPermissionRuleStore {
        PersistentPermissionRuleStore {
            user: vec![PersistentPermissionRule {
                pattern: "shell(rm *)".to_string(),
                effect: "deny".to_string(),
            }],
            projects: HashMap::new(),
        }
    }

    #[test]
    fn corrupt_file_after_good_load_keeps_previous_denies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.json");
        write_permission_rule_store(&path, &deny_store()).unwrap();
        let loaded = load_permission_rule_store(&path);
        assert_eq!(loaded.user.len(), 1);

        std::fs::write(&path, "{ not json").unwrap();
        let (kept, error) = reload_permission_rule_store(&path, &loaded);
        assert!(matches!(
            error,
            Some(PermissionRuleLoadError::Corrupt { .. })
        ));
        assert_eq!(kept.user.len(), 1);
        assert_eq!(kept.user[0].pattern, "shell(rm *)");
        assert_eq!(kept.user[0].effect, "deny");
        assert!(permission_rule_store_load_error(&path).is_some());
    }

    #[test]
    fn corrupt_file_at_startup_surfaces_error_and_denies_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.json");
        std::fs::write(&path, "\u{0}garbage").unwrap();
        assert!(matches!(
            try_load_permission_rule_store(&path),
            Err(PermissionRuleLoadError::Corrupt { .. })
        ));

        let store = load_permission_rule_store(&path);
        assert!(permission_rule_store_load_error(&path).is_some());
        assert_eq!(store.user.len(), 1);
        assert_eq!(store.user[0].pattern, "*");
        assert_eq!(store.user[0].effect, "deny");
        // The corrupt file is preserved for repair, not overwritten.
        assert!(write_permission_rule_store(&path, &store).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\u{0}garbage");
    }

    #[test]
    fn missing_file_is_an_empty_store_and_repair_clears_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.json");
        assert!(load_permission_rule_store(&path).user.is_empty());
        assert!(permission_rule_store_load_error(&path).is_none());

        std::fs::write(&path, "oops").unwrap();
        load_permission_rule_store(&path);
        assert!(permission_rule_store_load_error(&path).is_some());
        std::fs::write(&path, r#"{"user":[],"projects":{}}"#).unwrap();
        assert!(load_permission_rule_store(&path).user.is_empty());
        assert!(permission_rule_store_load_error(&path).is_none());
    }
}
