//! Account registry: API-key account entries and per-session / per-project
//! account selection.
//!
//! Secret boundary: this file never holds a secret. API-key account entries
//! carry only a label and provider; the key itself lives in the OS credential
//! store (see `rightkit_llm::agent_providers::client::credentials`). Subscription
//! accounts are not stored here at all: they are the managed CLI profiles under
//! `<state_root>/cli-profiles`, owned by the vendor CLI. Selections reference
//! accounts by id only.
//!
//! Account ids:
//! - subscription: `subscription/<provider>/<profile-label>`
//! - api key:      `api_key/<provider>/<slug>`
//!
//! There is deliberately no automatic switching: resolution returns the
//! session selection, else the project default, else `None`. A caller that
//! gets `None` asks the user; it never picks another account on a limit or
//! failure.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "accounts.json";
const LINKED_FILE_NAME: &str = "linked-profiles.json";
const SCHEMA_VERSION: u32 = 1;
const MAX_LABEL_CHARS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Subscription,
    ApiKey,
}

/// One API-key account. Holds no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyAccountEntry {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub slug: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelections {
    /// Project root path -> account id.
    #[serde(default)]
    pub project_defaults: BTreeMap<String, String>,
    /// Session id -> account id.
    #[serde(default)]
    pub sessions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsFile {
    pub version: u32,
    #[serde(default)]
    pub api_key_accounts: Vec<ApiKeyAccountEntry>,
    #[serde(default)]
    pub selections: AccountSelections,
}

impl Default for AccountsFile {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            api_key_accounts: Vec::new(),
            selections: AccountSelections::default(),
        }
    }
}

#[derive(Debug)]
pub enum AccountsError {
    Io(std::io::Error),
    Malformed(String),
    InvalidLabel(String),
    InvalidProvider(String),
    InvalidAccountId(String),
    NoStateRoot,
    UnknownAccount(String),
    /// A linked profile folder that cannot be linked (not absolute, missing, not a folder, or
    /// inside the host's own data folder).
    InvalidPath(String),
    /// A label already used by another account or linked profile.
    LabelInUse(String),
}

impl fmt::Display for AccountsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "accounts i/o: {e}"),
            Self::Malformed(e) => write!(f, "accounts file malformed: {e}"),
            Self::InvalidLabel(e) => write!(f, "invalid account label: {e}"),
            Self::InvalidProvider(e) => write!(f, "invalid provider: {e}"),
            Self::InvalidAccountId(e) => write!(f, "invalid account id: {e}"),
            Self::NoStateRoot => write!(f, "no {} state root available", crate::app_name()),
            Self::UnknownAccount(id) => write!(f, "unknown account: {id}"),
            Self::InvalidPath(why) => write!(f, "{why}"),
            Self::LabelInUse(label) => {
                write!(f, "the name \"{label}\" is already used by another account")
            }
        }
    }
}

impl std::error::Error for AccountsError {}

impl From<std::io::Error> for AccountsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub fn subscription_account_id(provider: &str, profile_label: &str) -> String {
    format!("subscription/{provider}/{profile_label}")
}

pub fn api_key_account_id(provider: &str, slug: &str) -> String {
    format!("api_key/{provider}/{slug}")
}

/// Classify an account id by shape. Returns `None` for malformed ids.
pub fn account_kind_of(id: &str) -> Option<AccountKind> {
    let mut parts = id.split('/');
    let kind = parts.next()?;
    let provider = parts.next()?;
    let tail = parts.next()?;
    if parts.next().is_some() || !valid_provider(provider) || !valid_slug(tail) {
        return None;
    }
    match kind {
        "subscription" => Some(AccountKind::Subscription),
        "api_key" => Some(AccountKind::ApiKey),
        _ => None,
    }
}

fn valid_provider(provider: &str) -> bool {
    !provider.is_empty()
        && provider.len() <= 32
        && provider
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= MAX_LABEL_CHARS
        && slug != "."
        && slug != ".."
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

fn slugify(label: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in label.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    trimmed
        .chars()
        .take(MAX_LABEL_CHARS - 4)
        .collect::<String>()
}

/// How long a registry writer waits for another writer of the same file.
const REGISTRY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The cross-process lock beside a registry file (`<file>.lock`), held for one read-change-write
/// of it. Several host processes share one state root: without the lock two of them read
/// the same file and the later save drops the earlier change. The default lock kind also
/// excludes other threads of this process. A lock that stays busy is an error, never a skip.
fn lock_registry(path: &Path) -> Result<rightkit_fs::FileLock, AccountsError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    rightkit_fs::FileLock::try_lock_with(
        Path::new(&lock_name),
        rightkit_fs::LockKind::default(),
        REGISTRY_LOCK_WAIT,
    )
    .map_err(|e| {
        AccountsError::Io(std::io::Error::other(format!(
            "the lock for {} could not be taken: {e}",
            path.display()
        )))
    })
}

/// A Claude profile folder the user already had, linked by path (never copied or moved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedProfile {
    pub provider: String,
    pub label: String,
    /// Absolute folder used as the vendor CLI's config folder (`CLAUDE_CONFIG_DIR`).
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedProfilesFile {
    pub version: u32,
    #[serde(default)]
    pub links: Vec<LinkedProfile>,
}

impl Default for LinkedProfilesFile {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            links: Vec::new(),
        }
    }
}

/// Providers whose existing profile folders can be linked. Codex is excluded: its login lives in
/// `auth.json`, which the host would have to copy into a run-scoped home to use it.
pub const LINKABLE_PROVIDERS: &[&str] = &["claude"];

/// Registry of linked profile folders, `<state_root>/linked-profiles.json`, beside `accounts.json`.
/// It holds only `provider`, `label` and a folder path. Written atomically with owner-only
/// permissions. Linking or unlinking never opens, writes, moves or deletes anything in the folder.
#[derive(Debug, Clone)]
pub struct LinkedProfileStore {
    path: PathBuf,
    state_root: Option<PathBuf>,
}

impl LinkedProfileStore {
    pub fn at(path: impl Into<PathBuf>, state_root: Option<PathBuf>) -> Self {
        Self {
            path: path.into(),
            state_root,
        }
    }

    pub fn default_location() -> Result<Self, AccountsError> {
        let root = crate::state_root().ok_or(AccountsError::NoStateRoot)?;
        Ok(Self::at(root.join(LINKED_FILE_NAME), Some(root)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<LinkedProfilesFile, AccountsError> {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => {
                serde_json::from_str(&raw).map_err(|e| AccountsError::Malformed(e.to_string()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LinkedProfilesFile::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, file: &LinkedProfilesFile) -> Result<(), AccountsError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(file)
            .map_err(|e| AccountsError::Malformed(e.to_string()))?;
        rightkit_fs::write_private_file(&self.path, body.as_bytes())
            .map_err(|e| AccountsError::Io(std::io::Error::other(e)))?;
        Ok(())
    }

    pub fn find(
        &self,
        provider: &str,
        label: &str,
    ) -> Result<Option<LinkedProfile>, AccountsError> {
        Ok(self
            .load()?
            .links
            .into_iter()
            .find(|l| l.provider == provider && l.label == label))
    }

    /// Check a folder is fit to link. Reads only metadata of the folder itself.
    pub fn validate_folder(&self, path: &str) -> Result<PathBuf, AccountsError> {
        let invalid = |m: &str| AccountsError::InvalidPath(m.to_string());
        let folder = PathBuf::from(path);
        if !folder.is_absolute() {
            return Err(invalid(
                "the folder must be a full path starting at the root",
            ));
        }
        let meta = std::fs::metadata(&folder).map_err(|_| invalid("that folder does not exist"))?;
        if !meta.is_dir() {
            return Err(invalid("that path is a file, not a folder"));
        }
        if let Some(root) = &self.state_root {
            let real = std::fs::canonicalize(&folder).unwrap_or_else(|_| folder.clone());
            let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            if real.starts_with(&real_root) {
                return Err(invalid(&format!(
                    "that folder is inside {}'s own data folder; link a folder you already had",
                    crate::app_name()
                )));
            }
        }
        Ok(folder)
    }

    /// Link `path` as `<provider>/<label>`. `label_in_use` tells whether a managed profile or
    /// account already owns the label (the caller knows the managed profiles).
    pub fn link(
        &self,
        provider: &str,
        label: &str,
        path: &str,
        label_in_use: bool,
    ) -> Result<LinkedProfile, AccountsError> {
        if !LINKABLE_PROVIDERS.contains(&provider) {
            return Err(AccountsError::InvalidProvider(format!(
                "{provider} (only {} profiles can be linked)",
                LINKABLE_PROVIDERS.join(", ")
            )));
        }
        if !valid_slug(label) {
            return Err(AccountsError::InvalidLabel(
                "use lowercase letters, digits, '-', '_' or '.' (up to 64)".into(),
            ));
        }
        let folder = self.validate_folder(path)?;
        let _lock = lock_registry(&self.path)?;
        let mut file = self.load()?;
        if label_in_use
            || file
                .links
                .iter()
                .any(|l| l.provider == provider && l.label == label)
        {
            return Err(AccountsError::LabelInUse(label.to_string()));
        }
        let entry = LinkedProfile {
            provider: provider.to_string(),
            label: label.to_string(),
            path: folder.to_string_lossy().into_owned(),
        };
        file.links.push(entry.clone());
        self.save(&file)?;
        Ok(entry)
    }

    /// Remove the link only. The folder is never touched.
    pub fn unlink(&self, provider: &str, label: &str) -> Result<LinkedProfile, AccountsError> {
        let _lock = lock_registry(&self.path)?;
        let mut file = self.load()?;
        let idx = file
            .links
            .iter()
            .position(|l| l.provider == provider && l.label == label)
            .ok_or_else(|| AccountsError::UnknownAccount(format!("{provider}/{label}")))?;
        let removed = file.links.remove(idx);
        self.save(&file)?;
        Ok(removed)
    }
}

/// File-backed account registry. Cheap to construct; every call re-reads the
/// file so the CLI, daemon and UI never disagree.
#[derive(Debug, Clone)]
pub struct AccountStore {
    path: PathBuf,
}

impl AccountStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<state_root>/accounts.json`.
    pub fn default_location() -> Result<Self, AccountsError> {
        let root = crate::state_root().ok_or(AccountsError::NoStateRoot)?;
        Ok(Self::at(root.join(FILE_NAME)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<AccountsFile, AccountsError> {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => {
                serde_json::from_str(&raw).map_err(|e| AccountsError::Malformed(e.to_string()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AccountsFile::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, file: &AccountsFile) -> Result<(), AccountsError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(file)
            .map_err(|e| AccountsError::Malformed(e.to_string()))?;
        rightkit_app_settings::atomic_write(&self.path, body.as_bytes())?;
        Ok(())
    }

    /// Register an API-key account. The caller stores the key in the OS
    /// credential store under the returned entry's id; this file never sees it.
    pub fn add_api_key_account(
        &self,
        label: &str,
        provider: &str,
    ) -> Result<ApiKeyAccountEntry, AccountsError> {
        let label = label.trim();
        if label.is_empty() {
            return Err(AccountsError::InvalidLabel(
                "label must not be empty".into(),
            ));
        }
        if label.chars().count() > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
            return Err(AccountsError::InvalidLabel(
                "label must be <= 64 chars with no control characters".into(),
            ));
        }
        if !valid_provider(provider) {
            return Err(AccountsError::InvalidProvider(provider.to_string()));
        }
        let base = slugify(label);
        if base.is_empty() {
            return Err(AccountsError::InvalidLabel(
                "label needs at least one letter or digit".into(),
            ));
        }
        let _lock = lock_registry(&self.path)?;
        let mut file = self.load()?;
        let mut slug = base.clone();
        let mut n = 2;
        while file
            .api_key_accounts
            .iter()
            .any(|a| a.provider == provider && a.slug == slug)
        {
            slug = format!("{base}-{n}");
            n += 1;
        }
        let entry = ApiKeyAccountEntry {
            id: api_key_account_id(provider, &slug),
            label: label.to_string(),
            provider: provider.to_string(),
            slug,
        };
        file.api_key_accounts.push(entry.clone());
        self.save(&file)?;
        Ok(entry)
    }

    /// Remove an API-key account and every selection that pointed at it.
    pub fn remove_api_key_account(&self, id: &str) -> Result<ApiKeyAccountEntry, AccountsError> {
        let _lock = lock_registry(&self.path)?;
        let mut file = self.load()?;
        let idx = file
            .api_key_accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or_else(|| AccountsError::UnknownAccount(id.to_string()))?;
        let removed = file.api_key_accounts.remove(idx);
        file.selections.project_defaults.retain(|_, v| v != id);
        file.selections.sessions.retain(|_, v| v != id);
        self.save(&file)?;
        Ok(removed)
    }

    /// Drop every selection pointing at `id` (used when a subscription
    /// profile is removed).
    pub fn clear_selections_for(&self, id: &str) -> Result<(), AccountsError> {
        let _lock = lock_registry(&self.path)?;
        let mut file = self.load()?;
        file.selections.project_defaults.retain(|_, v| v != id);
        file.selections.sessions.retain(|_, v| v != id);
        self.save(&file)
    }

    pub fn set_project_default(
        &self,
        project: &str,
        account_id: Option<&str>,
    ) -> Result<(), AccountsError> {
        self.set_selection(project, account_id, true)
    }

    pub fn set_session_account(
        &self,
        session_id: &str,
        account_id: Option<&str>,
    ) -> Result<(), AccountsError> {
        self.set_selection(session_id, account_id, false)
    }

    fn set_selection(
        &self,
        key: &str,
        account_id: Option<&str>,
        project: bool,
    ) -> Result<(), AccountsError> {
        if key.trim().is_empty() {
            return Err(AccountsError::InvalidAccountId(
                "empty selection key".into(),
            ));
        }
        if let Some(id) = account_id {
            if account_kind_of(id).is_none() {
                return Err(AccountsError::InvalidAccountId(id.to_string()));
            }
        }
        let _lock = lock_registry(&self.path)?;
        let mut file = self.load()?;
        let map = if project {
            &mut file.selections.project_defaults
        } else {
            &mut file.selections.sessions
        };
        match account_id {
            Some(id) => {
                map.insert(key.to_string(), id.to_string());
            }
            None => {
                map.remove(key);
            }
        }
        self.save(&file)
    }

    /// Session selection, else project default, else `None`. Never falls back
    /// to any other account.
    pub fn resolve(
        &self,
        session_id: Option<&str>,
        project: Option<&str>,
    ) -> Result<Option<String>, AccountsError> {
        let file = self.load()?;
        if let Some(id) = session_id.and_then(|s| file.selections.sessions.get(s)) {
            return Ok(Some(id.clone()));
        }
        Ok(project
            .and_then(|p| file.selections.project_defaults.get(p))
            .cloned())
    }
}
