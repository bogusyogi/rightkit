//! Provider API key storage in the OS keychain (Windows Credential Manager / macOS Keychain /
//! Linux Secret Service) through [`crate::agent_providers::keystore::KeyStoreBackend`]. Keychain-only and
//! fail-closed: there is no plaintext file and no opt-in degraded mode.

use crate::agent_providers::keystore::{KeyStoreBackend, KeyStoreError};
use std::sync::atomic::{AtomicBool, Ordering};

/// The service name provider keys are stored under in the OS keychain.
pub const SERVICE: &str = "rightkit-agent";

static KEYCHAIN_FAILURE_OBSERVED: AtomicBool = AtomicBool::new(false);

/// Non-secret credential-storage status for `/health` and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderKeyStorageHealth {
    pub keychain_failure_observed: bool,
}

/// Current non-secret provider-key storage status.
pub fn provider_key_storage_health() -> ProviderKeyStorageHealth {
    ProviderKeyStorageHealth {
        keychain_failure_observed: KEYCHAIN_FAILURE_OBSERVED.load(Ordering::Relaxed),
    }
}

/// Which store actually served (or would serve) a key, for status/diagnostic
/// surfaces. Never carries the key value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// Value came from a process environment variable.
    Env,
    /// Value came from the OS keychain.
    Keychain,
    /// No key configured anywhere.
    Missing,
    /// A keychain item exists but this build cannot read it.
    ///
    /// Distinct from `Missing` because the remedy is the opposite: the user has
    /// not forgotten to add a key, they added one that the OS is now refusing
    /// us. This happened for real — the engine daemon shipped ad-hoc signed, so
    /// its designated requirement was a cdhash that changed on every build, and
    /// macOS stopped trusting it with items an earlier build had stored.
    /// Reporting that as "missing" sent two separate debugging sessions looking
    /// for a key that was sitting right there.
    Unreadable,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Env => "env",
            KeySource::Keychain => "keychain",
            KeySource::Missing => "missing",
            KeySource::Unreadable => "unreadable",
        }
    }

    /// Guidance for the user, or `None` when nothing is wrong.
    pub fn remedy(self) -> Option<&'static str> {
        match self {
            KeySource::Unreadable => Some(
                "A saved key exists but this build of Host cannot open it. \
                 Re-enter it in Settings > Providers to rebind it to this build.",
            ),
            _ => None,
        }
    }
}

/// Read a provider key from the keychain. `None` when absent, blank, or unreadable (a keychain
/// error is recorded in [`provider_key_storage_health`]). Never logs the key value.
pub fn read_key<B: KeyStoreBackend>(backend: &B, provider_id: &str) -> Option<String> {
    match backend.get(SERVICE, provider_id) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        Err(KeyStoreError::Keyring(_)) => {
            KEYCHAIN_FAILURE_OBSERVED.store(true, Ordering::Relaxed);
            None
        }
        _ => None,
    }
}

/// Write a provider key to the keychain and verify the round trip. Fails closed: when the
/// keychain cannot verify the write the key is not stored anywhere.
pub fn write_key<B: KeyStoreBackend>(
    backend: &B,
    provider_id: &str,
    value: &str,
) -> Result<(), String> {
    let set_result = backend.set(SERVICE, provider_id, value);
    let verified = set_result.is_ok()
        && backend
            .get(SERVICE, provider_id)
            .map(|got| got == value)
            .unwrap_or(false);
    if verified {
        return Ok(());
    }
    let reason = match set_result {
        Err(KeyStoreError::Keyring(reason)) => reason,
        Err(KeyStoreError::NotFound { .. }) => "keychain reported key not found".to_string(),
        Ok(()) => "keychain accepted the write but could not read it back".to_string(),
    };
    KEYCHAIN_FAILURE_OBSERVED.store(true, Ordering::Relaxed);
    Err(format!(
        "provider key was not saved because the OS keychain could not verify the write ({reason})"
    ))
}

/// Remove a provider key from the keychain. Reports success if the key is gone, whether or not
/// it was there.
pub fn remove_key<B: KeyStoreBackend>(backend: &B, provider_id: &str) -> Result<(), String> {
    match backend.delete(SERVICE, provider_id) {
        Ok(()) => Ok(()),
        // Nothing to remove is the desired end state, not a failure.
        Err(KeyStoreError::NotFound { .. }) => Ok(()),
        Err(other) => Err(other.to_string()),
    }
}

/// Status check only. Used by the settings "is a key configured" surface.
pub fn status_source<B: KeyStoreBackend>(backend: &B, provider_id: &str) -> KeySource {
    // Distinguish "no item" from "item exists but the OS refused us": collapsing both into
    // Missing reported a key the user had definitely saved as never entered.
    match backend.get(SERVICE, provider_id) {
        Ok(value) if !value.trim().is_empty() => KeySource::Keychain,
        // An empty stored value is indistinguishable from none.
        Ok(_) | Err(KeyStoreError::NotFound { .. }) => KeySource::Missing,
        // A real error from the keychain (typically errSecAuthFailed /
        // errSecInteractionNotAllowed when the ACL no longer trusts this
        // binary). The item is there; we just cannot open it.
        Err(_) => KeySource::Unreadable,
    }
}
