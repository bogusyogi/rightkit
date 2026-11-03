//! Where the host keeps voice models and the ONNX Runtime library.
//!
//! The manifest (file names, sizes, SHA-256), `files_present`/`verify`, `sha256_file`, the
//! runtime file name and the default backend belong to `rightkit-tts` and are re-exported here
//! unchanged. What stays local is the host layout:
//!
//! - the `rightkit-models` store beside the pre-store flat folder
//!   (`<PREFIX>_QTS_MODEL_DIR` or `<data>/models/qts`), so an existing install is used in place;
//! - the 1.7B CustomVoice / VoiceDesign folder (`<PREFIX>_QTS_MODEL_DIR_1P7B`);
//! - the shared ONNX Runtime folder and the resolution order for it, which the other local
//!   ONNX providers (musicgen, lipsync) share.
use rightkit_tts::models as rk;
use rightkit_tts::Mode;
use std::path::{Path, PathBuf};

pub use rk::{default_backend, files_present, runtime_lib_name, sha256_file, verify};
pub use rk::{ManifestFile, DOWNLOAD_BYTES, HF_REPO, MANIFEST, MODEL_ID};

/// Revision and quantisation of the store entry (`rightkit-models`).
pub const REVISION: &str = "qts-1";
pub const QUANT: &str = "q8_0";

pub fn data_root() -> PathBuf {
    crate::host::app_data_root()
}

/// Pre-store flat folder: `<PREFIX>_QTS_MODEL_DIR` or `<data>/models/qts`.
/// Models installed there before the shared store are still used in place.
pub fn legacy_model_dir() -> PathBuf {
    match crate::host::env_var("QTS_MODEL_DIR").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => data_root().join("models").join("qts"),
    }
}

/// Root of the `rightkit-models` store that new installs go to (beside the
/// flat folder, so `<PREFIX>_QTS_MODEL_DIR` relocates both).
pub fn store_root() -> PathBuf {
    let legacy = legacy_model_dir();
    legacy
        .parent()
        .map(|p| p.join("qts-store"))
        .unwrap_or_else(|| legacy.join("store"))
}

/// Where the shared ONNX Runtime library lives (`FileSpec::shared`).
pub fn shared_runtime_dir() -> PathBuf {
    data_root().join("runtime").join("qts")
}

pub fn store() -> rightkit_models::Store {
    let store =
        rightkit_models::Store::new(store_root(), rightkit_local_gen::model_store::store_app());
    rightkit_local_gen::model_store::with_legacy_alias(store, MODEL_ID, &legacy_model_dir())
        .with_shared_root(ORT_SHARED_KIND, shared_runtime_dir())
        .expect("onnxruntime is a valid shared-runtime kind")
}

/// Shared-runtime kind of the ONNX Runtime library.
pub const ORT_SHARED_KIND: &str = "onnxruntime";

/// The store's active revision folder for the QTS model, when intact.
pub fn store_dir() -> Option<PathBuf> {
    store().active_dir(MODEL_ID)
}

/// The folder the worker loads Base from: the store's active revision, else the
/// pre-store flat folder (so an existing install reads as installed).
pub fn model_dir() -> PathBuf {
    store()
        .resolve_or_legacy(MODEL_ID, &legacy_model_dir())
        .filter(|d| files_present(d))
        .unwrap_or_else(legacy_model_dir)
}

/// 1.7B exports are local-only, outside the download manifest/catalog.
/// Prefer explicit directory, then the active store's QTS folder, then legacy.
pub fn model_dir_for_mode(mode: Mode) -> PathBuf {
    if mode == Mode::Base {
        return model_dir();
    }
    if let Some(dir) = crate::host::env_var("QTS_MODEL_DIR_1P7B").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    let dir = store_dir().unwrap_or_else(model_dir);
    if mode_files_present(&dir, mode) {
        dir
    } else {
        legacy_model_dir()
    }
}

/// Vocoder `mode` runs with from `dir`: the folder's own, else the Base folder's. Mirrors the
/// location rule `rightkit-tts` applies for a mode registered without a vocoder.
pub fn mode_vocoder(dir: &Path) -> PathBuf {
    let local = dir.join(rightkit_tts::mode_location::VOCODER_FILE);
    if local.is_file() {
        local
    } else {
        model_dir().join(rightkit_tts::mode_location::VOCODER_FILE)
    }
}

/// Files `mode` needs in `dir`, for status display: the manifest for Base, else the 1.7B GGUF
/// files as `rightkit-tts-qwen` resolves them, plus the vocoder.
pub fn mode_files(dir: &Path, mode: Mode) -> Vec<PathBuf> {
    if mode == Mode::Base {
        return MANIFEST.iter().map(|f| dir.join(f.name)).collect();
    }
    let mut paths = rk::mode_model_files(dir, mode);
    paths.push(mode_vocoder(dir));
    paths
}

/// Presence check without hashing. Base keeps the exact manifest sizes; the 1.7B modes are
/// present when `rightkit-tts` reports nothing missing.
pub fn mode_files_present(dir: &Path, mode: Mode) -> bool {
    if mode == Mode::Base {
        return files_present(dir);
    }
    rk::missing_for_mode(dir, &mode_vocoder(dir), mode).is_empty()
}

/// Local voice records (x-vector prompts) live here. Same folder as
/// `rightkit_tts::models::try_voices_dir_in(Some(data_root()))`.
pub fn voices_dir() -> PathBuf {
    data_root().join("voices").join("local")
}

/// Resolve and configure ONNX Runtime (>= 1.24, `api-24`) through RightKit.
/// Debug builds first accept `ORT_DYLIB_PATH` from the environment.
/// Bundled locations precede app-data locations; release builds have no override.
pub fn find_onnxruntime() -> Option<PathBuf> {
    use rightkit_onnx::{RuntimeCandidate, RuntimeError};
    let selected = resolve_onnxruntime().ok()?;
    match rightkit_onnx::configure_runtime([RuntimeCandidate::new(selected.path, selected.source)])
    {
        Ok(configured) => Some(configured.path),
        Err(RuntimeError::AlreadyConfigured { configured, .. }) => Some(configured),
        Err(_) => None,
    }
}

/// Initialize ONNX Runtime through RightKit, retaining any existing environment.
pub fn init_onnxruntime() -> Result<PathBuf, String> {
    use rightkit_onnx::{init_environment_with_options, EnvironmentInitOptions};
    let selected = resolve_onnxruntime()?;
    init_environment_with_options(
        &selected,
        &EnvironmentInitOptions {
            tolerate_already_initialized: true,
            ..Default::default()
        },
    )
    .map(|report| report.runtime_path)
}

/// The ONNX Runtime library the host would use, resolved without configuring this process.
/// The voice client hands it to the worker (`QtsClient::with_ort_runtime`), so installs that
/// predate the shared-runtime layout keep working.
pub fn resolved_onnxruntime() -> Option<PathBuf> {
    resolve_onnxruntime().ok().map(|selected| selected.path)
}

/// macOS: the library inside the app bundle, `Contents/Resources/runtime/libonnxruntime.dylib`,
/// derived from the running executable (`Contents/MacOS/<exe>`). Present or not, this is the only
/// place the signed app loads ONNX Runtime from.
#[cfg(target_os = "macos")]
pub fn bundled_onnxruntime_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    rightkit_onnx::installed_runtime_candidate(&exe, "runtime").map(|c| c.path)
}

/// macOS candidates: the bundled copy.
/// ONNX Runtime is never read from app data here: a library downloaded at run time keeps its
/// ad hoc signature and cannot pass library validation in the hardened engine.
#[cfg(target_os = "macos")]
fn onnxruntime_candidates() -> Vec<rightkit_onnx::RuntimeCandidate> {
    use rightkit_onnx::{CandidateSource, RuntimeCandidate};
    let mut candidates = Vec::new();
    if let Some(path) = bundled_onnxruntime_path() {
        candidates.push(RuntimeCandidate::new(path, CandidateSource::Bundled));
    }
    candidates
}

#[cfg(not(target_os = "macos"))]
fn onnxruntime_candidates() -> Vec<rightkit_onnx::RuntimeCandidate> {
    use rightkit_onnx::{CandidateSource, RuntimeCandidate};
    let name = runtime_lib_name();
    let mut candidates = Vec::new();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        candidates.push(RuntimeCandidate::new(
            exe_dir.join("runtime").join(name),
            CandidateSource::Bundled,
        ));
        candidates.push(RuntimeCandidate::new(
            exe_dir.join(name),
            CandidateSource::Bundled,
        ));
    }
    // Installs made before the shared-runtime layout kept the library here.
    if let Some(d) = store_dir() {
        candidates.push(RuntimeCandidate::new(
            d.join("runtime").join(name),
            CandidateSource::AppData,
        ));
    }
    if let Some(parent) = legacy_model_dir().parent() {
        candidates.push(RuntimeCandidate::new(
            parent.join("runtime").join(name),
            CandidateSource::AppData,
        ));
    }
    candidates.push(RuntimeCandidate::new(
        shared_runtime_dir().join(name),
        CandidateSource::AppData,
    ));
    candidates
}

fn resolve_onnxruntime() -> Result<rightkit_onnx::RuntimeSelection, String> {
    let mut candidates = onnxruntime_candidates();
    // Preserve the existing treatment of relative app-data roots. Explicit
    // candidates reach RightKit unchanged so its path validation remains authoritative.
    candidates
        .retain(|c| c.source == rightkit_onnx::CandidateSource::Explicit || c.path.is_absolute());
    rightkit_onnx::resolve_runtime(candidates).map_err(|error| error.to_string())
}

/// What the doctor reports for ONNX Runtime.
#[derive(Debug, Clone)]
pub struct OnnxRuntimeReport {
    /// The resolved library, else where this platform expects it (macOS: the bundled copy).
    pub path: Option<PathBuf>,
    pub exists: bool,
    /// Whether the library loads in this process.
    pub loadable: bool,
    /// The loader's error text when it does not.
    pub error: Option<String>,
}

/// Resolve and try to load ONNX Runtime in this process (no model needed).
pub fn onnxruntime_report() -> OnnxRuntimeReport {
    #[cfg(target_os = "macos")]
    let expected = bundled_onnxruntime_path();
    #[cfg(not(target_os = "macos"))]
    let expected = Some(shared_runtime_dir().join(runtime_lib_name()));
    let path = resolved_onnxruntime().or(expected);
    let exists = path.as_ref().is_some_and(|p| p.is_file());
    match init_onnxruntime() {
        Ok(_) => OnnxRuntimeReport {
            path,
            exists,
            loadable: true,
            error: None,
        },
        Err(e) => OnnxRuntimeReport {
            path,
            exists,
            loadable: false,
            error: Some(e),
        },
    }
}
