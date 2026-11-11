//! `ra-018` decomposition: API key credential resolution (keychain, environment fallback) —
//! moved out of `client.rs`.
//! See `.audit/20260901-daemon-deep-r3/architect/decomposition-plans.md`.

/// Load an API key: the OS keychain first, then an environment variable as a
/// last-resort fallback. Never logs the key value.
///
/// Order matters, and it used to be the other way round (Adrian, 2026-08-04).
/// With env first, a variable exported years ago in a shell profile silently
/// outranked the key the user had just typed into Settings: Settings would
/// report "Saved securely", Remove would appear to do nothing, and every
/// request kept going out under the stale env key. Keys entered in the app are
/// the source of truth; the env var stays only for headless/CI runs where there
/// is no keychain to write to.
pub fn load_api_key(provider_id: &str) -> Option<String> {
    // Without an OS keychain there is no store to consult, so the env var is
    // the only source. A host that supplies its key directly (the mobile
    // runtime passes it in `RuntimeConfig`) never reaches this at all.
    #[cfg(feature = "agent_providers-os-keyring")]
    if let Some(value) = load_api_key_from_store(provider_id) {
        return Some(value);
    }
    env_api_key(provider_id)
}

/// The environment fallback, used only when nothing is stored.
#[cfg(not(feature = "agent_providers-host-env"))]
pub fn env_api_key(_provider_id: &str) -> Option<String> {
    // The mobile runtime takes its key from the host app, never the environment.
    None
}

#[cfg(feature = "agent_providers-host-env")]
pub fn env_api_key(provider_id: &str) -> Option<String> {
    let env_vars: &[&str] = match provider_id {
        "openai" => &["OPENAI_API_KEY"],
        "commandcode" => &["COMMAND_CODE_API_KEY", "COMMANDCODE_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "opencode" | "opencode-go" => &["OPENCODE_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "tinyfish" => &["TINYFISH_API_KEY"],
        _ => return None,
    };
    env_vars.iter().find_map(|env_var| {
        let value = std::env::var(env_var).ok()?;
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

#[cfg(feature = "agent_providers-os-keyring")]
fn load_api_key_from_store(provider_id: &str) -> Option<String> {
    crate::agent_providers::key_migration::read_key(
        &crate::agent_providers::keystore::OsKeyStore,
        provider_id,
    )
}

// ---------------------------------------------------------------------------
// API-key accounts (multi-account model)
//
// An API-key account is a registry entry (label, provider; see
// `rightkit_agent_config::accounts`) plus one secret in the OS credential store. The
// secret is keyed by account id, never by provider alone, so several accounts
// per provider coexist. Nothing here reads the environment: an account either
// has its stored key or it has none (no fallback to another account or to an
// env var), so a session bound to an account can never silently bill another.
// ---------------------------------------------------------------------------

use crate::agent_providers::keystore::{KeyStoreBackend, KeyStoreError};

/// Credential-store service name for API-key accounts.
pub const ACCOUNT_KEY_SERVICE: &str = "rightkit-agent";

/// Credential-store entry name for an account id such as
/// `api_key/openai/work`. Returns `None` for any id that is not a well-formed
/// API-key account id.
pub fn account_key_name(account_id: &str) -> Option<String> {
    if rightkit_agent_config::accounts::account_kind_of(account_id)?
        != rightkit_agent_config::accounts::AccountKind::ApiKey
    {
        return None;
    }
    let mut parts = account_id.split('/').skip(1);
    Some(format!("account_key.{}.{}", parts.next()?, parts.next()?))
}

fn bad_account(account_id: &str) -> KeyStoreError {
    KeyStoreError::Keyring(format!("not an API-key account id: {account_id}"))
}

/// Store the secret for an API-key account through `backend`.
pub fn store_account_key_with(
    backend: &dyn KeyStoreBackend,
    account_id: &str,
    key: &str,
) -> Result<(), KeyStoreError> {
    let name = account_key_name(account_id).ok_or_else(|| bad_account(account_id))?;
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(KeyStoreError::Keyring("API key is empty".into()));
    }
    backend.set(ACCOUNT_KEY_SERVICE, &name, trimmed)
}

/// Load the secret for an API-key account. `Ok(None)` when none is stored.
pub fn load_account_key_with(
    backend: &dyn KeyStoreBackend,
    account_id: &str,
) -> Result<Option<String>, KeyStoreError> {
    let name = account_key_name(account_id).ok_or_else(|| bad_account(account_id))?;
    match backend.get(ACCOUNT_KEY_SERVICE, &name) {
        Ok(v) => Ok(Some(v)),
        Err(KeyStoreError::NotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether a secret is stored, without returning it.
pub fn account_key_present_with(
    backend: &dyn KeyStoreBackend,
    account_id: &str,
) -> Result<bool, KeyStoreError> {
    Ok(load_account_key_with(backend, account_id)?.is_some())
}

/// Delete the secret for an API-key account. A missing entry is success.
pub fn delete_account_key_with(
    backend: &dyn KeyStoreBackend,
    account_id: &str,
) -> Result<(), KeyStoreError> {
    let name = account_key_name(account_id).ok_or_else(|| bad_account(account_id))?;
    match backend.delete(ACCOUNT_KEY_SERVICE, &name) {
        Ok(()) | Err(KeyStoreError::NotFound { .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(feature = "agent_providers-os-keyring")]
pub fn store_account_key(account_id: &str, key: &str) -> Result<(), KeyStoreError> {
    store_account_key_with(
        &crate::agent_providers::keystore::OsKeyStore,
        account_id,
        key,
    )
}

#[cfg(feature = "agent_providers-os-keyring")]
pub fn load_account_key(account_id: &str) -> Result<Option<String>, KeyStoreError> {
    load_account_key_with(&crate::agent_providers::keystore::OsKeyStore, account_id)
}

#[cfg(feature = "agent_providers-os-keyring")]
pub fn account_key_present(account_id: &str) -> Result<bool, KeyStoreError> {
    account_key_present_with(&crate::agent_providers::keystore::OsKeyStore, account_id)
}

#[cfg(feature = "agent_providers-os-keyring")]
pub fn delete_account_key(account_id: &str) -> Result<(), KeyStoreError> {
    delete_account_key_with(&crate::agent_providers::keystore::OsKeyStore, account_id)
}
