//! Providers R4, R10, R33, R46: where a provider key lives on a host.
//!
//! One probe at start decides whether the host has an OS credential store (a no-backend open
//! means none; a locked, unreadable or empty store means present). The decision is never made by
//! a switch.
//!
//! * **Store present** (the desktop): subscription sign-in, then the OS credential store. The
//!   process environment and `<state root>/.env` are not read.
//! * **No store** (a server, a throwaway container, CI): subscription sign-in, then the process
//!   environment, then `<state root>/.env`. Both are read into memory once, at start. Nothing is
//!   exported: the names are removed from this process's own environment after the read, so no
//!   later child can inherit them, and [`is_credential_env_name`] names what the one launcher
//!   removes from every child (R46).
//!
//! `.env` holds only the names this module lists, plus route variables (`RIGHTKIT_<NAME>_API_KEY`,
//! the names a `provider-routes.*.api-key-env` may point at), one `NAME=value` per line. A file with group or world access is refused with
//! the remedy named. A value never appears in an error, a `Debug` print or a log line.
//!
//! The GitHub token names are read from `.env` only, never from the environment (R33: the product
//! uses its own GitHub connection, not a token from the terminal).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[cfg(feature = "agent_providers-os-keyring")]
use std::sync::{Arc, OnceLock};

/// File name of the key file inside the state root.
pub const DOTENV_FILE: &str = ".env";

/// The GitHub token names R33 and R4 list. Read from `.env` only.
pub const GITHUB_TOKEN_NAMES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// The web-search key name (storage rule 32).
pub const WEB_SEARCH_KEY_NAME: &str = "RIGHTKIT_WEB_SEARCH_API_KEY";

/// Daemon and driver-gateway secrets that no child process receives (R46).
pub const DAEMON_SECRET_NAMES: [&str; 3] = [
    "RIGHTKIT_DAEMON_TOKEN",
    "RIGHTKIT_GATEWAY_TOKEN",
    "RIGHTKIT_DRIVER_INSTANCE_SECRET",
];

/// Names this crate never reads but every child must lose: a variable of a vendor CLI or of the
/// coding agent that would override the account the host selected (R27, R46, #344).
const SCRUB_ONLY_NAMES: [&str; 3] = [
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CODEX_API_KEY",
];

/// Names read from `.env` only, never from the process environment: the GitHub token names
/// (R33: the product uses its own GitHub connection, not a token from the terminal).
pub fn is_dotenv_only_name(name: &str) -> bool {
    GITHUB_TOKEN_NAMES.contains(&name)
}

/// The names a provider's key may come from on a host with no OS credential store, in the order
/// they are tried. The first is `<PROVIDER>_API_KEY` with `-` turned into `_`, the name
/// the host's `provider set-key` command writes to `.env`; the others are the names the peers use.
pub fn provider_env_names(provider_id: &str) -> &'static [&'static str] {
    match provider_id {
        "openai" => &["OPENAI_API_KEY"],
        "commandcode" => &["COMMANDCODE_API_KEY", "COMMAND_CODE_API_KEY"],
        "opencode" => &["OPENCODE_API_KEY"],
        "opencode-go" => &["OPENCODE_GO_API_KEY", "OPENCODE_API_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        // The first name is what `set-key` writes; `GEMINI_API_KEY` is the name the Gemini CLI reads.
        "google" => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "antigravity" => &["ANTIGRAVITY_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "tinyfish" => &["TINYFISH_API_KEY"],
        _ => &[],
    }
}

fn provider_ids_with_names() -> [&'static str; 11] {
    [
        "google",
        "mistral",
        "antigravity",
        "anthropic",
        "openai",
        "commandcode",
        "opencode",
        "opencode-go",
        "minimax",
        "openrouter",
        "tinyfish",
    ]
}

/// Every credential name this module reads (providers, GitHub, web search), without the route
/// names a caller adds.
pub fn credential_env_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for id in provider_ids_with_names() {
        names.extend(provider_env_names(id).iter().map(|n| n.to_string()));
    }
    names.extend(GITHUB_TOKEN_NAMES.iter().map(|n| n.to_string()));
    names.insert(WEB_SEARCH_KEY_NAME.to_string());
    names
}

/// A route credential name: `RIGHTKIT_<NAME>_API_KEY` (for example
/// `RIGHTKIT_ANTHROPIC_API_KEY`). A `provider-routes.<id>.api-key-env` may point at such a name
/// and at nothing else, so a configuration file can never name an arbitrary variable of the
/// environment and have its value sent to a route's endpoint. The names this module reads for
/// providers, GitHub and web search are not route names.
pub fn is_route_key_name(name: &str) -> bool {
    const PREFIX: &str = "RIGHTKIT_";
    const SUFFIX: &str = "_API_KEY";
    name.len() > PREFIX.len() + SUFFIX.len()
        && name.starts_with(PREFIX)
        && name.ends_with(SUFFIX)
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && !credential_env_names().contains(name)
        && !rightkit_app_settings::agent_config::licence_env::is_licence_env_name(name)
}

/// Whether a variable must be removed from every child process's environment (R46): a credential
/// name this module reads, a route credential name, a name only the scrub knows, a daemon secret
/// or a licence locator. The one launcher calls this for each variable it would pass on.
pub fn is_credential_env_name(name: &str) -> bool {
    credential_env_names().contains(name)
        || is_route_key_name(name)
        || SCRUB_ONLY_NAMES.contains(&name)
        || DAEMON_SECRET_NAMES.contains(&name)
        || rightkit_app_settings::agent_config::licence_env::is_licence_env_name(name)
}

/// Whether the host has an OS credential store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStoreState {
    /// A store opened, even if it is locked, unreadable or empty.
    Present,
    /// No store backend exists here.
    None { reason: String },
}

impl CredentialStoreState {
    pub fn has_store(&self) -> bool {
        matches!(self, CredentialStoreState::Present)
    }

    /// The line logged once at start and shown by the host's doctor command (R4).
    pub fn log_line(&self) -> String {
        match self {
            CredentialStoreState::Present => "credential store: present".to_string(),
            CredentialStoreState::None { reason } => format!("credential store: none: {reason}"),
        }
    }
}

/// Account name the probe asks the store about. Presence only; nothing is ever written under it.
#[cfg(feature = "agent_providers-os-keyring")]
const PROBE_ACCOUNT: &str = "__rightkit_store_probe__";

/// Classify the answer of one presence read. Only the no-backend class (and a test or QA guard
/// that disables the store) means "none"; locked, denied, timed-out and every other error is a
/// present store whose error is reported where the key is used.
#[cfg(feature = "agent_providers-os-keyring")]
pub fn classify_probe(result: Result<bool, rightkit_credentials::Error>) -> CredentialStoreState {
    match result {
        Ok(_) => CredentialStoreState::Present,
        Err(rightkit_credentials::Error::Unavailable(reason)) => {
            CredentialStoreState::None { reason }
        }
        Err(rightkit_credentials::Error::Guarded { guard }) => CredentialStoreState::None {
            reason: format!("disabled by guard {guard}"),
        },
        Err(_) => CredentialStoreState::Present,
    }
}

/// Open the real store once and classify it.
#[cfg(feature = "agent_providers-os-keyring")]
pub fn probe_credential_store() -> CredentialStoreState {
    match crate::agent_providers::keystore::secret_store(
        crate::agent_providers::key_migration::SERVICE,
    ) {
        Ok(store) => classify_probe(store.exists(PROBE_ACCOUNT)),
        // An invalid namespace is a build defect, not a missing backend: never read the environment.
        Err(_) => CredentialStoreState::Present,
    }
}

/// Where a no-store key was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeySource {
    Environment,
    DotEnv,
}

/// One key found on a no-store host. The value is the secret; never log or serialise this type.
#[derive(Clone, PartialEq, Eq)]
pub struct HostKey {
    pub value: String,
    pub source: HostKeySource,
}

impl std::fmt::Debug for HostKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostKey")
            .field("source", &self.source)
            .field("value", &crate::agent_providers::keystore::REDACTED)
            .finish()
    }
}

/// The credentials read once at start. Holds secrets in memory only.
pub struct HostCredentials {
    state: CredentialStoreState,
    state_root: Option<PathBuf>,
    route_names: Vec<String>,
    env: BTreeMap<String, String>,
    dotenv: Mutex<BTreeMap<String, String>>,
    dotenv_error: Mutex<Option<String>>,
}

impl std::fmt::Debug for HostCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCredentials")
            .field("state", &self.state)
            .field("state_root", &self.state_root)
            .field("env_names", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn nonblank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

fn dotenv_remedy(path: &Path) -> String {
    if cfg!(windows) {
        format!(
            "{} can be read by another user; remove every access control entry except your own, then start the app again",
            path.display()
        )
    } else {
        format!(
            "{} can be read by other users; run `chmod 600 {}` and start the app again",
            path.display(),
            path.display()
        )
    }
}

/// Parse `NAME=value` lines, keeping only `allowed` names. Blank lines and `#` comments are
/// skipped; one pair of matching quotes around a value is removed. A value is never echoed.
fn parse_dotenv(text: &str, allowed: &dyn Fn(&str) -> bool) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !allowed(name) {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        if !value.trim().is_empty() {
            out.insert(name.to_string(), value.trim().to_string());
        }
    }
    out
}

/// Read `<state root>/.env`: a missing file is empty; a file another user can read is refused.
fn read_dotenv(
    path: &Path,
    allowed: &dyn Fn(&str) -> bool,
) -> Result<BTreeMap<String, String>, String> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    match rightkit_fs::private::is_owner_only(path) {
        Ok(true) => {}
        Ok(false) => return Err(dotenv_remedy(path)),
        Err(error) => {
            return Err(format!(
                "cannot check the permissions of {}: {error}",
                path.display()
            ))
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(parse_dotenv(&text, allowed))
}

impl HostCredentials {
    /// Build the host's credentials from explicit parts. `process_env` is a snapshot of the
    /// environment; nothing here touches the real environment, so a test passes fakes.
    pub fn load(
        state: CredentialStoreState,
        state_root: Option<&Path>,
        process_env: &[(String, String)],
    ) -> Self {
        let mut env = BTreeMap::new();
        let mut dotenv = BTreeMap::new();
        let mut dotenv_error = None;
        let mut route_names = BTreeSet::new();
        if !state.has_store() {
            let builtin = credential_env_names();
            for (name, value) in process_env {
                // The GitHub names are `.env` only (R33).
                if is_dotenv_only_name(name) {
                    continue;
                }
                if builtin.contains(name) || is_route_key_name(name) {
                    if let Some(value) = nonblank(Some(value)) {
                        if is_route_key_name(name) {
                            route_names.insert(name.clone());
                        }
                        env.insert(name.clone(), value);
                    }
                }
            }
            if let Some(root) = state_root {
                let allowed = |name: &str| builtin.contains(name) || is_route_key_name(name);
                match read_dotenv(&root.join(DOTENV_FILE), &allowed) {
                    Ok(map) => {
                        route_names.extend(map.keys().filter(|n| is_route_key_name(n)).cloned());
                        dotenv = map;
                    }
                    Err(error) => dotenv_error = Some(error),
                }
            }
        }
        Self {
            state,
            state_root: state_root.map(Path::to_path_buf),
            route_names: route_names.into_iter().collect(),
            env,
            dotenv: Mutex::new(dotenv),
            dotenv_error: Mutex::new(dotenv_error),
        }
    }

    pub fn state(&self) -> &CredentialStoreState {
        &self.state
    }

    pub fn has_store(&self) -> bool {
        self.state.has_store()
    }

    /// The route credential names found in the environment or `.env` at start.
    pub fn route_names(&self) -> &[String] {
        &self.route_names
    }

    /// Why `.env` was refused or unreadable, if it was. Names the remedy; never a value.
    pub fn dotenv_error(&self) -> Option<String> {
        self.dotenv_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// `<state root>/.env`, when a state root is known.
    pub fn dotenv_path(&self) -> Option<PathBuf> {
        self.state_root.as_ref().map(|r| r.join(DOTENV_FILE))
    }

    fn dotenv_value(&self, name: &str) -> Option<String> {
        self.dotenv
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    /// Look one name up: the environment snapshot, then `.env`. `Ok(None)` when absent. A refused
    /// `.env` is an error only when the environment did not answer. On a host with a store this is
    /// always `Ok(None)`.
    pub fn lookup(&self, name: &str) -> Result<Option<HostKey>, String> {
        if self.has_store() {
            return Ok(None);
        }
        if let Some(value) = self.env.get(name) {
            return Ok(Some(HostKey {
                value: value.clone(),
                source: HostKeySource::Environment,
            }));
        }
        if let Some(error) = self.dotenv_error() {
            return Err(error);
        }
        Ok(self.dotenv_value(name).map(|value| HostKey {
            value,
            source: HostKeySource::DotEnv,
        }))
    }

    /// A provider's key on a no-store host: each of the provider's names in order, environment
    /// before `.env` for each.
    pub fn provider_key(&self, provider_id: &str) -> Result<Option<HostKey>, String> {
        for name in provider_env_names(provider_id) {
            if let Some(found) = self.lookup(name)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// A configured route's key (`provider-routes.<id>.api-key-env`): only a route credential
    /// name ([`is_route_key_name`]) is ever looked up; any other name finds nothing.
    pub fn route_key(&self, env_name: &str) -> Result<Option<HostKey>, String> {
        if !is_route_key_name(env_name) {
            return Ok(None);
        }
        self.lookup(env_name)
    }

    /// The GitHub token the app itself holds on a no-store host: `.env` only (R33). The
    /// environment is never read for it.
    pub fn github_token(&self) -> Result<Option<String>, String> {
        if self.has_store() {
            return Ok(None);
        }
        if let Some(error) = self.dotenv_error() {
            return Err(error);
        }
        Ok(GITHUB_TOKEN_NAMES
            .iter()
            .find_map(|name| self.dotenv_value(name)))
    }

    /// The web-search key on a no-store host (environment, then `.env`).
    pub fn web_search_key(&self) -> Result<Option<String>, String> {
        Ok(self.lookup(WEB_SEARCH_KEY_NAME)?.map(|found| found.value))
    }

    /// The credential names and values read from this process's environment at start (not
    /// `.env`). For a child of this same process only (the `rpc` adapter re-runs
    /// the host's run command), which gets them on its launch spec, as R46 allows for a launch that
    /// needs a key; never for any other child.
    pub fn environment_keys(&self) -> Vec<(String, String)> {
        self.env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }

    /// Every secret value held, distinct and sorted, for `CrashConfig::redact_values`. These are
    /// the secrets themselves: hand them to a redactor, never to a log or a response.
    pub fn secret_values(&self) -> Vec<String> {
        let mut values: BTreeSet<String> = self.env.values().cloned().collect();
        values.extend(
            self.dotenv
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
                .cloned(),
        );
        values.into_iter().collect()
    }

    /// The same values as a `rightkit_logs::KnownSecrets` set for the log redactor.
    pub fn known_secrets(&self) -> rightkit_logs::KnownSecrets {
        rightkit_logs::KnownSecrets::from_values(self.secret_values())
    }

    /// Save a name's value in `.env` on a no-store host (R10): other lines of the file are kept,
    /// the file is replaced atomically and owner-only. Refused when the host has a store, when
    /// the file is readable by another user, or when the value holds a line break.
    pub fn save_dotenv_value(&self, name: &str, value: &str) -> Result<(), String> {
        if self.has_store() {
            return Err("this host has an OS credential store; keys are saved there".into());
        }
        let value = value.trim();
        if value.is_empty() {
            return Err("API key is empty".into());
        }
        if value.contains(['\r', '\n']) {
            return Err("API key must be a single line".into());
        }
        self.rewrite_dotenv(name, Some(value))
    }

    /// Remove a name's line from `.env`. A missing line or file is success.
    pub fn remove_dotenv_value(&self, name: &str) -> Result<(), String> {
        if self.has_store() {
            return Err("this host has an OS credential store; keys are removed there".into());
        }
        self.rewrite_dotenv(name, None)
    }

    fn rewrite_dotenv(&self, name: &str, value: Option<&str>) -> Result<(), String> {
        let path = self
            .dotenv_path()
            .ok_or_else(|| "no state root is available to hold .env".to_string())?;
        if !credential_env_names().contains(name) && !is_route_key_name(name) {
            return Err(format!(
                "{name} is not a credential name the host keeps in .env"
            ));
        }
        let existing = if path.exists() {
            if !matches!(rightkit_fs::private::is_owner_only(&path), Ok(true)) {
                return Err(dotenv_remedy(&path));
            }
            std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?
        } else {
            String::new()
        };
        let mut lines: Vec<String> = Vec::new();
        for line in existing.lines() {
            let bare = line.trim().strip_prefix("export ").unwrap_or(line.trim());
            let is_target = bare
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == name);
            if !is_target {
                lines.push(line.to_string());
            }
        }
        if let Some(value) = value {
            lines.push(format!("{name}={value}"));
        }
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        rightkit_fs::atomic_write_private(&path, text.as_bytes())
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        let mut map = self
            .dotenv
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match value {
            Some(value) => {
                map.insert(name.to_string(), value.to_string());
            }
            None => {
                map.remove(name);
            }
        }
        *self
            .dotenv_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(())
    }

    /// Save a provider's key where this host keeps keys: the first of the provider's names in
    /// `.env` on a no-store host (the store is the caller's, through `write_key`).
    pub fn save_provider_key(&self, provider_id: &str, value: &str) -> Result<(), String> {
        let name = provider_env_names(provider_id)
            .first()
            .ok_or_else(|| format!("{provider_id} does not use an API key"))?;
        self.save_dotenv_value(name, value)
    }

    /// Remove a provider's keys from `.env` (every name it may use).
    pub fn remove_provider_key(&self, provider_id: &str) -> Result<(), String> {
        for name in provider_env_names(provider_id) {
            self.remove_dotenv_value(name)?;
        }
        Ok(())
    }

    /// Remove the credential names from this process's own environment, so no later child can
    /// inherit them (R4, R46). Runs on a host with a store too: a stale key variable never
    /// reaches a child.
    #[cfg(feature = "agent_providers-os-keyring")]
    fn scrub_process_environment(&self) {
        let mut names = credential_env_names();
        names.extend(
            std::env::vars_os()
                .filter_map(|(name, _)| name.into_string().ok())
                .filter(|name| is_route_key_name(name)),
        );
        for name in names {
            std::env::remove_var(name);
        }
    }
}

/// The `.env` variable that holds a key-store entry on a no-store host. A provider id maps to the
/// provider's first name (`openai` -> `OPENAI_API_KEY`); an API-key account entry
/// `account_key.<provider>.<slug>` maps to `RIGHTKIT_ACCOUNT_<PROVIDER>_<SLUG>_API_KEY`
/// (non-alphanumerics become `_`, so two slugs that differ only in punctuation share a name).
pub fn dotenv_name_for_entry(key: &str) -> Option<String> {
    if let Some(name) = provider_env_names(key).first() {
        return Some((*name).to_string());
    }
    let rest = key.strip_prefix("account_key.")?;
    let mapped: String = rest
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let name = format!("RIGHTKIT_ACCOUNT_{mapped}_API_KEY");
    is_route_key_name(&name).then_some(name)
}

/// The key store of this host as a [`crate::agent_providers::keystore::KeyStoreBackend`]: the OS credential store
/// when the probe found one; on a no-store host the environment and `<state root>/.env` (R4,
/// R10). Provider keys, the Settings API and API-key accounts (R27) all go through it, so a
/// server can save and read the same keys a desktop keeps in its store.
#[cfg(feature = "agent_providers-os-keyring")]
#[derive(Clone)]
pub struct HostKeyStore {
    host: Arc<HostCredentials>,
}

#[cfg(feature = "agent_providers-os-keyring")]
impl HostKeyStore {
    pub fn new(host: Arc<HostCredentials>) -> Self {
        Self { host }
    }

    /// The store of the initialised host ([`host`]).
    pub fn current() -> Self {
        Self::new(host())
    }

    fn name(key: &str) -> Result<String, crate::agent_providers::keystore::KeyStoreError> {
        dotenv_name_for_entry(key).ok_or_else(|| {
            crate::agent_providers::keystore::KeyStoreError::Keyring(format!(
                "{key} has no variable name for the key file"
            ))
        })
    }
}

#[cfg(feature = "agent_providers-os-keyring")]
impl crate::agent_providers::keystore::KeyStoreBackend for HostKeyStore {
    fn get(
        &self,
        service: &str,
        key: &str,
    ) -> Result<String, crate::agent_providers::keystore::KeyStoreError> {
        use crate::agent_providers::keystore::{KeyStore, KeyStoreError};
        if self.host.has_store() {
            return KeyStore::get(service, key);
        }
        let name = Self::name(key)?;
        match self.host.lookup(&name) {
            Ok(Some(found)) => Ok(found.value),
            Ok(None) => Err(KeyStoreError::NotFound {
                service: service.to_string(),
                key: key.to_string(),
            }),
            Err(reason) => Err(KeyStoreError::Keyring(reason)),
        }
    }

    fn set(
        &self,
        service: &str,
        key: &str,
        value: &str,
    ) -> Result<(), crate::agent_providers::keystore::KeyStoreError> {
        use crate::agent_providers::keystore::{KeyStore, KeyStoreError};
        if self.host.has_store() {
            return KeyStore::set(service, key, value);
        }
        let name = Self::name(key)?;
        self.host
            .save_dotenv_value(&name, value)
            .map_err(KeyStoreError::Keyring)
    }

    fn delete(
        &self,
        service: &str,
        key: &str,
    ) -> Result<(), crate::agent_providers::keystore::KeyStoreError> {
        use crate::agent_providers::keystore::{KeyStore, KeyStoreError};
        if self.host.has_store() {
            return KeyStore::delete(service, key);
        }
        let name = Self::name(key)?;
        // Every spelling of a provider's name goes, so a stale alias cannot keep a removed key.
        if provider_env_names(key).is_empty() {
            self.host
                .remove_dotenv_value(&name)
                .map_err(KeyStoreError::Keyring)
        } else {
            self.host
                .remove_provider_key(key)
                .map_err(KeyStoreError::Keyring)
        }
    }

    fn found_in(
        &self,
        _service: &str,
        key: &str,
    ) -> crate::agent_providers::key_migration::KeySource {
        use crate::agent_providers::key_migration::KeySource;
        if self.host.has_store() {
            return KeySource::Keychain;
        }
        let found = provider_env_names(key)
            .iter()
            .map(|n| n.to_string())
            .chain(dotenv_name_for_entry(key))
            .find_map(|name| self.host.lookup(&name).ok().flatten());
        match found.map(|f| f.source) {
            Some(HostKeySource::Environment) => KeySource::Environment,
            Some(HostKeySource::DotEnv) => KeySource::DotEnv,
            None => KeySource::Missing,
        }
    }
}

#[cfg(feature = "agent_providers-os-keyring")]
static HOST: OnceLock<Arc<HostCredentials>> = OnceLock::new();

/// Probe the store, read the environment and `.env` once, remove the names from this process's
/// environment, and keep the result for the life of the process. Call it at daemon or CLI start,
/// before any thread that spawns children. Later calls return the first result. Log
/// `state().log_line()` once and show it in the host's doctor command.
#[cfg(feature = "agent_providers-os-keyring")]
pub fn initialize(state_root: Option<&Path>) -> Arc<HostCredentials> {
    HOST.get_or_init(|| {
        let state = probe_credential_store();
        let snapshot: Vec<(String, String)> = std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        let host = HostCredentials::load(state, state_root, &snapshot);
        host.scrub_process_environment();
        Arc::new(host)
    })
    .clone()
}

/// The initialised host, starting it from the state root when nothing called [`initialize`] yet.
#[cfg(feature = "agent_providers-os-keyring")]
pub fn host() -> Arc<HostCredentials> {
    if let Some(host) = HOST.get() {
        return host.clone();
    }
    let root = rightkit_app_settings::agent_config::state_root();
    initialize(root.as_deref())
}

/// Save a provider key where this host keeps it: the OS credential store when one exists
/// (a write the store cannot verify stores nothing, R10), `<state root>/.env` otherwise.
#[cfg(feature = "agent_providers-os-keyring")]
pub fn store_provider_key(provider_id: &str, value: &str) -> Result<(), String> {
    let host = host();
    if host.has_store() {
        crate::agent_providers::key_migration::write_key(
            &crate::agent_providers::keystore::OsKeyStore,
            provider_id,
            value.trim(),
        )
    } else {
        host.save_provider_key(provider_id, value)
    }
}

/// Remove a provider key from where this host keeps it.
#[cfg(feature = "agent_providers-os-keyring")]
pub fn remove_provider_key(provider_id: &str) -> Result<(), String> {
    let host = host();
    if host.has_store() {
        crate::agent_providers::key_migration::remove_key(
            &crate::agent_providers::keystore::OsKeyStore,
            provider_id,
        )
    } else {
        host.remove_provider_key(provider_id)
    }
}
