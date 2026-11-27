//! Subscription-account identity for the account list.
//!
//! A subscription account is a managed CLI profile (see [`super::profile`]).
//! The vendor CLI owns the login; the host app reads only the non-secret
//! account metadata the CLI writes next to it, and never a token field.
//!
//! What is read, per provider:
//!
//! - `claude`: `<managed_home>/.claude.json`, key `oauthAccount`, fields
//!   `emailAddress`, `organizationName`, `organizationUuid`, `displayName` only. Claude Code is
//!   launched with `CLAUDE_CONFIG_DIR` and `HOME` both set to the managed
//!   home (see `providers/claude.rs`), so that is where it writes this file.
//!   The deserializer names only those three fields, so no other content of
//!   the file is materialized, and Claude Code keeps credentials in
//!   `.credentials.json` or the OS keychain, never in this file.
//! - linked `claude` profiles (a folder the user already had, see
//!   `rightkit_agent_config::accounts::LinkedProfileStore`; only for a host that chose
//!   [`host_options::set_linked_profiles`]): the same single file, `.claude.json` in that
//!   folder. For the default folder `<home>/.claude` Claude Code keeps `.claude.json` beside the
//!   folder (`<home>/.claude.json`) when `CLAUDE_CONFIG_DIR` is unset, so that file is read as
//!   the fallback. No other file of a linked folder is opened.
//! - `codex`: nothing. The only account metadata Codex writes is inside
//!   `auth.json`, which also holds tokens; reading it would breach the
//!   "never read subscription tokens" policy, so identity stays `None`
//!   until the CLI exposes a status command we can parse.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::driver_host::cli_runtime::ids::{CliAuthMode, CliProviderId};
use crate::driver_host::cli_runtime::profile::{list_profiles, load_profile, ProfileStoreError};
use crate::driver_host::host_options;

/// Upper bound on `.claude.json` we are willing to parse (it carries project
/// history and can grow large).
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;

/// The providers whose profiles are subscription accounts, before the host's choice of built-in
/// providers ([`host_options::set_built_in_providers`]) is applied.
const SUBSCRIPTION_PROVIDERS: &[&str] = &["claude", "codex"];

/// The subscription providers this host offers: [`SUBSCRIPTION_PROVIDERS`] limited to the
/// built-in providers the host chose.
fn subscription_providers() -> impl Iterator<Item = &'static str> {
    SUBSCRIPTION_PROVIDERS
        .iter()
        .copied()
        .filter(|provider| host_options::built_in_provider_offered(provider))
}

/// Non-secret identity of a signed-in subscription account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountIdentity {
    pub email: Option<String>,
    pub organization: Option<String>,
    /// `oauthAccount.organizationUuid`: two organizations under one email are two accounts (#341).
    #[serde(default)]
    pub organization_uuid: Option<String>,
    pub display_name: Option<String>,
}

impl AccountIdentity {
    fn is_empty(&self) -> bool {
        self.email.is_none()
            && self.organization.is_none()
            && self.organization_uuid.is_none()
            && self.display_name.is_none()
    }

    /// Stable fingerprint of this account for `provider`; see [`account_fingerprint`].
    pub fn fingerprint(&self, provider: &str) -> Option<String> {
        account_fingerprint(
            provider,
            self.email.as_deref(),
            self.organization_uuid.as_deref(),
        )
    }
}

/// Account identity key (#341): provider + email + organization UUID, never the email alone.
/// Case and surrounding space in the email and UUID do not matter. `None` when neither an email
/// nor an organization UUID is known (nothing to key on). The inputs are non-secret account
/// metadata; the output is the first 16 hex characters of a SHA-256 over them.
pub fn account_fingerprint(
    provider: &str,
    email: Option<&str>,
    organization_uuid: Option<&str>,
) -> Option<String> {
    let norm = |value: Option<&str>| value.map(|v| v.trim().to_lowercase()).unwrap_or_default();
    let (email, org) = (norm(email), norm(organization_uuid));
    if email.is_empty() && org.is_empty() {
        return None;
    }
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for part in [provider.trim().to_lowercase(), email, org] {
        hasher.update(part.as_bytes());
        hasher.update(&[0u8]);
    }
    Some(hasher.finalize().chars().take(16).collect())
}

/// Fingerprint of whoever is signed in under `managed_home`, when the vendor CLI exposes an
/// identity (Claude only today). `None` means "keep whatever fingerprint is stored".
pub fn fingerprint_for_home(provider: &CliProviderId, managed_home: &Path) -> Option<String> {
    read_identity(provider, managed_home)?.fingerprint(provider.as_str())
}

#[derive(Deserialize)]
struct ClaudeJson {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<ClaudeOauthAccount>,
}

#[derive(Deserialize)]
struct ClaudeOauthAccount {
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
    #[serde(rename = "organizationName")]
    organization_name: Option<String>,
    #[serde(rename = "organizationUuid")]
    organization_uuid: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control))
}

/// Read the non-secret identity of the account signed in under
/// `managed_home`. `None` when not signed in, when the provider exposes no
/// non-secret metadata, or when the file is absent/unreadable/oversized.
pub fn read_identity(provider: &CliProviderId, managed_home: &Path) -> Option<AccountIdentity> {
    match provider.as_str() {
        "claude" => read_claude_identity(managed_home),
        _ => None,
    }
}

fn read_claude_identity(managed_home: &Path) -> Option<AccountIdentity> {
    let path = managed_home.join(".claude.json");
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_METADATA_BYTES {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: ClaudeJson = serde_json::from_str(&raw).ok()?;
    let account = parsed.oauth_account?;
    let identity = AccountIdentity {
        email: clean(account.email_address),
        organization: clean(account.organization_name),
        organization_uuid: clean(account.organization_uuid),
        display_name: clean(account.display_name),
    };
    (!identity.is_empty()).then_some(identity)
}

/// Where a subscription account's folder comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSource {
    /// A profile the host created under `<state_root>/cli-profiles/`.
    #[default]
    Managed,
    /// A folder the user already had, linked by path.
    Linked,
}

/// Serde guards of the fields that exist only for a host that chose
/// [`host_options::set_linked_profiles`]: without that option (and with the value every account
/// then has) the account list serialises exactly the keys it always had.
fn skip_default_source(source: &AccountSource) -> bool {
    !host_options::linked_profiles() && *source == AccountSource::Managed
}

fn skip_unset_field(field: &Option<String>) -> bool {
    !host_options::linked_profiles() && field.is_none()
}

/// One subscription account as the account list reports it.
///
/// `source`, `folder` and `platform` are serialised only for a host that chose
/// [`host_options::set_linked_profiles`] (and then always, `null` included); without it they are
/// left out and every account is `Managed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionAccount {
    /// `subscription/<provider>/<label>`.
    pub id: String,
    pub provider: String,
    pub label: String,
    pub signed_in: bool,
    pub auth_mode: CliAuthMode,
    pub identity: Option<AccountIdentity>,
    /// provider + email + org UUID fingerprint; `None` when no identity is readable (#341).
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// The profile's config home (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`). Used by the same-home
    /// rule (#343); never serialized.
    #[serde(skip)]
    pub managed_home: std::path::PathBuf,
    #[serde(default, skip_serializing_if = "skip_default_source")]
    pub source: AccountSource,
    /// The user's own folder, for a linked profile only.
    #[serde(default, skip_serializing_if = "skip_unset_field")]
    pub folder: Option<String>,
    /// `bedrock` or `vertex` when the folder was set up with Claude Code's own wizard for that
    /// platform; `None` for a Claude account login.
    #[serde(default, skip_serializing_if = "skip_unset_field")]
    pub platform: Option<String>,
}

/// Which cloud platform a Claude config folder is set up for, if any. Claude Code's own
/// Bedrock and Vertex set-up saves its choice in the `env` block of `<folder>/settings.json`
/// (code.claude.com/docs/en/amazon-bedrock, google-vertex-ai). Only the two switches are read;
/// nothing else in the file is kept, logged or returned.
pub fn claude_platform_of(folder: &Path) -> Option<&'static str> {
    let path = folder.join("settings.json");
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_METADATA_BYTES {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let env = parsed.get("env")?.as_object()?;
    let on = |name: &str| {
        env.get(name).is_some_and(|value| match value {
            serde_json::Value::String(text) => matches!(text.trim(), "1" | "true"),
            serde_json::Value::Bool(flag) => *flag,
            serde_json::Value::Number(number) => number.as_i64() == Some(1),
            _ => false,
        })
    };
    if on("CLAUDE_CODE_USE_BEDROCK") {
        Some("bedrock")
    } else if on("CLAUDE_CODE_USE_VERTEX") {
        Some("vertex")
    } else {
        None
    }
}

/// Home folder of the current user (`HOME`, else `USERPROFILE`).
pub fn user_home() -> Option<std::path::PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .map(std::path::PathBuf::from)
        .find(|path| path.is_absolute())
}

/// Whether `folder` is the default Claude folder (`<home>/.claude`), the one Claude Code uses when
/// `CLAUDE_CONFIG_DIR` is unset.
pub fn is_default_claude_home(folder: &Path) -> bool {
    user_home().is_some_and(|home| folder == home.join(".claude"))
}

/// Identity of a linked Claude folder. Reads `<folder>/.claude.json` only; for the default folder
/// `<home>/.claude` it falls back to `<home>/.claude.json`, where Claude Code keeps it when
/// `CLAUDE_CONFIG_DIR` is unset. Never opens any other file (no credential file).
pub fn read_linked_claude_identity(folder: &Path) -> Option<AccountIdentity> {
    read_claude_identity(folder).or_else(|| {
        if is_default_claude_home(folder) {
            read_claude_identity(folder.parent()?)
        } else {
            None
        }
    })
}

/// A folder that may hold a Claude login, found without opening anything but its `.claude.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredFolder {
    pub path: String,
    pub identity: Option<AccountIdentity>,
}

/// Likely existing Claude folders: `$CLAUDE_CONFIG_DIR`, `<home>/.claude`, and every
/// `<home>/.claude-*` or `<home>/.claude_*` folder that contains a `.claude.json`. Sorted by path,
/// no duplicates. Nothing is linked, created or modified.
pub fn discover_claude_folders() -> Vec<DiscoveredFolder> {
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from);
    discover_claude_folders_in(user_home().as_deref(), config_dir.as_deref())
}

fn discover_claude_folders_in(
    home: Option<&Path>,
    config_dir: Option<&Path>,
) -> Vec<DiscoveredFolder> {
    let mut found: std::collections::BTreeSet<std::path::PathBuf> = Default::default();
    if let Some(dir) = config_dir {
        if dir.is_absolute() && dir.is_dir() {
            found.insert(dir.to_path_buf());
        }
    }
    if let Some(home) = home {
        let default = home.join(".claude");
        if default.is_dir() {
            found.insert(default);
        }
        if let Ok(entries) = std::fs::read_dir(home) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !(name.starts_with(".claude-") || name.starts_with(".claude_")) {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() && path.join(".claude.json").is_file() {
                    found.insert(path);
                }
            }
        }
    }
    found
        .into_iter()
        .map(|path| DiscoveredFolder {
            identity: read_claude_identity(&path).or_else(|| {
                // The default folder keeps its `.claude.json` beside it, in the home folder.
                (home.is_some_and(|h| path == h.join(".claude")))
                    .then(|| path.parent().and_then(read_claude_identity))
                    .flatten()
            }),
            path: path.to_string_lossy().into_owned(),
        })
        .collect()
}

/// The folder of a linked profile `<provider>/<label>`, when linked. `None` for a host that did
/// not choose [`host_options::set_linked_profiles`].
pub fn linked_profile_home(provider: &str, label: &str) -> Option<std::path::PathBuf> {
    if !host_options::linked_profiles() {
        return None;
    }
    let store = rightkit_agent_config::accounts::LinkedProfileStore::default_location().ok()?;
    store
        .find(provider, label)
        .ok()
        .flatten()
        .map(|link| std::path::PathBuf::from(link.path))
}

/// Every subscription profile, claude then codex (the providers the host offers): the managed
/// ones on disk (sorted by label within a provider), then, for a host that chose
/// [`host_options::set_linked_profiles`], the linked ones. Identity is read live so a re-login
/// under a different account shows up without a stale cache.
pub fn list_subscription_accounts() -> Vec<SubscriptionAccount> {
    let mut out = Vec::new();
    for provider in subscription_providers() {
        let provider_id = CliProviderId::new(provider);
        for record in list_profiles(&provider_id) {
            let identity = if record.signed_in {
                read_identity(&provider_id, &record.managed_home)
            } else {
                None
            };
            let fingerprint = identity.as_ref().and_then(|i| i.fingerprint(provider));
            out.push(SubscriptionAccount {
                id: rightkit_agent_config::accounts::subscription_account_id(
                    provider,
                    &record.label,
                ),
                provider: provider.to_string(),
                label: record.label,
                signed_in: record.signed_in,
                auth_mode: record.auth_mode,
                identity,
                fingerprint,
                managed_home: record.managed_home,
                source: AccountSource::Managed,
                folder: None,
                platform: None,
            });
        }
    }
    if host_options::linked_profiles() {
        if let Ok(store) = rightkit_agent_config::accounts::LinkedProfileStore::default_location() {
            let links = store.load().map(|file| file.links).unwrap_or_default();
            let linked = linked_accounts(links, &out);
            out.extend(linked);
        }
    }
    out
}

/// Linked profiles as accounts. A link whose label a managed profile already owns is skipped (the
/// managed one wins; linking refuses such labels, so this only guards against a later clash). A
/// linked Claude account counts as signed in when its `.claude.json` holds an account; whether the
/// login itself is still valid is for the vendor CLI to say, never read here.
fn linked_accounts(
    links: Vec<rightkit_agent_config::accounts::LinkedProfile>,
    existing: &[SubscriptionAccount],
) -> Vec<SubscriptionAccount> {
    let mut out = Vec::new();
    for link in links {
        let id =
            rightkit_agent_config::accounts::subscription_account_id(&link.provider, &link.label);
        if existing.iter().any(|a| a.id == id) {
            continue;
        }
        let folder = std::path::PathBuf::from(&link.path);
        let identity = if link.provider == "claude" {
            read_linked_claude_identity(&folder)
        } else {
            None
        };
        let fingerprint = identity
            .as_ref()
            .and_then(|i| i.fingerprint(&link.provider));
        // A folder set up for Bedrock or Vertex has no Claude account in it: its credentials are
        // the cloud's, which only the cloud can check. It counts as ready to use.
        let platform = (link.provider == "claude")
            .then(|| claude_platform_of(&folder))
            .flatten()
            .map(str::to_string);
        out.push(SubscriptionAccount {
            id,
            provider: link.provider,
            label: link.label,
            signed_in: identity.is_some() || platform.is_some(),
            auth_mode: if identity.is_some() {
                CliAuthMode::NativeCliAccount
            } else {
                CliAuthMode::Unknown
            },
            identity,
            fingerprint,
            managed_home: folder,
            source: AccountSource::Linked,
            folder: Some(link.path),
            platform,
        });
    }
    out
}

/// Whether a managed profile exists for `<provider>/<label>` (used to validate
/// a subscription selection).
pub fn subscription_profile_exists(provider: &str, label: &str) -> Result<bool, ProfileStoreError> {
    let provider_id = CliProviderId::new(provider);
    Ok(load_profile(&provider_id, label)?.is_some())
}

/// Identity for rate-limit stamping, looked up by managed profile id
/// (`<provider>/<label>`, e.g. `claude/work`). Pass the result to
/// `crate::driver_host::stamp_account` or `parse_claude_stream_events`. Non-secret
/// metadata only; `None` when the profile is unknown or malformed. When the
/// profile exists but no identity metadata is readable, the provider id is
/// still returned with `email`/`organization` unset.
pub fn claude_identity_for_profile(
    profile_id: &str,
) -> Option<crate::driver_host::ClaudeAccountIdentity> {
    identity_for_profile(profile_id)
}

/// Typed identity for any managed or linked subscription profile (`claude/work`, `codex/personal`):
/// provider, account id, label and kind always; email and organization only when the vendor CLI
/// exposes them (Codex never does, since its metadata sits beside tokens). `None` when the
/// profile is unknown or malformed.
pub fn identity_for_profile(profile_id: &str) -> Option<crate::driver_host::ClaudeAccountIdentity> {
    let (provider, label) = profile_id.split_once('/')?;
    let provider_id = CliProviderId::new(provider);
    let identity = match load_profile(&provider_id, label).ok()? {
        Some(record) => read_identity(&provider_id, &record.managed_home),
        None => {
            // A linked profile (only for a host that chose `set_linked_profiles`).
            let folder = linked_profile_home(provider, label)?;
            if provider == "claude" {
                read_linked_claude_identity(&folder)
            } else {
                None
            }
        }
    };
    Some(crate::driver_host::ClaudeAccountIdentity {
        provider_id: provider.to_string(),
        email: identity.as_ref().and_then(|i| i.email.clone()),
        organization: identity.and_then(|i| i.organization),
        account_id: Some(rightkit_agent_config::accounts::subscription_account_id(
            provider, label,
        )),
        label: Some(label.to_string()),
        kind: Some("subscription".to_string()),
    })
}
