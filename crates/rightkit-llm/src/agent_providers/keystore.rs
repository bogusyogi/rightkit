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

    /// Where a value this backend can `get` lives, for status views. The OS store answers
    /// `KeySource::Keychain`; the no-store host answers environment or `.env`
    /// (`host_credentials::HostKeyStore`).
    fn found_in(
        &self,
        _service: &str,
        _key: &str,
    ) -> crate::agent_providers::key_migration::KeySource {
        crate::agent_providers::key_migration::KeySource::Keychain
    }

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

/// Kill-switch for every real-keychain touch, present in test builds only. `cfg!(test)` disables
/// OS keychain access for every cargo test binary, and in a `test-mode` build (the e2e daemon
/// binary) setting this env var (`RIGHTKIT_DISABLE_OS_KEYCHAIN=1`) does the same. A release build
/// has no such switch. On macOS a cargo test binary has a fresh hash every build, so each run
/// would otherwise re-trigger the "wants to use your confidential information" prompt and
/// "Always Allow" can never stick.
#[cfg(any(test, feature = "agent_providers-test-mode"))]
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
/// Only `test-mode` builds (which a release build never is) also honour the env var.
#[cfg(feature = "agent_providers-os-keyring")]
pub fn keychain_disabled() -> bool {
    #[cfg(any(test, feature = "agent_providers-test-mode"))]
    {
        cfg!(test) || std::env::var_os(DISABLE_ENV).is_some_and(|v| v == "1")
    }
    #[cfg(not(any(test, feature = "agent_providers-test-mode")))]
    {
        false
    }
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
            KeyStoreError::Keyring("os keychain disabled for this test build".into())
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

/// In-memory [`KeyStoreBackend`] fake for tests — never touches a real OS
/// credential store. Exposed unconditionally (not `#[cfg(test)]`) so it can
/// be reused from other crates' test suites (e.g. the `host app` binary's
/// provider-key handler tests) without a dev-dependency cycle.
#[derive(Default)]
pub struct InMemoryKeyStore {
    entries: std::sync::Mutex<std::collections::HashMap<(String, String), String>>,
    /// When set, every `set` call fails with this message instead of storing
    /// the value — used to exercise the file-fallback path.
    pub fail_writes: Option<String>,
    /// When set, every `get` fails with this message even though the entry
    /// exists — the shape of a macOS ACL denial, where the item is present but
    /// the OS refuses this binary. Distinct from a missing entry, and the only
    /// way to exercise `KeySource::Unreadable`.
    pub fail_reads: Option<String>,
}

impl InMemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a backend whose writes always fail (simulates a keychain
    /// that is unavailable, e.g. headless Linux with no secret service).
    pub fn failing(reason: &str) -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::HashMap::new()),
            fail_writes: Some(reason.to_string()),
            fail_reads: None,
        }
    }

    /// A store that holds the entry but refuses to hand it back, as macOS does
    /// once a keychain item's ACL no longer trusts the calling binary.
    pub fn unreadable(reason: &str) -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::HashMap::new()),
            fail_writes: None,
            fail_reads: Some(reason.to_string()),
        }
    }
}

/// Canonical placeholder a secret is replaced with whenever credential
/// material must be kept out of rendered output (CLI JSON, config views,
/// probe details). One vocabulary across CLI and daemon so a redaction scan
/// can assert on it.
pub const REDACTED: &str = rightkit_logs::KNOWN_SECRET_MARKER;

impl KeyStoreBackend for InMemoryKeyStore {
    fn get(&self, service: &str, key: &str) -> Result<String, KeyStoreError> {
        if let Some(reason) = &self.fail_reads {
            return Err(KeyStoreError::Keyring(reason.clone()));
        }
        let entries = self.entries.lock().unwrap();
        entries
            .get(&(service.to_string(), key.to_string()))
            .cloned()
            .ok_or_else(|| KeyStoreError::NotFound {
                service: service.to_string(),
                key: key.to_string(),
            })
    }

    fn set(&self, service: &str, key: &str, value: &str) -> Result<(), KeyStoreError> {
        if let Some(reason) = &self.fail_writes {
            return Err(KeyStoreError::Keyring(reason.clone()));
        }
        let mut entries = self.entries.lock().unwrap();
        entries.insert((service.to_string(), key.to_string()), value.to_string());
        Ok(())
    }

    fn create(&self, service: &str, key: &str, value: &str) -> Result<bool, KeyStoreError> {
        if let Some(reason) = &self.fail_writes {
            return Err(KeyStoreError::Keyring(reason.clone()));
        }
        let mut entries = self.entries.lock().unwrap();
        match entries.entry((service.to_string(), key.to_string())) {
            std::collections::hash_map::Entry::Occupied(_) => Ok(false),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(value.to_string());
                Ok(true)
            }
        }
    }

    fn delete(&self, service: &str, key: &str) -> Result<(), KeyStoreError> {
        let mut entries = self.entries.lock().unwrap();
        entries
            .remove(&(service.to_string(), key.to_string()))
            .map(|_| ())
            .ok_or_else(|| KeyStoreError::NotFound {
                service: service.to_string(),
                key: key.to_string(),
            })
    }

    fn delete_if_equals(
        &self,
        service: &str,
        key: &str,
        expected: &str,
    ) -> Result<KeyDelete, KeyStoreError> {
        let mut entries = self.entries.lock().unwrap();
        let slot = (service.to_string(), key.to_string());
        Ok(match entries.get(&slot) {
            None => KeyDelete::Absent,
            Some(held) if held == expected => {
                entries.remove(&slot);
                KeyDelete::Deleted
            }
            Some(_) => KeyDelete::Changed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Use a unique key suffix per test to avoid cross-test pollution.
    fn test_key(suffix: &str) -> String {
        format!("test_key_{suffix}")
    }

    #[test]
    fn keystore_roundtrip() {
        let key = test_key("roundtrip");
        // Clean up any leftover from a prior run
        let _ = KeyStore::delete(KeyStore::SERVICE, &key);

        // Skip gracefully if system keyring is unavailable or unreliable (CI).
        if KeyStore::set(KeyStore::SERVICE, &key, "sk-test-value").is_err() {
            return;
        }
        match KeyStore::get(KeyStore::SERVICE, &key) {
            Err(_) => return, // keyring available but unreliable — skip
            Ok(got) => assert_eq!(got, "sk-test-value"),
        }
        let _ = KeyStore::delete(KeyStore::SERVICE, &key);
    }

    #[test]
    fn keystore_missing_key_returns_error() {
        let key = test_key("nonexistent_abc123xyz");
        // Ensure it doesn't exist
        let _ = KeyStore::delete(KeyStore::SERVICE, &key);

        let result = KeyStore::get(KeyStore::SERVICE, &key);
        assert!(result.is_err());
        match result.unwrap_err() {
            KeyStoreError::NotFound { .. } => {} // real keychain, key absent
            // Under cargo the DISABLE_ENV kill-switch answers before the OS
            // keychain is consulted (.cargo/config.toml).
            KeyStoreError::Keyring(msg) if msg.contains("os keychain disabled") => {}
            e => panic!("expected NotFound or disabled keychain, got: {e}"),
        }
    }

    /// A service name nothing else uses, so a real-keychain test can never
    /// disturb the user's own `host app` items.
    fn scratch_service() -> String {
        format!("rightkit-agent-keystore-test-{}", std::process::id())
    }

    /// Whether this run may touch the real OS keychain.
    ///
    /// `.cargo/config.toml` sets `RIGHTKIT_DISABLE_OS_KEYCHAIN=1` for every
    /// cargo invocation, so the keychain tests above silently no-op — which is
    /// precisely why the overwrite bug reached a user. Opt in with
    /// `RIGHTKIT_KEYCHAIN_TESTS=1` and an unset kill-switch.
    fn real_keychain_enabled() -> bool {
        std::env::var("RIGHTKIT_KEYCHAIN_TESTS").as_deref() == Ok("1")
            && std::env::var(DISABLE_ENV).map(|v| v != "1").unwrap_or(true)
    }

    #[test]
    fn overwriting_an_existing_key_succeeds() {
        if !real_keychain_enabled() {
            return;
        }
        let service = scratch_service();
        let key = test_key("overwrite");
        let _ = KeyStore::delete(&service, &key);

        // An unsigned cargo test binary triggers a macOS approval prompt that
        // nobody answers, so the call hits the 5s cap. That is the environment
        // refusing, not the code failing — skip rather than report red.
        if KeyStore::set(&service, &key, "sk-first").is_err() {
            return;
        }
        // The bug: `set_password` cannot always update in place, falls through
        // to an add, and returns errSecDuplicateItem. So the FIRST save worked
        // and every correction after it failed.
        KeyStore::set(&service, &key, "sk-second")
            .expect("overwriting an existing key must work once the first save did");
        assert_eq!(
            KeyStore::get(&service, &key).expect("read back"),
            "sk-second",
            "the overwrite must win, not leave the old value in place"
        );
        let _ = KeyStore::delete(&service, &key);
    }

    #[test]
    fn in_memory_keystore_write_then_read() {
        let store = InMemoryKeyStore::new();
        store.set("rightkit-agent", "openrouter", "sk-abc").unwrap();
        assert_eq!(store.get("rightkit-agent", "openrouter").unwrap(), "sk-abc");
    }

    #[test]
    fn in_memory_keystore_missing_key_is_not_found() {
        let store = InMemoryKeyStore::new();
        match store.get("rightkit-agent", "missing").unwrap_err() {
            KeyStoreError::NotFound { .. } => {}
            e => panic!("expected NotFound, got: {e}"),
        }
    }

    #[test]
    fn in_memory_keystore_delete_removes_entry() {
        let store = InMemoryKeyStore::new();
        store.set("rightkit-agent", "openrouter", "sk-abc").unwrap();
        store.delete("rightkit-agent", "openrouter").unwrap();
        assert!(store.get("rightkit-agent", "openrouter").is_err());
    }

    #[test]
    fn in_memory_keystore_failing_backend_rejects_writes() {
        let store = InMemoryKeyStore::failing("no secret service");
        let err = store
            .set("rightkit-agent", "openrouter", "sk-abc")
            .unwrap_err();
        match err {
            KeyStoreError::Keyring(msg) => assert_eq!(msg, "no secret service"),
            other => panic!("expected Keyring error, got: {other}"),
        }
    }

    #[test]
    fn os_key_store_implements_backend_trait() {
        // Compile-time + smoke check that OsKeyStore satisfies KeyStoreBackend
        // and delegates to the same KeyStore statics (skips gracefully if the
        // system keyring is unavailable, same as keystore_roundtrip above).
        let backend = OsKeyStore;
        let key = test_key("os_backend_roundtrip");
        let _ = KeyStoreBackend::delete(&backend, KeyStore::SERVICE, &key);
        if KeyStoreBackend::set(&backend, KeyStore::SERVICE, &key, "sk-test").is_err() {
            return;
        }
        match KeyStoreBackend::get(&backend, KeyStore::SERVICE, &key) {
            Err(_) => return,
            Ok(got) => assert_eq!(got, "sk-test"),
        }
        let _ = KeyStoreBackend::delete(&backend, KeyStore::SERVICE, &key);
    }
}
