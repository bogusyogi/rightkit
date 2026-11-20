//! Process-wide lazy device service for an app that builds its device store on first use.
//!
//! Always compiled (std only). The store itself (namespace, account, interaction and guard policy)
//! is the host's; this module owns the lazy cell (a failed build is never cached), the log-and-
//! return error reporting, and the unsupported-platform store. No device id is ever logged: only the
//! storage error text, which carries no value.

use std::sync::{Arc, Mutex};

use crate::{DeviceService, SecureStore, StorageOperation};

/// Event the build and read failures are logged under (the host's existing vocabulary).
pub const DEVICE_ID_ERROR_EVENT: &str = "secure_device_id_error";

pub type SharedDeviceService<S> = Arc<DeviceService<S>>;

/// A `static`-friendly lazy holder for the shared [`DeviceService`].
pub struct LazyDeviceService<S> {
    cell: Mutex<Option<SharedDeviceService<S>>>,
}

impl<S> LazyDeviceService<S> {
    pub const fn new() -> Self {
        Self {
            cell: Mutex::new(None),
        }
    }
}

impl<S> Default for LazyDeviceService<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: SecureStore<String>> LazyDeviceService<S> {
    /// Create the process-wide device service on first call from `build_store`; a failed build is
    /// logged under [`DEVICE_ID_ERROR_EVENT`] and not cached.
    pub fn get_or_init(
        &self,
        build_store: impl FnOnce() -> Result<S, String>,
        log: impl Fn(&str, &str),
    ) -> Result<SharedDeviceService<S>, String> {
        let mut cell = self
            .cell
            .lock()
            .map_err(|_| "device service lock poisoned".to_string())?;
        if let Some(service) = cell.as_ref() {
            return Ok(service.clone());
        }
        let store = build_store().inspect_err(|message| {
            log(DEVICE_ID_ERROR_EVENT, message);
        })?;
        let service = Arc::new(DeviceService::new(store));
        *cell = Some(service.clone());
        Ok(service)
    }
}

/// The stable raw device id from `service` (created on first use), with failures logged under
/// [`DEVICE_ID_ERROR_EVENT`]. Read failures use
/// `could not read device id from secure storage: <text>`. Other failures log their own text.
pub fn device_id_reported<S: SecureStore<String>>(
    service: &DeviceService<S>,
    log: impl Fn(&str, &str),
) -> Result<String, String> {
    service.get_or_create().map_err(|error| {
        let message = if error.operation == StorageOperation::Get {
            format!(
                "could not read device id from secure storage: {}",
                error.message
            )
        } else {
            error.message
        };
        log(DEVICE_ID_ERROR_EVENT, &message);
        message
    })
}

/// A device store for a platform with no compatible credential item: every operation fails.
pub struct UnsupportedDeviceStore;

impl SecureStore<String> for UnsupportedDeviceStore {
    fn get(&self) -> Result<Option<String>, String> {
        Err("device id secure storage is not supported on this platform".to_string())
    }
    fn set(&self, _: &String) -> Result<(), String> {
        self.get().map(|_| ())
    }
    fn delete(&self) -> Result<(), String> {
        self.get().map(|_| ())
    }
}
