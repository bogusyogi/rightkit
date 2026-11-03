//! Cross-platform API key storage via the shared `rightkit-credentials` crate.
//!
//! [`KeyStore`] is the real OS-keychain-backed store (Windows Credential
//! Manager / macOS Keychain / Linux Secret Service via `rightkit-credentials`).
//! [`KeyStoreBackend`] is a trait seam so callers can inject an in-memory
//! fake in tests without touching a real OS credential store.

/// Errors from the key store.
#[derive(Debug, thiserror::Error)]
pub enum KeyStoreError {
    #[error("key not found: {service}/{key}")]
    NotFound { service: String, key: String },

    #[error("keyring error: {0}")]
    Keyring(String),
}

/// What [`KeyStoreBackend::delete_if_equals`] did. Only `Deleted` and `Absent` leave the store without the item;
/// `Changed` and `Unsupported` removed nothing, so the caller must treat the item as still there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDelete {
    /// The item held the expected value and was removed.
    Deleted,
    /// There was no item.
    Absent,
    /// The item exists with another value (a writer replaced it); nothing was removed.
    Changed,
    /// This backend cannot compare and delete in one step, so it removed nothing.
    Unsupported,
}

/// Backend seam for provider-key storage. Implemented by [`KeyStore`] (the
/// real OS keychain) and by test-only in-memory fakes so migration/fallback
/// logic can be unit-tested without a real secret service.
pub trait KeyStoreBackend {
    fn get(&self, service: &str, key: &str) -> Result<String, KeyStoreError>;
    fn set(&self, service: &str, key: &str, value: &str) -> Result<(), KeyStoreError>;
    fn delete(&self, service: &str, key: &str) -> Result<(), KeyStoreError>;

    /// Store `value` only when no credential exists yet; `Ok(true)` when this call created it,
    /// `Ok(false)` when one was already there (never replaced). A concurrent writer's value wins and
    /// the caller reads it back. The default is read-then-set; the real store and the in-memory fake
    /// override it with a single create-only operation.
    fn create(&self, service: &str, key: &str, value: &str) -> Result<bool, KeyStoreError> {
        match self.get(service, key) {
            Ok(_) => Ok(false),
            Err(KeyStoreError::NotFound { .. }) => self.set(service, key, value).map(|()| true),
            Err(error) => Err(error),
        }
    }

    /// Remove the item only if it still holds exactly `expected`, as one step a concurrent writer cannot
    /// interleave with. A backend that cannot do that must leave the default, which removes nothing and
    /// answers [`KeyDelete::Unsupported`]: an unconditional delete here could erase a key a user just saved.
    fn delete_if_equals(
        &self,
        _service: &str,
        _key: &str,
        _expected: &str,
    ) -> Result<KeyDelete, KeyStoreError> {
        Ok(KeyDelete::Unsupported)
    }
}

/// Zero-sized handle onto the real OS keychain, so it can be passed wherever
/// a [`KeyStoreBackend`] is expected.
#[derive(Clone, Copy, Default)]
#[cfg(feature = "agent_providers-os-keyring")]
pub struct OsKeyStore;

#[cfg(feature = "agent_providers-os-keyring")]
impl KeyStoreBackend for OsKeyStore {
    fn get(&self, service: &str, key: &str) -> Result<String, KeyStoreError> {
        KeyStore::get(service, key)
    }

    fn set(&self, service: &str, key: &str, value: &str) -> Result<(), KeyStoreError> {
        KeyStore::set(service, key, value)
    }

    fn delete(&self, service: &str, key: &str) -> Result<(), KeyStoreError> {
        KeyStore::delete(service, key)
    }

    fn create(&self, service: &str, key: &str, value: &str) -> Result<bool, KeyStoreError> {
        KeyStore::create(service, key, value)
    }

    fn delete_if_equals(
        &self,
        service: &str,
        key: &str,
        expected: &str,
    ) -> Result<KeyDelete, KeyStoreError> {
        KeyStore::delete_if_equals(service, key, expected)
    }
}

/// Cross-platform API key store backed by the system keyring.
///
/// Service name is always `"host app"`. Key names are provider slugs,
/// e.g. `"anthropic_api_key"`.
#[cfg(feature = "agent_providers-os-keyring")]
pub struct KeyStore;

/// Kill-switch for every real-keychain touch. Enforced in code: `cfg!(test)`
/// disables OS keychain access for every cargo test binary, and setting this
/// env var (e.g. `RIGHTKIT_DISABLE_OS_KEYCHAIN=1`) does the same for non-test
/// callers such as a locally run daemon. On macOS a cargo test binary has a
/// fresh hash every build, so each run would otherwise re-trigger the
/// "wants to use your confidential information" prompt and "Always Allow"
/// can never stick.
pub const DISABLE_ENV: &str = "RIGHTKIT_DISABLE_OS_KEYCHAIN";

/// Whether OS-keychain access is suppressed for this process.
///
/// Under `cargo test` this is unconditionally true. Every rebuilt test binary
/// has a fresh hash, so macOS treats it as a new application and re-prompts;
/// "Always Allow" can never stick, and a headless run hangs behind a dialog
/// nobody sees. That used to be handled by an `[env]` block in
/// `engine/.cargo/config.toml`, but a managed build broker legitimately
/// refuses to honour workspace cargo configs that inject environment
/// variables -- so the guarantee belongs in the code that needs it, not in a
/// build-tool config that may or may not be applied.
///
/// The env var lets a developer running the daemon locally opt out of keychain access.
#[cfg(feature = "agent_providers-os-keyring")]
fn keychain_disabled() -> bool {
    std::env::var_os(DISABLE_ENV).is_some_and(|v| v == "1")
}

/// Per-call cap for background keychain operations. macOS blocks a keychain
/// read inside `SecItemCopyMatching` while the authorization prompt is up, so
/// an unanswered prompt would hang daemon boot; `rightkit-credentials` runs each
/// operation on a throwaway thread and gives up after this window (fail
/// closed). Interactive operations use the crate's 90s window instead.
#[cfg(feature = "agent_providers-os-keyring")]
const OS_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(feature = "agent_providers-os-keyring")]
pub(crate) fn secret_store(service: &str) -> Result<rightkit_credentials::Store, KeyStoreError> {
    let namespace = rightkit_credentials::Namespace::new(service)
        .map_err(|e| KeyStoreError::Keyring(e.to_string()))?;
    let config = rightkit_credentials::Config::new(namespace)
        .guard_when(keychain_disabled(), rightkit_credentials::GuardMode::Deny)
        .timeout(OS_CALL_TIMEOUT);
    Ok(rightkit_credentials::Store::open(config))
}

#[cfg(feature = "agent_providers-os-keyring")]
fn secret_error(error: rightkit_credentials::Error) -> KeyStoreError {
    match error {
        rightkit_credentials::Error::Guarded { .. } => {
            KeyStoreError::Keyring(format!("os keychain disabled via {DISABLE_ENV}"))
        }
        other => KeyStoreError::Keyring(other.to_string()),
    }
}

#[cfg(feature = "agent_providers-os-keyring")]
impl KeyStore {
    const SERVICE: &'static str = "rightkit-agent";

    /// Retrieve a value from the key store.
    pub fn get(service: &str, key: &str) -> Result<String, KeyStoreError> {
        let secret = secret_store(service)?
            .get(key)
            .map_err(secret_error)?
            .ok_or_else(|| KeyStoreError::NotFound {
                service: service.to_string(),
                key: key.to_string(),
            })?;
        secret
            .expose_str()
            .map(str::to_owned)
            .map_err(|e| KeyStoreError::Keyring(e.to_string()))
    }

    /// Store a value in the key store, replacing any existing entry.
    ///
    /// Interactive on purpose: this is only ever reached from the user's own
    /// "Save key" click. Replacing an item whose ACL belongs to an older build
    /// REQUIRES macOS to ask them to authorize it; `rightkit-credentials` performs
    /// the delete-and-recreate repair for that case. Background reads keep
    /// using the silent, 5-second-capped path.
    pub fn set(service: &str, key: &str, value: &str) -> Result<(), KeyStoreError> {
        secret_store(service)?
            .interactive()
            .set_str(key, value)
            .map_err(secret_error)
    }

    /// Create-only store: `Ok(true)` when this call created the item, `Ok(false)` when one already
    /// existed (it is never replaced, so a concurrent writer's value is not clobbered).
    pub fn create(service: &str, key: &str, value: &str) -> Result<bool, KeyStoreError> {
        let secret = rightkit_credentials::Secret::from_bytes(value.as_bytes().to_vec());
        secret_store(service)?
            .put(key, &secret, rightkit_credentials::WriteIntent::CreateOnly)
            .map_err(secret_error)
    }

    /// Delete a value from the key store.
    pub fn delete(service: &str, key: &str) -> Result<(), KeyStoreError> {
        match secret_store(service)?.delete(key).map_err(secret_error)? {
            true => Ok(()),
            false => Err(KeyStoreError::NotFound {
                service: service.to_string(),
                key: key.to_string(),
            }),
        }
    }

    /// Delete the item only if it still holds exactly `expected` (the published store's compare-and-delete, one step
    /// under the same per-target lock as `set` and `create`). macOS has no such operation: it answers
    /// [`KeyDelete::Unsupported`] and removes nothing.
    pub fn delete_if_equals(
        service: &str,
        key: &str,
        expected: &str,
    ) -> Result<KeyDelete, KeyStoreError> {
        let expected = rightkit_credentials::Secret::from_bytes(expected.as_bytes().to_vec());
        let outcome = secret_store(service)?
            .delete_if_equals(key, &expected)
            .map_err(secret_error)?;
        Ok(match outcome {
            rightkit_credentials::ConditionalDelete::Deleted => KeyDelete::Deleted,
            rightkit_credentials::ConditionalDelete::Absent => KeyDelete::Absent,
            rightkit_credentials::ConditionalDelete::Changed => KeyDelete::Changed,
            rightkit_credentials::ConditionalDelete::Unsupported => KeyDelete::Unsupported,
            // A future outcome that is not known to have removed the item is treated as not removed.
            #[allow(unreachable_patterns)]
            _ => KeyDelete::Unsupported,
        })
    }

    /// Get the default service name used by the host app.
    pub fn default_service() -> &'static str {
        Self::SERVICE
    }

    /// Convenience: get a provider API key using the default service.
    pub fn get_api_key(provider_slug: &str) -> Result<String, KeyStoreError> {
        Self::get(Self::SERVICE, provider_slug)
    }

    /// Convenience: set a provider API key using the default service.
    pub fn set_api_key(provider_slug: &str, value: &str) -> Result<(), KeyStoreError> {
        Self::set(Self::SERVICE, provider_slug, value)
    }

    /// Convenience: delete a provider API key using the default service.
    pub fn delete_api_key(provider_slug: &str) -> Result<(), KeyStoreError> {
        Self::delete(Self::SERVICE, provider_slug)
    }
}

/// Canonical placeholder a secret is replaced with whenever credential
/// material must be kept out of rendered output (CLI JSON, config views,
/// probe details). One vocabulary across CLI and daemon so a redaction scan
/// can assert on it.
pub const REDACTED: &str = rightkit_logs::KNOWN_SECRET_MARKER;
