//! Fixed roots the engine resolves for itself, plus the one place that still reads the launch
//! model choice.
//!
//! Release rule (`@rightkit/release` switch classes S3/S4): a release engine reads no environment
//! variable to choose a path, library, executable or model. The roots below are derived by
//! `rightkit-dictation-runtime` at startup (`engine_paths::resolve`, using the same `model_paths` and
//! `app_paths` code the shell links, and the engine's own executable location for bundled
//! resources) and installed here once. Every former `RIGHTKIT_*` path read goes through an accessor in
//! this module.
//!
//! The model CHOICE (backend alias, shared-store variant directory, load-check flag) is not a
//! path the engine can derive: it is what the shell decided for this launch. It arrives in the
//! launch frame (first stdin line, `rightkit_dictation_core::engine::launch`), validated and installed by
//! `rightkit-dictation-runtime`, and is read only through [`launch_model_choice`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Roots resolved once at engine startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnginePaths {
    /// Per-user app data root.
    pub app_data_root: PathBuf,
    /// Base directory of the app-private models (`<app_data_root>/models`).
    pub models_base: PathBuf,
    /// Bundled KWS resources.
    pub kws_dir: PathBuf,
    /// Bundled ffmpeg, when the installation has one.
    pub ffmpeg: Option<PathBuf>,
    /// Compiled CoreML model cache (`<app_data_root>/coreml-cache`).
    pub coreml_compiled_cache: PathBuf,
    /// Whisper CoreML bundle fallback (`<models_base>/coreml/whisper-multi`).
    pub whisper_coreml_dir: PathBuf,
}

static INSTALLED: OnceLock<EnginePaths> = OnceLock::new();

/// Install the resolved roots. Only the first install takes effect; a second returns its argument.
pub fn install(paths: EnginePaths) -> Result<(), EnginePaths> {
    INSTALLED.set(paths)
}

/// The installed roots, if startup installed them.
pub fn installed() -> Option<&'static EnginePaths> {
    INSTALLED.get()
}

/// Models base directory: the installed root.
pub fn models_base_dir() -> Option<PathBuf> {
    installed().map(|paths| paths.models_base.clone())
}

/// Bundled KWS directory: the installed root.
pub fn kws_dir() -> Option<PathBuf> {
    installed().map(|paths| paths.kws_dir.clone())
}

/// Bundled ffmpeg: the installed root.
pub fn ffmpeg_path() -> Option<PathBuf> {
    installed().and_then(|paths| paths.ffmpeg.clone())
}

/// Compiled CoreML model cache directory: the installed root.
#[cfg(target_os = "macos")]
pub fn coreml_compiled_cache_dir() -> Option<PathBuf> {
    installed().map(|paths| paths.coreml_compiled_cache.clone())
}

/// Whisper CoreML bundle fallback: the installed root.
#[cfg(target_os = "macos")]
pub fn whisper_coreml_dir() -> Option<PathBuf> {
    installed().map(|paths| paths.whisper_coreml_dir.clone())
}

/// What the shell decided for this launch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchModelChoice {
    /// ASR backend alias (`parakeet-tdt`, ...); `None` means the persisted setting decides.
    pub backend: Option<String>,
    /// macOS: the resolved Parakeet bundle directory (may be a shared-store variant).
    pub coreml_model_dir: Option<PathBuf>,
    /// Windows: the resolved Parakeet model directory or subdirectory name.
    pub asr_model_subdir: Option<String>,
    /// True for a temporary load-check child that must not take the model lease.
    pub load_check: bool,
}

static PARAKEET_PINNED: AtomicBool = AtomicBool::new(false);
static PINNED_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static LAUNCH: OnceLock<rightkit_dictation_core::engine::launch::EngineLaunch> = OnceLock::new();

/// Roles that get no launch frame (the headless `--speech-service` host, launched detached with
/// stdin closed, and the `calibrate` / `calibrate-route` one-shots) always serve
/// Parakeet TDT and never lend a load-check lease. `dir` is the Parakeet directory the role
/// resolved itself (the shared-store Ultra copy, which never sits under the app's own models
/// root); `None` keeps the derived models root. Call once at startup, before any model load.
pub fn pin_parakeet_model_choice(dir: Option<PathBuf>) {
    let _ = PINNED_DIR.set(dir);
    PARAKEET_PINNED.store(true, Ordering::SeqCst);
}

/// Install the launch frame the shell sent. `rightkit-dictation-runtime` validates the directories
/// before installing. Only the first install takes effect.
pub fn install_launch(launch: rightkit_dictation_core::engine::launch::EngineLaunch) {
    let _ = LAUNCH.set(launch);
}

/// THE ONE READ of the launch model choice: the pinned Parakeet role, else the installed launch
/// frame. A process started without a frame has made no choice.
pub fn launch_model_choice() -> LaunchModelChoice {
    if PARAKEET_PINNED.load(Ordering::SeqCst) {
        let dir = PINNED_DIR.get().cloned().flatten();
        return LaunchModelChoice {
            backend: Some("parakeet-tdt".to_string()),
            coreml_model_dir: dir.clone().filter(|_| cfg!(target_os = "macos")),
            asr_model_subdir: dir
                .filter(|_| !cfg!(target_os = "macos"))
                .map(|dir| dir.to_string_lossy().into_owned()),
            ..LaunchModelChoice::default()
        };
    }
    if let Some(launch) = LAUNCH.get() {
        return LaunchModelChoice {
            backend: Some(launch.backend.clone()),
            coreml_model_dir: launch.coreml_model_dir.clone(),
            asr_model_subdir: launch
                .asr_model_subdir
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            load_check: launch.load_check,
        };
    }
    // No frame means no choice was made for this process; the persisted setting decides the
    // backend and the engine derives the Parakeet directory.
    LaunchModelChoice::default()
}
