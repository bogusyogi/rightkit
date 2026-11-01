//! App-lock state types: no OS call and no UI-framework type, so a database gate
//! and application state compile without the shell. The OS verification lives in `verify`.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppLockState {
    Disabled,
    Locked,
    Unlocked,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppLockStatus {
    pub enabled: bool,
    pub state: AppLockState,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLockRuntime {
    enabled: bool,
    state: AppLockState,
    available: bool,
    reason: Option<String>,
}

impl AppLockRuntime {
    pub fn new(
        enabled: bool,
        state: AppLockState,
        available: bool,
        reason: Option<String>,
    ) -> Self {
        Self {
            enabled,
            state,
            available,
            reason,
        }
    }

    pub fn disabled() -> Self {
        Self::new(false, AppLockState::Disabled, true, None)
    }

    pub fn locked(reason: Option<String>) -> Self {
        Self::new(true, AppLockState::Locked, true, reason)
    }

    pub fn unlocked() -> Self {
        Self::new(true, AppLockState::Unlocked, true, None)
    }

    pub fn unavailable(enabled: bool, reason: String) -> Self {
        Self::new(enabled, AppLockState::Unavailable, false, Some(reason))
    }

    /// True when the lock forbids any DB access: lock enabled and not
    /// currently unlocked (Locked, or Unavailable because Hello is missing).
    pub fn blocks_db(&self) -> bool {
        self.enabled && self.state != AppLockState::Unlocked
    }

    pub fn status(&self) -> AppLockStatus {
        AppLockStatus {
            enabled: self.enabled,
            state: self.state,
            available: self.available,
            reason: self.reason.clone(),
        }
    }
}
