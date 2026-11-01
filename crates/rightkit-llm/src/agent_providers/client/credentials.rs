//! `ra-018` decomposition: API key credential resolution (credential store, or environment and `.env` on a no-store host) —
//! moved out of `client.rs`.
//! See `.audit/20260901-daemon-deep-r3/architect/decomposition-plans.md`.

/// Load a provider API key where this host keeps it (providers R4). With an OS credential store
/// the key is the stored one and the environment is not read. On a host with no store the key is
/// the process environment's, then `<state root>/.env`, both read into memory once at start
/// ([`crate::agent_providers::host_credentials`]). Without an OS credential store feature (the
/// mobile runtime passes its key in `RuntimeConfig`) there is nothing to read. Never logs the key
/// value.
pub fn load_api_key(provider_id: &str) -> Option<String> {
    #[cfg(feature = "agent_providers-os-keyring")]
    {
        let host = crate::agent_providers::host_credentials::host();
        if host.has_store() {
            load_api_key_from_store(provider_id)
        } else {
            // A refused `.env` is reported by the broker and the host's doctor and set-key
            // commands; here it leaves the key absent.
            host.provider_key(provider_id)
                .ok()
                .flatten()
                .map(|found| found.value)
        }
    }
    #[cfg(not(feature = "agent_providers-os-keyring"))]
    {
        let _ = provider_id;
        None
    }
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
// `rightkit_app_settings::agent_config::accounts`) plus one secret in the OS credential store. The
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
    if rightkit_app_settings::agent_config::accounts::account_kind_of(account_id)?
        != rightkit_app_settings::agent_config::accounts::AccountKind::ApiKey
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
        &crate::agent_providers::host_credentials::HostKeyStore::current(),
        account_id,
        key,
    )
}

#[cfg(feature = "agent_providers-os-keyring")]
pub fn load_account_key(account_id: &str) -> Result<Option<String>, KeyStoreError> {
    load_account_key_with(
        &crate::agent_providers::host_credentials::HostKeyStore::current(),
        account_id,
    )
}

#[cfg(feature = "agent_providers-os-keyring")]
pub fn account_key_present(account_id: &str) -> Result<bool, KeyStoreError> {
    account_key_present_with(
        &crate::agent_providers::host_credentials::HostKeyStore::current(),
        account_id,
    )
}

#[cfg(feature = "agent_providers-os-keyring")]
pub fn delete_account_key(account_id: &str) -> Result<(), KeyStoreError> {
    delete_account_key_with(
        &crate::agent_providers::host_credentials::HostKeyStore::current(),
        account_id,
    )
}

#[cfg(test)]
mod account_key_tests {
    use super::*;
    use crate::agent_providers::keystore::InMemoryKeyStore;

    #[test]
    fn keys_are_per_account_not_per_provider() {
        let kv = InMemoryKeyStore::new();
        store_account_key_with(&kv, "api_key/openai/a", " sk-a ").unwrap();
        store_account_key_with(&kv, "api_key/openai/b", "sk-b").unwrap();
        assert_eq!(
            load_account_key_with(&kv, "api_key/openai/a")
                .unwrap()
                .as_deref(),
            Some("sk-a")
        );
        assert_eq!(
            load_account_key_with(&kv, "api_key/openai/b")
                .unwrap()
                .as_deref(),
            Some("sk-b")
        );
        assert_eq!(
            load_account_key_with(&kv, "api_key/openai/c").unwrap(),
            None
        );
    }

    #[test]
    fn delete_is_idempotent_and_presence_hides_value() {
        let kv = InMemoryKeyStore::new();
        store_account_key_with(&kv, "api_key/openai/a", "sk-a").unwrap();
        assert!(account_key_present_with(&kv, "api_key/openai/a").unwrap());
        delete_account_key_with(&kv, "api_key/openai/a").unwrap();
        delete_account_key_with(&kv, "api_key/openai/a").unwrap();
        assert!(!account_key_present_with(&kv, "api_key/openai/a").unwrap());
    }

    #[test]
    fn rejects_non_api_key_ids_and_empty_keys() {
        let kv = InMemoryKeyStore::new();
        assert!(store_account_key_with(&kv, "subscription/claude/x", "k").is_err());
        assert!(store_account_key_with(&kv, "garbage", "k").is_err());
        assert!(store_account_key_with(&kv, "api_key/openai/a", "  ").is_err());
        assert!(account_key_name("api_key/openai/a/../b").is_none());
    }
}
