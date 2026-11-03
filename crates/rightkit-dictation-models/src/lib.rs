//! Desktop model download, verification, archive installation, activation, and cleanup.

pub mod ane_cache_prune;
pub mod archive_install;
pub mod model_download_grant;
pub mod model_fetch;
pub mod model_paths;
pub mod resumable_download;
pub mod retired_model_cleanup;
pub mod shared;
pub mod ultra_model;
pub mod whisper_model;

pub use rightkit_dictation_telemetry::{app_paths, system_identity, telemetry};

use std::sync::OnceLock;

pub struct RetiredModelSettingsProvider {
    pub selected_backend: fn() -> Option<String>,
    pub finalize_retired_switch: fn() -> Result<(), String>,
}

static RETIRED_SETTINGS: OnceLock<RetiredModelSettingsProvider> = OnceLock::new();

pub fn install_retired_settings_provider(provider: RetiredModelSettingsProvider) -> bool {
    RETIRED_SETTINGS.set(provider).is_ok()
}

mod settings {
    pub(crate) fn snapshot_asr_backend_raw() -> Option<String> {
        super::RETIRED_SETTINGS
            .get()
            .and_then(|provider| (provider.selected_backend)())
    }

    pub(crate) fn finalize_retired_unified_switch() -> Result<(), String> {
        super::RETIRED_SETTINGS
            .get()
            .ok_or_else(|| "retired model settings provider not installed".to_string())
            .and_then(|provider| (provider.finalize_retired_switch)())
    }
}
