//! Engine startup: derive every fixed root the engine needs and install it into
//! `rightkit-dictation-inference`.
//!
//! The shell used to compute these and hand them over through the sidecar environment. The
//! engine now resolves them itself, with the SAME code the shell links:
//!
//! - app data root: `rightkit_dictation_core::app_paths` (the OS account's known folder; the direct-root
//!   override exists only in test builds);
//! - models base, KWS directory, bundled ffmpeg, CoreML cache, Whisper CoreML fallback:
//!   `rightkit_dictation_models::model_paths`, which resolves bundled resources from the engine's
//!   own executable location. The installers place the engine beside the shell executable
//!   (Windows NSIS; macOS `Contents/MacOS`), with resources under `resources/` or
//!   `Contents/Resources`, and both executables see the same relative layout.
//!
//! Which model to load is not a path and is not derived here (see
//! `rightkit_dictation_inference::engine_paths::launch_model_choice`).

use rightkit_dictation_inference::engine_paths::{self, EnginePaths};
use rightkit_dictation_models::model_paths;

/// The fixed roots for this process.
pub(crate) fn resolve() -> EnginePaths {
    EnginePaths {
        app_data_root: rightkit_dictation_core::app_paths::app_data_root(),
        models_base: model_paths::install_base(),
        kws_dir: model_paths::kws_dir(),
        ffmpeg: model_paths::bundled_ffmpeg_path(),
        coreml_compiled_cache: model_paths::coreml_compiled_cache_dir(),
        whisper_coreml_dir: model_paths::whisper_coreml_fallback_dir(),
    }
}

/// Resolve and install the roots once. Called first thing in `entry::run`.
pub(crate) fn install() {
    let resolved = resolve();
    // Apple's ANE compiler persists its output only when the cache directory exists.
    #[cfg(target_os = "macos")]
    if let Err(error) = std::fs::create_dir_all(&resolved.coreml_compiled_cache) {
        tracing::warn!(
            path = %resolved.coreml_compiled_cache.display(),
            %error,
            "CoreML compiled cache directory could not be created"
        );
    }
    tracing::info!(
        app_data_root = %resolved.app_data_root.display(),
        models_base = %resolved.models_base.display(),
        kws_dir = %resolved.kws_dir.display(),
        ffmpeg = ?resolved.ffmpeg,
        "engine paths resolved"
    );
    // A second install keeps the first; an engine installs exactly once.
    let _ = engine_paths::install(resolved);
}
