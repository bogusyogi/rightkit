//! Secret handling shared by the net crates: the redacted [`Secret`], the [`SecretStore`]
//! interface and [`MemorySecretStore`], an in-memory implementation for tests and ephemeral
//! sessions. This crate holds no production (Keychain / Credential Manager) implementation,
//! and `rightkit-credentials` does not implement [`SecretStore`] (it has its own `Store` and
//! `Secret` types and does not depend on this crate). An app that wants OS-backed storage
//! behind this trait implements it itself.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

/// A string that never prints itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }
    /// The only way to read the value. Call at the point of use (header write).
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl From<String> for Secret {
    fn from(v: String) -> Self {
        Secret(v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError(pub String);

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "secret store: {}", self.0)
    }
}
impl std::error::Error for SecretError {}

/// Service/account keyed secret storage. The only implementation in this crate is
/// [`MemorySecretStore`]; callers supply any production implementation.
pub trait SecretStore: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<Secret>, SecretError>;
    fn set(&self, service: &str, account: &str, value: &Secret) -> Result<(), SecretError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), SecretError>;
}

/// In-memory store for tests and ephemeral sessions.
#[derive(Default)]
pub struct MemorySecretStore {
    inner: Mutex<HashMap<(String, String), Secret>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(service.to_string(), account.to_string()))
            .cloned())
    }
    fn set(&self, service: &str, account: &str, value: &Secret) -> Result<(), SecretError> {
        self.inner
            .lock()
            .unwrap()
            .insert((service.to_string(), account.to_string()), value.clone());
        Ok(())
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), SecretError> {
        self.inner
            .lock()
            .unwrap()
            .remove(&(service.to_string(), account.to_string()));
        Ok(())
    }
}
