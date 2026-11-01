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
use rightkit_onnx::RuntimeError;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::semantic::model_install::{
        model_spec, scratch_dir, store_app, PinnedFile, MODEL_ID,
    };
    use rightkit_models::InstallOptions;

    #[test]
    fn model_dir_prefers_env_override() {
        let store = Store::new(scratch_dir("env-store"), &store_app());
        let (dir, source) = locate_model_with(
            &store,
            MODEL_ID,
            None,
            Some(PathBuf::from("/tmp/some-model-dir")),
        )
        .expect("env override resolves without a resource base");
        assert_eq!(dir, PathBuf::from("/tmp/some-model-dir"));
        assert_eq!(source, ModelSource::EnvOverride);
    }

    #[test]
    fn model_dir_errors_without_install_base_or_override() {
        let store = Store::new(scratch_dir("none-store"), &store_app());
        assert!(locate_model_with(&store, MODEL_ID, None, None).is_err());
    }

    #[test]
    fn empty_bundled_dir_keeps_the_incomplete_bundle_error() {
        let store = Store::new(scratch_dir("inc-store"), &store_app());
        let base = scratch_dir("inc-res");
        let err = locate_model_with(&store, MODEL_ID, Some(&base), None).unwrap_err();
        assert!(err.contains("bundled embed model incomplete"), "{err}");
    }

    #[test]
    fn installed_location_is_preferred_over_resource_dir() {
        let root = scratch_dir("pref-store");
        let src = scratch_dir("pref-src");
        std::fs::write(src.join("a.bin"), b"hello").unwrap();
        let store = Store::new(&root, &store_app());
        let pins = [PinnedFile {
            path: "a.bin",
            bytes: 5,
            sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        }];
        let spec = model_spec(MODEL_ID, "t1", &pins, "http://127.0.0.1:9");
        let opts = InstallOptions {
            require_pinned: true,
            ..InstallOptions::default()
        };
        store
            .adopt(&spec, &opts, |f| Some(src.join(&f.path)))
            .unwrap();
        // A complete-looking bundled dir must lose to the installed one.
        let base = scratch_dir("pref-res");
        let bundled = bundled_model_dir(&base);
        std::fs::create_dir_all(&bundled).unwrap();
        for f in [
            "model_q4.onnx",
            "model_q4.onnx_data",
            "tokenizer.json",
            "config.json",
        ] {
            std::fs::write(bundled.join(f), b"x").unwrap();
        }
        let (dir, source) = locate_model_with(&store, MODEL_ID, Some(&base), None).unwrap();
        assert_eq!(source, ModelSource::Installed);
        assert_eq!(dir, store.resolve(MODEL_ID).unwrap().dir);
        assert_ne!(dir, bundled);
        // Without the install the bundled copy is used.
        let empty = Store::new(scratch_dir("pref-empty"), &store_app());
        let (dir, source) = locate_model_with(&empty, MODEL_ID, Some(&base), None).unwrap();
        assert_eq!(source, ModelSource::Bundled);
        assert_eq!(dir, bundled);
    }

    #[test]
    fn no_candidates_is_an_error() {
        assert!(runtime_from_candidates(Vec::new()).is_err());
    }

    #[test]
    fn bundled_runtime_in_resources_is_a_candidate_on_every_platform() {
        let base = scratch_dir("rt-res");
        let dir = base.join("resources").join("onnxruntime");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(rightkit_onnx::runtime_filename());
        std::fs::write(&file, b"not a real runtime").unwrap();
        let got = onnxruntime_dylib_path(Some(&base)).expect("bundled runtime resolves");
        assert_eq!(got, file.canonicalize().unwrap());
    }

    #[test]
    fn missing_runtime_reports_not_found_not_a_hardcoded_refusal() {
        let base = scratch_dir("rt-missing");
        let err = onnxruntime_dylib_path(Some(&base)).unwrap_err();
        assert!(err.contains("ONNX Runtime unavailable"), "{err}");
    }

    #[test]
    fn verify_model_files_present_reports_missing_files() {
        let dir = scratch_dir("verify");
        let err = verify_model_files_present(&dir, "embeddinggemma-300m-q4").unwrap_err();
        assert!(err.contains("model_q4.onnx"));
    }

    fn touch(dir: &Path, names: &[&str]) {
        for f in names {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
    }

    #[test]
    fn verify_accepts_the_q4_set_without_tokenizer_side_files() {
        let dir = scratch_dir("verify-q4");
        touch(
            &dir,
            &[
                "model_q4.onnx",
                "model_q4.onnx_data",
                "tokenizer.json",
                "config.json",
            ],
        );
        verify_model_files_present(&dir, "embeddinggemma2-q4").expect("q4 set is complete");
    }

    #[test]
    fn verify_refuses_an_unreadable_e40_manifest_and_accepts_the_legacy_bundle() {
        let manifest = scratch_dir("verify-e40-manifest");
        touch(
            &manifest,
            &[
                "manifest.json",
                "model_32.onnx",
                "model.onnx_data",
                "embed_tokens.i8.npy",
                "tokenizer.json",
            ],
        );
        let err = verify_model_files_present(&manifest, "embeddinggemma2-e40-dml").unwrap_err();
        assert!(err.contains("bundled embed model incomplete"), "{err}");

        let legacy = scratch_dir("verify-e40-legacy");
        touch(
            &legacy,
            &[
                "model_32.onnx",
                "model_64.onnx",
                "model_128.onnx",
                "model_256.onnx",
                "model.onnx_data",
                "embed_tokens.f16.npy",
                "tokenizer.json",
            ],
        );
        let err = verify_model_files_present(&legacy, "embeddinggemma2-e40-dml").unwrap_err();
        assert!(err.contains("model_512.onnx"), "{err}");
        touch(&legacy, &["model_512.onnx"]);
        verify_model_files_present(&legacy, "embeddinggemma2-e40-dml").expect("legacy set");
    }
}
