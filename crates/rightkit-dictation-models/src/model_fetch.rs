//! Phase D — model resolution + download-on-demand + integrity verify.
//!
//! A model that is already present is a presence check that no-ops. When it is
//! ABSENT and a `<subdir>.manifest.json` is shipped beside the models, this
//! downloads each required file to the models dir and sha256-verifies it
//! (atomic temp → rename). `RIGHTKIT_MODEL_BASE_URL` can repoint the manifest's base
//! URL in test-mode builds only; release builds ignore it.
//!
//! No manifest ships today, so the download branch stays dormant and an absent
//! model is an error.
//!
//! DORMANT — not wired to any live command path; live model downloads are
//! `whisper_model.rs` / engine-side. Kept for the unified-fetcher future.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Always-named files that must be present in a usable HrTransducer model dir.
/// (Encoder / decoder_joint filenames vary by quantization — checked separately
/// by `has_model_weights`; HrTransducer::load picks the concrete files.)
pub const REQUIRED_FILES: &[&str] = &["config.json", "vocab.txt"];

/// Resolve the models base dir — app-data ONLY, the single model location on
/// every machine. `RIGHTKIT_APP_DATA_DIR` (via `app_paths`) moves the app-data root for
/// QA isolation; there is deliberately no dev/staged-model or `RIGHTKIT_MODELS_DIR`
/// escape hatch here (that env is only the shell→sidecar channel, see
/// `model_paths::resolve_models_base`).
pub fn models_base() -> PathBuf {
    crate::app_paths::app_data_file("models")
}

fn files_present(dir: &Path, required: &[&str]) -> bool {
    dir.is_dir()
        && required.iter().all(|f| {
            fs::metadata(dir.join(f))
                .map(|m| m.is_file() && m.len() > 0)
                .unwrap_or(false)
        })
}

fn has_model_weights(dir: &Path) -> bool {
    let enc = [
        "encoder-model.int8.onnx",
        "encoder-model.onnx",
        "encoder.onnx",
    ]
    .iter()
    .any(|f| dir.join(f).is_file());
    let dec = [
        "decoder_joint-model.int8.onnx",
        "decoder_joint-model.onnx",
        "decoder_joint.onnx",
        "decoder_joint.int8.onnx",
    ]
    .iter()
    .any(|f| dir.join(f).is_file());
    enc && dec
}

/// Ensure model `subdir` under `base` is present + complete; returns its path.
/// Downloads + verifies if absent AND a source is configured, else a clear,
/// actionable error.
pub fn ensure_model(base: &Path, subdir: &str, required: &[&str]) -> Result<PathBuf, String> {
    let dir = base.join(subdir);
    if files_present(&dir, required) && has_model_weights(&dir) {
        return Ok(dir);
    }
    if let Some(manifest) = load_manifest(base, subdir)? {
        download_model(&dir, &manifest)?;
        if files_present(&dir, required) && has_model_weights(&dir) {
            return Ok(dir);
        }
    }
    Err(format!(
        "model '{subdir}' not found at {} and no download source configured \
         (ship {subdir}.manifest.json or place the model files there)",
        dir.display()
    ))
}

#[derive(Debug, serde::Deserialize)]
pub struct ModelManifest {
    pub base_url: String,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ManifestFile {
    pub name: String,
    pub sha256: String,
}

/// Load `<base>/<subdir>.manifest.json` if present. `RIGHTKIT_MODEL_BASE_URL`
/// overrides the manifest's `base_url` (lets a build/CI point at a mirror).
pub fn load_manifest(base: &Path, subdir: &str) -> Result<Option<ModelManifest>, String> {
    let p = base.join(format!("{subdir}.manifest.json"));
    if !p.is_file() {
        return Ok(None);
    }
    let mut m: ModelManifest = serde_json::from_slice(&fs::read(&p).map_err(|e| e.to_string())?)
        .map_err(|e| format!("parse {}: {e}", p.display()))?;
    if let Some(env) = rightkit_dictation_core::dev_var_os!("RIGHTKIT_MODEL_BASE_URL") {
        m.base_url = env.to_string_lossy().to_string();
    }
    Ok(Some(m))
}

/// Reject any manifest file name that isn't a single plain path component —
/// blocks `..` traversal, absolute paths, and drive/root prefixes before the
/// name is joined onto the on-disk models dir.
pub fn reject_path_traversal(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(()),
        _ => Err(format!(
            "manifest file name '{name}' is not a single plain path component (rejected)"
        )),
    }
}

fn download_model(dir: &Path, manifest: &ModelManifest) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for f in &manifest.files {
        reject_path_traversal(&f.name)?;
        let dest = dir.join(&f.name);
        if dest.is_file()
            && sha256_file(&dest)
                .map(|h| h.eq_ignore_ascii_case(&f.sha256))
                .unwrap_or(false)
        {
            continue; // already have a verified copy
        }
        let url = format!("{}/{}", manifest.base_url.trim_end_matches('/'), f.name);
        crate::telemetry::log_message(
            "model_download",
            &format!("fetching {url} -> {}", dest.display()),
        );
        download_and_verify(&url, &dest, &f.sha256)?;
    }
    Ok(())
}

/// Download `url` to `dest` (atomic temp → rename) and verify its sha256.
pub fn download_and_verify(url: &str, dest: &Path, expected_sha256: &str) -> Result<(), String> {
    let tmp = dest.with_extension("part");
    // Global timeout so a stalled mirror can't hang the fetch forever (this runs
    // on the engine-init thread when a model is missing).
    let client = crate::resumable_download::download_client(Duration::from_secs(900));
    let request = rightkit_http::Request::get(url).timeout(Duration::from_secs(900));
    let resp = client
        .client_for(url)
        .stream(&request)
        .map_err(|e| format!("GET {url}: {e}"))?;
    let mut reader = resp.into_reader();
    let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string())?;
    file.sync_all().ok();
    drop(file);
    let got = sha256_file(&tmp)?;
    if !got.eq_ignore_ascii_case(expected_sha256) {
        let _ = fs::remove_file(&tmp);
        return Err(format!(
            "sha256 mismatch for {url}: got {got}, expected {expected_sha256}"
        ));
    }
    rightkit_fs::publish_file(&tmp, dest).map_err(|e| format!("install {}: {e}", dest.display()))
}

pub use rightkit_models::hash::sha256_file;
