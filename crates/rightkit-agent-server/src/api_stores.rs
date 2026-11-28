use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};

fn host_state_file(name: &str) -> PathBuf {
    rightkit_agent_config::state_root_required().join(name)
}

/// A store location the environment may move (feature `state-store-env`). The variable name each
/// one reads exists only with that feature: without it no name is compiled in and
/// [`state_store_override`] answers `None` for every key. No `Debug` derive: it would compile the
/// variant names into the binary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateStoreKey {
    Team,
    Message,
    Instincts,
    PermissionRule,
    PermissionDenial,
    WorkspaceTrust,
    TelegramBindings,
    PeerTeamStoreRoot,
    RunChangesDir,
}

#[cfg(feature = "state-store-env")]
impl StateStoreKey {
    /// The part of the variable name after the host's environment prefix.
    fn env_suffix(self) -> &'static str {
        match self {
            Self::Team => "TEAM_STORE",
            Self::Message => "MESSAGE_STORE",
            Self::Instincts => "INSTINCTS_STORE",
            Self::PermissionRule => "PERMISSION_RULE_STORE",
            Self::PermissionDenial => "PERMISSION_DENIAL_STORE",
            Self::WorkspaceTrust => "WORKSPACE_TRUST_STORE",
            Self::TelegramBindings => "TELEGRAM_BINDINGS",
            Self::PeerTeamStoreRoot => "PEER_TEAM_STORE_ROOT",
            Self::RunChangesDir => "RUN_CHANGES_DIR",
        }
    }
}

/// The file the host's `<env prefix>_<key>` variable names, when the host lets the environment
/// move its state stores (`host_options::state_store_paths_from_environment`) and the variable
/// is set. Feature `state-store-env` on.
#[cfg(feature = "state-store-env")]
pub(crate) fn state_store_override(key: StateStoreKey) -> Option<PathBuf> {
    if !crate::host_options::state_store_paths_from_environment() {
        return None;
    }
    std::env::var_os(rightkit_agent_config::env_name(key.env_suffix()))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Feature `state-store-env` off: the environment is never read, whatever the host option says.
#[cfg(not(feature = "state-store-env"))]
pub(crate) fn state_store_override(key: StateStoreKey) -> Option<PathBuf> {
    match key {
        StateStoreKey::Team
        | StateStoreKey::Message
        | StateStoreKey::Instincts
        | StateStoreKey::PermissionRule
        | StateStoreKey::PermissionDenial
        | StateStoreKey::WorkspaceTrust
        | StateStoreKey::TelegramBindings
        | StateStoreKey::PeerTeamStoreRoot
        | StateStoreKey::RunChangesDir => None,
    }
}

/// The store file for `name`, or the override file (see [`state_store_override`]).
fn store_path(key: StateStoreKey, name: &str) -> PathBuf {
    state_store_override(key).unwrap_or_else(|| host_state_file(name))
}

pub(crate) fn default_team_store_path() -> PathBuf {
    store_path(StateStoreKey::Team, "teams.json")
}

pub(crate) fn default_message_store_path() -> PathBuf {
    store_path(StateStoreKey::Message, "messages.json")
}

pub(crate) fn default_instincts_store_path() -> PathBuf {
    store_path(StateStoreKey::Instincts, "instincts.json")
}

pub(crate) fn default_permission_rule_store_path() -> PathBuf {
    store_path(StateStoreKey::PermissionRule, "permission-rules.json")
}

pub(crate) fn default_permission_denial_store_path() -> PathBuf {
    store_path(StateStoreKey::PermissionDenial, "permission-denials.json")
}

pub(crate) fn default_workspace_trust_store_path() -> PathBuf {
    store_path(StateStoreKey::WorkspaceTrust, "workspace-trust.json")
}

// The workspace trust store has one resolver. The file the API writes
// (`default_workspace_trust_store_path`) and every check in this crate read the same file:
// `rightkit_agent_config` reads `<state root>/workspace-trust.json` and knows no override, so
// when the override is in effect `install_workspace_trust_rule` makes its `is_workspace_trusted`
// read the override file (a trust granted for this process alone, which that crate keeps
// privately, then does not apply), and the two functions below cover what that rule does not:
// the file name and `is_workspace_registered`. Without feature `state-store-env` there is no
// override: the rule is not installed and both functions answer from `rightkit_agent_config`.

/// The workspace trust store file every reader uses: the override file when
/// `state_store_paths_from_environment` is on and the variable is set, else
/// `<state root>/workspace-trust.json` (`None` when there is no state root).
pub(crate) fn workspace_trust_store_file() -> Option<PathBuf> {
    state_store_override(StateStoreKey::WorkspaceTrust)
        .or_else(rightkit_agent_config::workspace_trust_store_file)
}

/// [`rightkit_agent_config::is_workspace_registered`] against the one trust store file: whether
/// a trust decision (trusted or not) is recorded for the folder or an ancestor of it.
#[cfg(feature = "state-store-env")]
pub(crate) fn is_workspace_registered(workspace_root: &FsPath) -> bool {
    let Some(store) = state_store_override(StateStoreKey::WorkspaceTrust) else {
        return rightkit_agent_config::is_workspace_registered(workspace_root);
    };
    let text = workspace_root.to_string_lossy();
    let Ok(raw) = std::fs::read_to_string(&store) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(projects) = parsed.get("projects").and_then(|p| p.as_object()) else {
        return false;
    };
    let fold_case = cfg!(windows);
    rightkit_agent_config::nearest_trust_record(&text, |candidate| {
        projects
            .iter()
            .find(|(stored, _)| {
                rightkit_agent_config::normalize_trust_key(stored, fold_case) == candidate
            })
            .map(|(_, record)| record)
    })
    .is_some()
}

/// Feature `state-store-env` off: there is no override file, so this is the configuration
/// crate's own answer.
#[cfg(not(feature = "state-store-env"))]
pub(crate) fn is_workspace_registered(workspace_root: &FsPath) -> bool {
    rightkit_agent_config::is_workspace_registered(workspace_root)
}

/// With an override in effect, make the configuration crate's own trust checks read the override
/// file: the rule it is given replaces its persisted-store lookup. The rule fails closed. Without
/// an override the configuration crate keeps its default lookup. Run once at start-up, before
/// any thread. A host that installed a rule of its own keeps it. Exists only with feature
/// `state-store-env`.
#[cfg(feature = "state-store-env")]
pub(crate) fn install_workspace_trust_rule() {
    if state_store_override(StateStoreKey::WorkspaceTrust).is_some() {
        rightkit_agent_config::set_workspace_trust_rule(|root| {
            state_store_override(StateStoreKey::WorkspaceTrust).is_some_and(|store| {
                rightkit_agent_config::is_workspace_trusted_in(&store, &root.to_string_lossy())
            })
        });
    }
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

/// What a store file looked like when this process last read it: enough to tell cheaply (one
/// `stat`, no read) that another process has replaced it since. Every writer replaces the file
/// by rename, so the file identity changes even when two writes land in one clock tick.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StoreFileStamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    file_id: u64,
}

/// The stamp of the file at `path`, `None` when there is no file.
fn store_file_stamp(path: &FsPath) -> Option<StoreFileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let file_id = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let file_id = 0;
    Some(StoreFileStamp {
        modified: meta.modified().ok(),
        len: meta.len(),
        file_id,
    })
}

fn store_file_stamps() -> &'static std::sync::Mutex<HashMap<PathBuf, Option<StoreFileStamp>>> {
    static STAMPS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Option<StoreFileStamp>>>> =
        std::sync::OnceLock::new();
    STAMPS.get_or_init(Default::default)
}

/// Records `stamp` as the last state of `path` this process read. Take the stamp BEFORE reading
/// the file: a replacement between the two is then seen as a change by the next check.
fn remember_store_file_stamp(path: &FsPath, stamp: Option<StoreFileStamp>) {
    store_file_stamps()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_path_buf(), stamp);
}

/// Whether `stamp` differs from the last one recorded for `path` (true when none was recorded).
fn store_file_stamp_changed(path: &FsPath, stamp: &Option<StoreFileStamp>) -> bool {
    store_file_stamps()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        != Some(stamp)
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
    remember_store_file_stamp(path, store_file_stamp(path));
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

/// Brings the held store up to date with the file. Two cases reload it:
/// - the last load of `path` failed: retry it, so a repaired file replaces the held store and
///   clears the recorded error;
/// - the file was replaced since this process last read it (another process of the host added or
///   removed a rule): its rules, denies included, replace the held copy.
///
/// A file that cannot be loaded keeps the held (last good or deny-all) store and records the
/// error. Returns the current unresolved error, if any. When nothing is flagged and the file is
/// unchanged this costs one `stat`.
///
/// The held store's mutex is kept for the whole check, as the writers keep it for their write,
/// so a reload can never put an older file back over a newer write of this process.
pub(crate) fn refresh_permission_rule_store(
    path: &FsPath,
    store: &std::sync::Mutex<PersistentPermissionRuleStore>,
) -> Option<PermissionRuleLoadError> {
    let mut guard = store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stamp = store_file_stamp(path);
    if permission_rule_store_load_error(path).is_none() && !store_file_stamp_changed(path, &stamp) {
        return None;
    }
    remember_store_file_stamp(path, stamp);
    let (next, error) = reload_permission_rule_store(path, &guard);
    *guard = next;
    error
}

/// Why [`update_permission_rule_store`] changed nothing.
#[derive(Debug)]
pub(crate) enum PermissionRuleUpdateError<E> {
    /// The change itself was refused (bad input, a rule that is no longer the one named).
    Rejected(E),
    /// The file could not be locked, read or written.
    Storage(String),
}

/// One change to the persisted permission rules, safe with several processes writing.
///
/// Under the cross-process lock the FILE is read again, `change` is applied to that copy (never
/// to this process's in-memory copy), the result is written atomically, and only then does it
/// become the held store. So a rule another process added is kept, a rule another process
/// removed stays removed, and a deny written elsewhere is never dropped by this write.
///
/// `change` gets the store as it is on disk (to modify) and the store this process held (what
/// its clients were last shown). `Ok(None)` means there is nothing to change: nothing is written.
/// Whenever the file was read, the held store is brought up to date with it, also on a refusal,
/// so the client's next listing shows what is really there. A failed write leaves the held store
/// as it was. A file that fails to load is refused and never replaced.
pub(crate) fn update_permission_rule_store<T, E>(
    path: &FsPath,
    held: &std::sync::Mutex<PersistentPermissionRuleStore>,
    change: impl FnOnce(
        &mut PersistentPermissionRuleStore,
        &PersistentPermissionRuleStore,
    ) -> Result<Option<T>, E>,
) -> Result<Option<T>, PermissionRuleUpdateError<E>> {
    use PermissionRuleUpdateError::{Rejected, Storage};
    if let Some(error) = permission_rule_store_load_error(path) {
        // Do not overwrite a file the operator still needs to repair with a store derived
        // from the fail-closed placeholder.
        return Err(Storage(format!(
            "refusing to write permission rules: {error}"
        )));
    }
    crate::store_write::with_store_lock(path, "permission-rules", || {
        // Lock order everywhere: file lock, then the held store's mutex.
        let mut held = held.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let stamp = store_file_stamp(path);
        let on_disk = try_load_permission_rule_store(path)
            .map_err(|error| Storage(format!("refusing to update permission rules: {error}")))?;
        let mut next = on_disk.clone();
        match change(&mut next, &held) {
            Ok(Some(value)) => {
                let raw = serde_json::to_string_pretty(&next)
                    .map_err(|error| Storage(error.to_string()))?;
                write_store_file(path, &raw).map_err(Storage)?;
                remember_store_file_stamp(path, store_file_stamp(path));
                *held = next;
                Ok(Some(value))
            }
            Ok(None) => {
                remember_store_file_stamp(path, stamp);
                *held = on_disk;
                Ok(None)
            }
            Err(refusal) => {
                remember_store_file_stamp(path, stamp);
                *held = on_disk;
                Err(Rejected(refusal))
            }
        }
    })
    .map_err(Storage)?
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

/// Reads a JSON store that may not exist yet. A missing file is `Ok(None)`; a file that cannot be
/// read or parsed is an error, so a corrupt store is never mistaken for an empty one (rule 10).
fn read_json_store<T: serde::de::DeserializeOwned>(path: &FsPath) -> Result<Option<T>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{} is unreadable: {error}", path.display())),
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| format!("{} is corrupt: {error}", path.display()))
}

pub(crate) fn load_permission_denial_store(path: &FsPath) -> Vec<PermissionDenialRow> {
    remember_store_file_stamp(path, store_file_stamp(path));
    match read_json_store(path) {
        Ok(rows) => rows.unwrap_or_default(),
        Err(error) => {
            tracing::error!(%error, "permission denial store failed to load; writes are refused until it is repaired");
            Vec::new()
        }
    }
}

/// How many denial rows and message rows a store keeps (the oldest are dropped).
const HISTORY_ROW_LIMIT: usize = 500;

fn keep_newest_rows<T>(rows: &mut Vec<T>) {
    if rows.len() > HISTORY_ROW_LIMIT {
        let keep_from = rows.len() - HISTORY_ROW_LIMIT;
        rows.drain(0..keep_from);
    }
}

/// Adds one denial row to the store, safe with several processes writing: under the
/// cross-process lock the file is read again, the row is appended to THAT list (unless a row
/// with the same non-empty correlation is already there) and the list is written atomically, so
/// rows other processes recorded are kept. The list as it now is on disk becomes the `held` one
/// before the lock is released, so two threads of this process cannot leave the older list held.
/// A corrupt file is an error and is never replaced (rule 10); `held` is then left as it was.
pub(crate) fn append_permission_denial_row(
    path: &FsPath,
    held: &std::sync::Mutex<Vec<PermissionDenialRow>>,
    row: PermissionDenialRow,
) -> Result<(), String> {
    crate::store_write::with_store_lock(path, "permission-denials", || {
        let stamp = store_file_stamp(path);
        let mut rows = read_json_store::<Vec<PermissionDenialRow>>(path)
            .map_err(|detail| format!("refusing to overwrite {detail}; repair or remove it first"))?
            .unwrap_or_default();
        let recorded = !row.correlation.is_empty()
            && rows
                .iter()
                .any(|existing| existing.correlation == row.correlation);
        if recorded {
            remember_store_file_stamp(path, stamp);
        } else {
            rows.push(row);
            keep_newest_rows(&mut rows);
            let raw = serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?;
            write_store_file(path, &raw)?;
            remember_store_file_stamp(path, store_file_stamp(path));
        }
        // Lock order everywhere: file lock, then the held list's mutex.
        *held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = rows;
        Ok(())
    })?
}

/// Replaces a held copy of a store with the file's when the file was replaced since this process
/// last looked (another process of the host wrote it). One `stat` when nothing changed. `load`
/// returns `None` for a file that is missing or cannot be used: the held copy is then kept.
///
/// The held copy's mutex is kept for the whole check, and every writer assigns its result to the
/// held copy before releasing the file lock, so a reload never leaves an older state held.
fn refresh_held_store<T>(
    path: &FsPath,
    held: &std::sync::Mutex<T>,
    load: impl FnOnce(&FsPath) -> Option<T>,
) {
    let mut guard = held.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let stamp = store_file_stamp(path);
    if !store_file_stamp_changed(path, &stamp) {
        return;
    }
    remember_store_file_stamp(path, stamp);
    if let Some(loaded) = load(path) {
        *guard = loaded;
    }
}

/// Brings the held denial rows up to date with rows other processes recorded
/// (see [`refresh_held_store`]). A missing or unreadable file keeps the held rows.
pub(crate) fn refresh_permission_denial_store(
    path: &FsPath,
    held: &std::sync::Mutex<Vec<PermissionDenialRow>>,
) {
    refresh_held_store(path, held, |path| read_json_store(path).ok().flatten());
}

/// Brings the held message history up to date with rows other processes recorded
/// (see [`refresh_held_store`]).
pub(crate) fn refresh_message_history_store(
    path: &FsPath,
    held: &std::sync::Mutex<Vec<MessageHistoryRow>>,
) {
    refresh_held_store(path, held, crate::store_write::load_or_set_aside);
}

/// Brings the held team presets up to date with teams other processes saved or removed
/// (see [`refresh_held_store`]).
pub(crate) fn refresh_team_store(
    path: &FsPath,
    held: &std::sync::Mutex<HashMap<String, rightkit_agent_coding_tools::TeamDefinition>>,
) {
    refresh_held_store(path, held, crate::store_write::load_or_set_aside);
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
    match read_json_store(path) {
        Ok(store) => store.unwrap_or_default(),
        Err(error) => {
            // Reads fail safe: an unreadable store grants no trust. Writes are refused.
            tracing::error!(%error, "workspace trust store failed to load; writes are refused until it is repaired");
            WorkspaceTrustStore::default()
        }
    }
}

/// Records one trust decision with a read-modify-write of the file under a cross-process lock, so
/// the daemon, the CLI and the TUI each keep the others' decisions (rule 12). A corrupt file is an
/// error and is never replaced (rule 10). Returns the merged store as written.
pub(crate) fn upsert_workspace_trust_record(
    path: &FsPath,
    key: String,
    record: WorkspaceTrustRecord,
) -> Result<WorkspaceTrustStore, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let options = rightkit_fs::lock::LockOptions::wait(std::time::Duration::from_secs(10))
        .labeled("workspace-trust");
    let approves = record.trust.trim().eq_ignore_ascii_case("trusted");
    let pin_scope = key.clone();
    rightkit_fs::lock::with_lock(FsPath::new(&lock_name), &options, || {
        let mut store = read_json_store::<WorkspaceTrustStore>(path)
            .map_err(|detail| format!("refusing to update {detail}; repair or remove it first"))?
            .unwrap_or_default();
        store.projects.insert(key, record);
        let raw = serde_json::to_string_pretty(&store).map_err(|e| e.to_string())?;
        write_store_file(path, &raw)?;
        if approves {
            clear_hook_set_pins(&pin_scope);
        }
        Ok(store)
    })
    .map_err(|error| format!("workspace trust store is locked: {error}"))?
}

fn clear_hook_set_pins(key: &str) {
    let Some(store) = rightkit_agent_config::definition_pins::pin_store_path("hook-sets.json")
    else {
        return;
    };
    rightkit_agent_config::definition_pins::clear_scopes_under(&store, key);
    if let Ok(real) = std::fs::canonicalize(key) {
        rightkit_agent_config::definition_pins::clear_scopes_under(
            &store,
            &rightkit_agent_config::workspace_trust_key(&real.to_string_lossy()),
        );
    }
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

/// A missing file is an empty history. A file that exists but cannot be read or parsed is set
/// aside and logged first (storage rule 14), so the empty history is never saved over it.
pub(crate) fn load_message_history_store(path: &FsPath) -> Vec<MessageHistoryRow> {
    crate::store_write::load_or_set_aside(path).unwrap_or_default()
}

fn write_message_history_store(path: &FsPath, rows: &[MessageHistoryRow]) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(rows).map_err(|e| e.to_string())?;
    write_store_file(path, &raw)
}

/// Adds one row to the message history, safe with several processes writing: under the
/// cross-process lock the file is read again, the row is appended to THAT list and the list is
/// written atomically, so rows other processes recorded are kept. A damaged file is set aside
/// first, as at load. The list as it now is on disk becomes the `held` one before the lock is
/// released; on an error `held` is left as it was.
pub(crate) fn append_message_history_row(
    path: &FsPath,
    held: &std::sync::Mutex<Vec<MessageHistoryRow>>,
    row: MessageHistoryRow,
) -> Result<(), String> {
    crate::store_write::with_store_lock(path, "messages", || {
        let mut rows = load_message_history_store(path);
        rows.push(row);
        keep_newest_rows(&mut rows);
        write_message_history_store(path, &rows)?;
        *held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = rows;
        Ok(())
    })?
}

/// One change to the team presets, safe with several processes writing: under the cross-process
/// lock the file is read again, `change` is applied to THAT map (one team added or removed) and
/// the map is written atomically, so teams other processes saved are kept. A damaged file is set
/// aside first, as at load. `change` returns whether to write. The map as it now is on disk
/// becomes the `held` one before the lock is released; on an error `held` is left as it was.
pub(crate) fn update_team_store(
    path: &FsPath,
    held: &std::sync::Mutex<HashMap<String, rightkit_agent_coding_tools::TeamDefinition>>,
    change: impl FnOnce(&mut HashMap<String, rightkit_agent_coding_tools::TeamDefinition>) -> bool,
) -> Result<(), String> {
    crate::store_write::with_store_lock(path, "teams", || {
        let mut teams = load_team_store(path);
        if change(&mut teams) {
            write_team_store(path, &teams)?;
        }
        *held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = teams;
        Ok(())
    })?
}

/// A missing file is an empty store. A file that exists but does not parse is
/// set aside first (`crate::store_write::set_aside_damaged`), so the empty
/// store the daemon starts with is never saved over the only copy of the team
/// presets (orchestration rule 23, CR-46).
pub(crate) fn load_team_store(
    path: &FsPath,
) -> HashMap<String, rightkit_agent_coding_tools::TeamDefinition> {
    crate::store_write::load_or_set_aside(path).unwrap_or_default()
}

fn write_team_store(
    path: &FsPath,
    teams: &HashMap<String, rightkit_agent_coding_tools::TeamDefinition>,
) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(teams).map_err(|e| e.to_string())?;
    write_store_file(path, &raw)
}
