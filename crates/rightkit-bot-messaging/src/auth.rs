//! Telegram user authorization gate.

/// The result of an authorization check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthDecision {
    /// The user is on the allow-list (or the list is open) — proceed.
    Allowed,
    /// The user is not authorized — reject.
    Denied { reason: String },
}

impl AuthDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, AuthDecision::Allowed)
    }
}

/// Gates Telegram interactions by user id.
///
/// An empty allow-list means *closed* (deny everyone) — a remote bot with no configured
/// users is a misconfiguration, not an open door. Add user ids explicitly.
#[derive(Debug, Clone, Default)]
pub struct AuthGate {
    allowed_users: Vec<u64>,
}

impl AuthGate {
    pub fn new(allowed_users: Vec<u64>) -> Self {
        Self { allowed_users }
    }

    pub fn allow(mut self, user_id: u64) -> Self {
        if !self.allowed_users.contains(&user_id) {
            self.allowed_users.push(user_id);
        }
        self
    }

    pub fn check(&self, user_id: u64) -> AuthDecision {
        if self.allowed_users.is_empty() {
            return AuthDecision::Denied {
                reason: "no users are authorized for remote access (allow-list is empty)".into(),
            };
        }
        if self.allowed_users.contains(&user_id) {
            AuthDecision::Allowed
        } else {
            AuthDecision::Denied {
                reason: format!("user {user_id} is not on the allow-list"),
            }
        }
    }

    /// Return stable provenance for an authorized Telegram principal.
    ///
    /// Authorization and identity stay coupled: callers cannot manufacture
    /// a remote principal for a user outside this gate's allow-list.
    pub fn authenticated_principal(&self, user_id: u64) -> Result<String, AuthDecision> {
        match self.check(user_id) {
            AuthDecision::Allowed => Ok(format!("telegram:{user_id}")),
            denied => Err(denied),
        }
    }

    pub fn is_open(&self) -> bool {
        !self.allowed_users.is_empty()
    }
}
