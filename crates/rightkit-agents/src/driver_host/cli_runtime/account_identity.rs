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
//! - `codex`: nothing. The only account metadata Codex writes is inside
//!   `auth.json`, which also holds tokens; reading it would breach the
//!   "never read subscription tokens" policy, so identity stays `None`
//!   until the CLI exposes a status command we can parse.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::driver_host::cli_runtime::ids::{CliAuthMode, CliProviderId};
use crate::driver_host::cli_runtime::profile::{list_profiles, load_profile, ProfileStoreError};

/// Upper bound on `.claude.json` we are willing to parse (it carries project
/// history and can grow large).
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;

const SUBSCRIPTION_PROVIDERS: &[&str] = &["claude", "codex"];

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

/// One subscription account as the account list reports it.
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
}

/// Every managed subscription profile on disk, claude then codex, sorted by
/// label within a provider. Identity is read live so a re-login under a
/// different account shows up without a stale cache.
pub fn list_subscription_accounts() -> Vec<SubscriptionAccount> {
    let mut out = Vec::new();
    for provider in SUBSCRIPTION_PROVIDERS {
        let provider_id = CliProviderId::new(*provider);
        for record in list_profiles(&provider_id) {
            let identity = if record.signed_in {
                read_identity(&provider_id, &record.managed_home)
            } else {
                None
            };
            let fingerprint = identity.as_ref().and_then(|i| i.fingerprint(provider));
            out.push(SubscriptionAccount {
                id: rightkit_app_settings::agent_config::accounts::subscription_account_id(
                    provider,
                    &record.label,
                ),
                provider: (*provider).to_string(),
                label: record.label,
                signed_in: record.signed_in,
                auth_mode: record.auth_mode,
                identity,
                fingerprint,
                managed_home: record.managed_home,
            });
        }
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

/// Typed identity for any managed subscription profile (`claude/work`, `codex/personal`):
/// provider, account id, label and kind always; email and organization only when the vendor CLI
/// exposes them (Codex never does, since its metadata sits beside tokens). `None` when the
/// profile is unknown or malformed.
pub fn identity_for_profile(profile_id: &str) -> Option<crate::driver_host::ClaudeAccountIdentity> {
    let (provider, label) = profile_id.split_once('/')?;
    let provider_id = CliProviderId::new(provider);
    let record = load_profile(&provider_id, label).ok()??;
    let identity = read_identity(&provider_id, &record.managed_home);
    Some(crate::driver_host::ClaudeAccountIdentity {
        provider_id: provider.to_string(),
        email: identity.as_ref().and_then(|i| i.email.clone()),
        organization: identity.and_then(|i| i.organization),
        account_id: Some(
            rightkit_app_settings::agent_config::accounts::subscription_account_id(provider, label),
        ),
        label: Some(label.to_string()),
        kind: Some("subscription".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::profile::{
        profile_for, save_profile, with_isolated_profile_home,
    };

    #[test]
    fn reads_only_named_non_secret_claude_fields() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":" a@b.co ","organizationName":"Acme","displayName":"A",
               "accessToken":concat!("sk", "-ant-oat-SECRET"),"refreshToken":"SECRET2"},"projects":{}}"#,
        )
        .unwrap();
        let id = read_identity(&CliProviderId::new("claude"), dir.path()).unwrap();
        assert_eq!(id.email.as_deref(), Some("a@b.co"));
        assert_eq!(id.organization.as_deref(), Some("Acme"));
        let json = serde_json::to_string(&id).unwrap();
        assert!(!json.contains("SECRET"));
    }

    #[test]
    fn two_orgs_under_one_email_have_different_fingerprints() {
        let write = |dir: &std::path::Path, org: &str| {
            std::fs::write(
                dir.join(".claude.json"),
                format!(
                    r#"{{"oauthAccount":{{"emailAddress":"a@b.co","organizationUuid":"{org}","organizationName":"Same"}}}}"#
                ),
            )
            .unwrap();
        };
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(one.path(), "11111111-aaaa");
        write(two.path(), "22222222-bbbb");
        let claude = CliProviderId::new("claude");
        let (fp1, fp2) = (
            fingerprint_for_home(&claude, one.path()).unwrap(),
            fingerprint_for_home(&claude, two.path()).unwrap(),
        );
        assert_ne!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
        // Same account, different spelling: same fingerprint.
        assert_eq!(
            account_fingerprint("claude", Some(" A@B.co "), Some("11111111-AAAA")),
            Some(fp1)
        );
    }

    #[test]
    fn fingerprint_includes_provider_and_needs_some_identity() {
        let claude = account_fingerprint("claude", Some("a@b.co"), None).unwrap();
        let codex = account_fingerprint("codex", Some("a@b.co"), None).unwrap();
        assert_ne!(claude, codex);
        assert_eq!(account_fingerprint("claude", None, None), None);
        assert_eq!(account_fingerprint("claude", Some("  "), Some("")), None);
        // Fields cannot be shifted into one another.
        assert_ne!(
            account_fingerprint("claude", Some("a"), Some("b")),
            account_fingerprint("claude", Some("ab"), None)
        );
    }

    #[test]
    fn missing_malformed_or_empty_metadata_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = CliProviderId::new("claude");
        assert!(read_identity(&p, dir.path()).is_none());
        std::fs::write(dir.path().join(".claude.json"), "{not json").unwrap();
        assert!(read_identity(&p, dir.path()).is_none());
        std::fs::write(dir.path().join(".claude.json"), r#"{"oauthAccount":{}}"#).unwrap();
        assert!(read_identity(&p, dir.path()).is_none());
    }

    #[test]
    fn codex_never_reads_auth_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("auth.json"),
            r#"{"tokens":{"id_token":"x"}}"#,
        )
        .unwrap();
        assert!(read_identity(&CliProviderId::new("codex"), dir.path()).is_none());
    }

    #[test]
    fn codex_profile_gets_typed_identity_without_email() {
        with_isolated_profile_home(|| {
            let codex = CliProviderId::new("codex");
            let mut p = profile_for(&codex, "personal").unwrap();
            std::fs::write(
                p.managed_home.join("auth.json"),
                r#"{"tokens":{"id_token":"SECRET"},"email":"leak@x.io"}"#,
            )
            .unwrap();
            p.signed_in = true;
            save_profile(&p).unwrap();
            let who = identity_for_profile("codex/personal").unwrap();
            assert_eq!(who.provider_id, "codex");
            assert_eq!(who.label.as_deref(), Some("personal"));
            assert_eq!(who.kind.as_deref(), Some("subscription"));
            assert_eq!(
                who.account_id.as_deref(),
                Some("subscription/codex/personal")
            );
            assert_eq!(who.email, None);
            assert!(!serde_json::to_string(&who).unwrap().contains("SECRET"));
            assert!(identity_for_profile("codex/ghost").is_none());
        });
    }

    #[test]
    fn lists_profiles_with_identity_only_when_signed_in() {
        with_isolated_profile_home(|| {
            let claude = CliProviderId::new("claude");
            let mut signed = profile_for(&claude, "work").unwrap();
            std::fs::write(
                signed.managed_home.join(".claude.json"),
                r#"{"oauthAccount":{"emailAddress":"w@x.io"}}"#,
            )
            .unwrap();
            signed.signed_in = true;
            save_profile(&signed).unwrap();
            profile_for(&claude, "spare").unwrap();

            let who = claude_identity_for_profile("claude/work").unwrap();
            assert_eq!(who.provider_id, "claude");
            assert_eq!(who.organization, None);
            assert_eq!(who.email.as_deref(), Some("w@x.io"));
            assert!(claude_identity_for_profile("claude/ghost").is_none());
            assert_eq!(who.label.as_deref(), Some("work"));
            assert_eq!(who.kind.as_deref(), Some("subscription"));
            assert_eq!(who.account_id.as_deref(), Some("subscription/claude/work"));

            let list = list_subscription_accounts();
            let work = list.iter().find(|a| a.label == "work").unwrap();
            assert_eq!(work.id, "subscription/claude/work");
            assert_eq!(
                work.identity.as_ref().unwrap().email.as_deref(),
                Some("w@x.io")
            );
            let spare = list.iter().find(|a| a.label == "spare").unwrap();
            assert!(!spare.signed_in && spare.identity.is_none());
        });
    }
}
