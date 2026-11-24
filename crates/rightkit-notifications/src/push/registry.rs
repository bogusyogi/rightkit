//! Device token registry, one token per paired device.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{DeviceToken, PushError};

/// Where the per-device APNs token lives. The host implements this over its own persistence (the
/// crate has none and takes no database dependency). Keys are the paired device's id. Errors must
/// not contain a token.
pub trait DeviceTokenStore: Send + Sync {
    fn get(&self, device_id: &str) -> Result<Option<DeviceToken>, PushError>;
    /// Register or replace the token for `device_id`.
    fn put(&self, device_id: &str, token: DeviceToken) -> Result<(), PushError>;
    /// Remove the entry only if it still holds `token`, so a device that re-registered a fresh
    /// token between a send and its 410 keeps the fresh one. True when something was removed.
    fn remove_if_current(&self, device_id: &str, token: &DeviceToken) -> Result<bool, PushError>;
    /// Remove the entry (the device was unpaired). True when something was removed.
    fn remove(&self, device_id: &str) -> Result<bool, PushError>;
}

/// An in-process store: tokens are lost on exit. For hosts that re-register on every launch (iOS
/// hands the app its token at each start) and for wiring before real persistence exists.
#[derive(Default)]
pub struct MemoryTokenStore {
    map: Mutex<HashMap<String, DeviceToken>>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, DeviceToken>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for MemoryTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MemoryTokenStore(<{} devices>)", self.lock().len())
    }
}

fn check_id(device_id: &str) -> Result<(), PushError> {
    if device_id.is_empty() || device_id.len() > 128 {
        return Err(PushError::Invalid("device id"));
    }
    Ok(())
}

impl DeviceTokenStore for MemoryTokenStore {
    fn get(&self, device_id: &str) -> Result<Option<DeviceToken>, PushError> {
        check_id(device_id)?;
        Ok(self.lock().get(device_id).cloned())
    }

    fn put(&self, device_id: &str, token: DeviceToken) -> Result<(), PushError> {
        check_id(device_id)?;
        self.lock().insert(device_id.to_owned(), token);
        Ok(())
    }

    fn remove_if_current(&self, device_id: &str, token: &DeviceToken) -> Result<bool, PushError> {
        check_id(device_id)?;
        let mut map = self.lock();
        if map.get(device_id) == Some(token) {
            map.remove(device_id);
            return Ok(true);
        }
        Ok(false)
    }

    fn remove(&self, device_id: &str) -> Result<bool, PushError> {
        check_id(device_id)?;
        Ok(self.lock().remove(device_id).is_some())
    }
}
