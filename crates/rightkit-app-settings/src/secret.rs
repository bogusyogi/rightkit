use serde::{Deserialize, Serialize};

/// A pointer to a secret held by the platform credential store. Safe to keep
/// in settings: it carries no secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub service: String,
    pub account: String,
}

impl SecretRef {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    Absent,
    /// The credential store could not be queried (locked, unavailable).
    Unknown,
}

/// App-injected adapter over the existing credential approach (Keychain,
/// Credential Manager, sidecar). Reports presence only; never returns values.
pub trait SecretProbe {
    fn presence(&self, secret: &SecretRef) -> Presence;
}
