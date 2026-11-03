//! The one resolver for a dictation app's per-user app-data root, shared by every crate that
//! reads or writes it (telemetry, settings, the engine runtime).
//!
//! The host names its data folder once at startup with [`configure_app_data_dir_name`]. The
//! production root is then the shared owner's (`rightkit_fs::app_dirs`) machine-local folder for
//! that flat name, with the dev namespace and the QA override variables switched off:
//!
//! | OS | production root |
//! |---|---|
//! | Windows | `<Local AppData known folder>\<name>` |
//! | macOS | `$HOME/Library/Application Support/<name>` |
//! | other Unix | `$XDG_DATA_HOME` (else `$HOME/.local/share`) `/<name>` |
//!
//! There is no override variable, no test root and no test-binary guard: every build resolves
//! the real per-user folder (rightkit#1437; Adrian, 2026-11-03: "dev mode, is mine ... real data.
//! snapshot and restore.").

use std::path::PathBuf;

use rightkit_fs::app_dirs::{AppDirs, AppId, Options};

static APP_DATA_DIR_NAME: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Name the app's data folder (for example `MyAppNext`). Call once, before anything resolves a
/// path. Returns false when a different name was already configured (the first one stays).
pub fn configure_app_data_dir_name(name: &'static str) -> bool {
    APP_DATA_DIR_NAME.set(name).is_ok() || APP_DATA_DIR_NAME.get() == Some(&name)
}

/// The configured data folder name.
///
/// # Panics
///
/// When [`configure_app_data_dir_name`] was never called: app data must
/// never land in a guessed folder.
pub fn app_data_dir_name() -> &'static str {
    if let Some(name) = APP_DATA_DIR_NAME.get() {
        return name;
    }
    panic!("app data folder name not configured: call app_paths::configure_app_data_dir_name at startup")
}

fn app_id() -> AppId {
    AppId::new(app_data_dir_name()).without_org()
}

/// The installed-build location: the owner's machine-local folder for the configured name, never
/// the dev namespace and never a QA override. When the OS gives no usable base folder this
/// panics with the reason: app data must never land in the current directory.
///
/// Every telemetry line resolves its path through here. The owner caches the Windows
/// known-folder resolution per process (rightkit-fs 0.4.8, rightkit#1337); uncached, it cost
/// ~12 ms per call.
pub fn production_data_root() -> PathBuf {
    resolve_production_data_root()
}

fn resolve_production_data_root() -> PathBuf {
    let options = Options::current().dev(false);
    AppDirs::resolve_with_options(app_id(), &options)
        .map(|dirs| dirs.local)
        .unwrap_or_else(|error| {
            panic!(
                "cannot resolve the per-user app data folder ({error}); refusing to use the current directory"
            )
        })
}

pub fn app_data_root() -> PathBuf {
    production_data_root()
}
