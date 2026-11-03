//! Locate the embed-model directory and the ONNX Runtime library at runtime.
//!
//! Model resolution order:
//!   0. `EMBED_MODEL_DIR` (test-run selector, `test_selector!`).
//!   1. The `rightkit-models` install location (per-user store, digest-pinned;
//!      see `model_install`).
//!   2. The bundled Tauri resource dir (`resources/embed_model`), kept so a
//!      packaged model still works.
//!
//! When none exists the error is a clear FAIL-LOUD message (never a silent
//! fallback to another embedding space); `semantic_model_status` surfaces the
//! same text to the UI.
//!
//! ONNX Runtime is obtained through `rightkit-onnx` only (candidate validation,
//! System32 rejection). Nothing here loads a library by itself.

use rightkit_models::Store;
use std::path::{Path, PathBuf};

/// Where a resolved model directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    EnvOverride,
    Installed,
    Bundled,
}

/// Path selectors, read only through `MailStoreHost::test_selector`: a value exists only in a
/// test run.
fn model_dir_override() -> Option<PathBuf> {
    crate::host::host()
        .test_selector("EMBED_MODEL_DIR")
        .map(|v| PathBuf::from(v.trim()))
}

fn onnxruntime_override() -> Option<PathBuf> {
    crate::host::host()
        .test_selector("ONNXRUNTIME_DYLIB")
        .map(|v| PathBuf::from(v.trim()))
}

/// Bundled resource model directory under a Tauri resource base.
pub fn bundled_model_dir(resource_base: &Path) -> PathBuf {
    resource_base.join("resources").join("embed_model")
}

/// [`locate_model_with`] reading the dev override from the environment.
pub fn locate_model(
    store: &Store,
    model_id: &str,
    resource_base: Option<&Path>,
) -> Result<(PathBuf, ModelSource), String> {
    locate_model_with(store, model_id, resource_base, model_dir_override())
}

/// Resolve the model directory and report its source.
pub fn locate_model_with(
    store: &Store,
    model_id: &str,
    resource_base: Option<&Path>,
    env_override: Option<PathBuf>,
) -> Result<(PathBuf, ModelSource), String> {
    if let Some(dir) = env_override {
        return Ok((dir, ModelSource::EnvOverride));
    }
    if let Some(resolved) = store.resolve(model_id) {
        return Ok((resolved.dir, ModelSource::Installed));
    }
    let base = resource_base.ok_or_else(|| {
        "semantic model not installed and no resource base is available".to_string()
    })?;
    let bundled = bundled_model_dir(base);
    verify_model_files_present(&bundled, model_id)?;
    Ok((bundled, ModelSource::Bundled))
}

/// Resolve the ONNX Runtime shared library through `rightkit-onnx`.
///
/// Candidates, in order, on every platform including Windows:
///   1. `ONNXRUNTIME_DYLIB` (test-run selector, `test_selector!`).
///   2. The bundled `resources/onnxruntime/<runtime_filename()>`.
///
/// `rightkit-onnx` still rejects the System32 (Windows ML) copy and relative
/// paths. A Windows build must ship `onnxruntime.dll` (plus `DirectML.dll`
/// beside it) in the resource dir; until one is shipped the result is the
/// honest "not found" error, not a hardcoded refusal.
pub fn onnxruntime_dylib_path(resource_base: Option<&Path>) -> Result<PathBuf, String> {
    runtime_from_candidates(runtime_candidates(resource_base, onnxruntime_override()))
}

fn runtime_candidates(
    resource_base: Option<&Path>,
    env_override: Option<PathBuf>,
) -> Vec<rightkit_onnx::RuntimeCandidate> {
    use rightkit_onnx::{CandidateSource, RuntimeCandidate};
    let mut out = Vec::new();
    if let Some(p) = env_override {
        out.push(RuntimeCandidate::new(p, CandidateSource::Explicit));
    }
    if let Some(base) = resource_base {
        out.push(RuntimeCandidate::new(
            base.join("resources")
                .join("onnxruntime")
                .join(rightkit_onnx::runtime_filename()),
            CandidateSource::Bundled,
        ));
    }
    out
}

fn runtime_from_candidates(
    candidates: Vec<rightkit_onnx::RuntimeCandidate>,
) -> Result<PathBuf, String> {
    match rightkit_onnx::resolve_runtime(candidates) {
        Ok(selection) => Ok(selection.path),
        Err(RuntimeError::NoCandidates) => Err(no_candidates_reason().to_string()),
        Err(e) => Err(format!("ONNX Runtime unavailable: {e}")),
    }
}

fn no_candidates_reason() -> &'static str {
    "no resource base available for the ONNX Runtime"
}

/// FAIL-LOUD existence check: every file the loader will read must be
/// present before we ever construct the embedder, so a missing/corrupt
/// bundle surfaces as one clear error instead of a confusing embedder
/// internal failure three calls deep.
///
/// The file set is the one `rightkit_embed::bundle_files` lists for the layout
/// found in `dir` (Core ML package, E40 bundle, or the Q4 model), and the
/// crate checks that every listed file exists.
pub fn verify_model_files_present(dir: &Path, model_id: &str) -> Result<(), String> {
    match rightkit_embed::bundle_files(dir) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(format!(
            "bundled embed model incomplete at {dir:?}: no model bundle found (model_q4.onnx missing) (model_id={model_id})"
        )),
        Err(e) => Err(format!(
            "bundled embed model incomplete at {dir:?}: {e} (model_id={model_id})"
        )),
    }
}
