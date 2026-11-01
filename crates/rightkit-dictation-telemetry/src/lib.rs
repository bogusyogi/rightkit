//! Desktop diagnostics, telemetry transport, crashes, and system metrics.

pub mod app_paths;
pub mod crash_dump;
pub mod http_client;
pub use rightkit_logs::system::{identity as system_identity, metrics as system_metrics};
pub mod feedback;
pub mod legacy_telemetry_cleanup;
pub mod telemetry;
mod telemetry_writer;

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default)]
pub struct TelemetrySettings {
    pub developer_mode: bool,
    pub diagnostics_enabled: bool,
    /// Metadata belongs to host application, not this dependency crate.
    pub app_version: Option<&'static str>,
    pub app_revision: Option<&'static str>,
}

static SETTINGS_PROVIDER: OnceLock<fn() -> TelemetrySettings> = OnceLock::new();

pub fn install_settings_provider(provider: fn() -> TelemetrySettings) -> bool {
    let installed = SETTINGS_PROVIDER.set(provider).is_ok();
    if installed {
        spawn_legacy_telemetry_cleanup();
    }
    // A fresh provider may report a different consent state — drop the
    // cached diagnostics-enabled flag so the next event consults it.
    telemetry::notify_settings_changed();
    installed
}

/// The host installs the settings provider once, early in startup: remove what the retired
/// telemetry pipeline left behind, off the startup path. Best effort; failures stay local.
fn spawn_legacy_telemetry_cleanup() {
    let _ = std::thread::Builder::new()
        .name("legacy-telemetry-cleanup".to_string())
        .spawn(|| {
            let _ = legacy_telemetry_cleanup::remove_legacy_telemetry_files();
        });
}

mod settings {
    #[derive(Clone, Copy, Debug, Default)]
    pub(crate) struct Snapshot {
        pub(crate) app_version: Option<&'static str>,
    }

    pub(crate) fn snapshot() -> Snapshot {
        let settings = super::SETTINGS_PROVIDER
            .get()
            .map(|provider| provider())
            .unwrap_or_default();
        Snapshot {
            app_version: settings.app_version,
        }
    }

    pub(crate) fn diagnostics_enabled() -> bool {
        super::SETTINGS_PROVIDER
            .get()
            .map(|provider| provider().diagnostics_enabled)
            .unwrap_or(false)
    }
}

/// Fallback keeps contract helpers deterministic when telemetry is exercised
/// without the desktop host's settings bridge.
pub(crate) const DEFAULT_HOST_APP_VERSION: &str = "0.0.0";

pub(crate) fn host_app_version() -> &'static str {
    settings::snapshot()
        .app_version
        .unwrap_or(DEFAULT_HOST_APP_VERSION)
}
