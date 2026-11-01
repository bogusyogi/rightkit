//! The host app's settings document, read and written by the shell and the engine alike.
//!
//! File: `<app data>/<namespace>/settings.json` (namespace: [`crate::host::HostConfig::settings_namespace`]), opened through `rightkit-app-settings` (versioned, atomic
//! replace, quarantine on corruption). One document holds every setting; the three local-image
//! fields are the ones this module validates. `ui` and its two flags belong to the shell's Settings
//! page; they are declared here only so that a write from either process keeps them.
//!
//! Validation (checked before anything is written; a refused patch changes nothing):
//! - `idle_unload_seconds`: a whole number of seconds, 0 to 86400.
//! - `memory_mode`: accepted by [`crate::memory::parse_requested`] (`auto`, `performance`,
//!   `balanced`, `low`; empty means `auto`). Stored trimmed and lower-cased.
//! - `model_root`: an absolute path (a relative one is ignored by path resolution).
//!
//! Concurrency (rightkit-app-settings 0.2.0): a write takes an exclusive advisory lock on the sidecar
//! `settings.json.lock`, re-reads the file under that lock, applies the edit to the fresh copy,
//! refreshes `settings.json.bak`, and replaces the file atomically (temp file, then rename, then a
//! directory sync on Unix and macOS). Two processes writing different fields lose nothing. A
//! writer that cannot take the lock within 10 seconds gets a lock-timeout error. Reads take no
//! lock; they see the old or the new file, never a mix. A process's cached value is refreshed when
//! the file's identity or stamp changes, which is the next read after another process's write.

use std::path::Path;

use rightkit_app_settings::{Schema, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::memory;

/// The settings namespace in force (the host's [`crate::host::HostConfig::settings_namespace`]).
pub fn namespace() -> &'static str {
    crate::host::config().settings_namespace.as_str()
}
pub const VERSION: u32 = 1;
/// The largest accepted `idle_unload_seconds`.
pub const IDLE_UNLOAD_MAX_SECONDS: u64 = 86_400;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Local-image model directory (was `config.json` `model_root`).
    pub model_root: Option<String>,
    /// sd-server idle-unload seconds (was `config.json` `idle_unload_seconds`).
    pub idle_unload_seconds: Option<u64>,
    /// Local image memory mode: `auto` (default when unset), `performance`, `balanced`, `low`.
    /// Read by the desktop service and by the engine (same key in `settings.json`).
    pub memory_mode: Option<String>,
    /// Frontend Settings page values (dotted keys, `@rightkit/settings-ui`). Shell-owned.
    pub ui: Map<String, Value>,
}

/// Opens the settings document under `app_data` (the app data root).
pub fn open(app_data: &Path) -> Result<Store<AppSettings>, rightkit_app_settings::Error> {
    Store::<AppSettings>::open(app_data, Schema::new(namespace(), VERSION))
}

/// The saved settings, read without writing: no lock, no migration. A process that must not create
/// or change the file (a provider that reads the model root) calls this. A missing or unreadable
/// document reads as the defaults.
pub fn read(app_data: &Path) -> AppSettings {
    let path = app_data.join(namespace()).join("settings.json");
    let options = rightkit_app_settings::StoreOptions::new(Schema::new(namespace(), VERSION));
    Store::<AppSettings>::open_read_only(&path, options)
        .map(|store| store.get())
        .unwrap_or_default()
}

/// A partial change to the local-image fields. Outer `None` leaves a field alone; `Some(None)`
/// clears it; `Some(Some(v))` sets it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalImagePatch {
    pub model_root: Option<Option<String>>,
    pub idle_unload_seconds: Option<Option<u64>>,
    pub memory_mode: Option<Option<String>>,
}

impl LocalImagePatch {
    /// Checks every set value and returns it in stored form. Errors are `invalid_request: ...`.
    pub fn validated(self) -> Result<Self, String> {
        let model_root = match self.model_root {
            Some(Some(path)) => {
                if !Path::new(&path).is_absolute() {
                    return Err("invalid_request: model_root must be an absolute path".to_string());
                }
                Some(Some(path))
            }
            other => other,
        };
        let idle_unload_seconds = match self.idle_unload_seconds {
            Some(Some(seconds)) if seconds > IDLE_UNLOAD_MAX_SECONDS => {
                return Err(format!(
                    "invalid_request: idle_unload_seconds must be 0..{IDLE_UNLOAD_MAX_SECONDS}"
                ));
            }
            other => other,
        };
        let memory_mode = match self.memory_mode {
            Some(Some(mode)) => {
                memory::parse_requested(&mode)?;
                Some(Some(mode.trim().to_ascii_lowercase()))
            }
            other => other,
        };
        Ok(Self {
            model_root,
            idle_unload_seconds,
            memory_mode,
        })
    }
}

/// Validates `patch` and commits it in one write. A refused patch writes nothing.
pub fn apply(store: &Store<AppSettings>, patch: LocalImagePatch) -> Result<(), String> {
    let patch = patch.validated()?;
    store
        .update(move |s| {
            if let Some(root) = patch.model_root {
                s.model_root = root;
            }
            if let Some(seconds) = patch.idle_unload_seconds {
                s.idle_unload_seconds = seconds;
            }
            if let Some(mode) = patch.memory_mode {
                s.memory_mode = mode;
            }
            Ok(())
        })
        .map_err(|e| format!("write settings: {e}"))
}
