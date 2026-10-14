use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use rightkit_http::{Secret, SecretStore};

use crate::error::LlmError;

/// Source of provider credentials, queried on every call so rotation takes effect
/// immediately. Production binding: [`SecretStoreCredentials`] over `rightkit-credentials`.
pub trait CredentialSource: Send + Sync {
    fn credential(&self, provider: &str) -> Result<Option<Secret>, LlmError>;
}

/// Credentials held in a [`SecretStore`] under `service`, one account per provider id.
pub struct SecretStoreCredentials {
    pub store: Arc<dyn SecretStore>,
    pub service: String,
}

impl SecretStoreCredentials {
    pub fn new(store: Arc<dyn SecretStore>, service: impl Into<String>) -> Self {
        Self {
            store,
            service: service.into(),
        }
    }
}

impl CredentialSource for SecretStoreCredentials {
    fn credential(&self, provider: &str) -> Result<Option<Secret>, LlmError> {
        self.store
            .get(&self.service, provider)
            .map_err(|e| LlmError::Malformed(e.to_string()))
    }
}

/// Fixed in-memory credentials (tests, one-shot CLI use). An app that already holds keys
/// in its own store implements [`CredentialSource`] itself, returning [`Secret`] values.
#[derive(Default, Clone)]
pub struct StaticCredentials(pub HashMap<String, Secret>);

impl fmt::Debug for StaticCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Provider ids only: the values are secrets.
        let mut ids: Vec<&String> = self.0.keys().collect();
        ids.sort();
        f.debug_struct("StaticCredentials")
            .field("providers", &ids)
            .finish()
    }
}

impl StaticCredentials {
    pub fn with(mut self, provider: &str, value: &str) -> Self {
        self.0.insert(provider.to_string(), Secret::new(value));
        self
    }
}

impl CredentialSource for StaticCredentials {
    fn credential(&self, provider: &str) -> Result<Option<Secret>, LlmError> {
        Ok(self.0.get(provider).cloned())
    }
}
