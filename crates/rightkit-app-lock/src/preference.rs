//! The stored lock preference and its fail-closed reading.

/// What the stored app-lock preference says. A read that fails is `Unreadable`, never `Disabled`:
/// an absent item is the only "off".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockPreference {
    /// The lock is on.
    Enabled,
    /// The lock is off (the item is absent or holds anything but the on marker).
    Disabled,
    /// The item could not be read; the message is the read error.
    Unreadable(String),
}

/// True only for the explicit on marker `"1"`.
pub fn enabled_from_secret(value: Option<String>) -> bool {
    matches!(value.as_deref(), Some("1"))
}

/// Classify the raw read of the preference item.
pub fn lock_preference_from_read(read: Result<Option<String>, String>) -> LockPreference {
    match read {
        Ok(value) if enabled_from_secret(value.clone()) => LockPreference::Enabled,
        Ok(_) => LockPreference::Disabled,
        Err(error) => LockPreference::Unreadable(error),
    }
}

impl LockPreference {
    /// Fails closed: an unreadable preference counts as enabled, so nothing treats a denied or
    /// timed-out read of the lock item as "no lock".
    pub fn is_enabled(&self) -> bool {
        !matches!(self, LockPreference::Disabled)
    }
}
