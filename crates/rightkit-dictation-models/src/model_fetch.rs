//! Phase D — model resolution + download-on-demand + integrity verify.
//!
//! A model that is already present is a presence check that no-ops. When it is
//! ABSENT and a `<subdir>.manifest.json` is shipped beside the models, this
//! downloads each required file to the models dir and sha256-verifies it
//! (atomic temp → rename). The manifest's `base_url` is used as shipped.
//!
//! No manifest ships today, so the download branch stays dormant and an absent
//! model is an error.
//!
//! DORMANT — not wired to any live command path; live model downloads are
//! `whisper_model.rs` / engine-side. Kept for the unified-fetcher future.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Largest manifest file read.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
/// Most files one manifest may list.
pub const MAX_MANIFEST_FILES: usize = 256;
/// Largest single file a manifest may name, in bytes (4 GiB).
pub const MAX_MANIFEST_FILE_BYTES: u64 = 4 << 30;
/// Most bytes one manifest may download in all (16 GiB).
pub const MAX_MANIFEST_TOTAL_BYTES: u64 = 16 << 30;

/// Always-named files that must be present in a usable HrTransducer model dir.
/// (Encoder / decoder_joint filenames vary by quantization — checked separately
/// by `has_model_weights`; HrTransducer::load picks the concrete files.)
pub const REQUIRED_FILES: &[&str] = &["config.json", "vocab.txt"];

/// Resolve the models base dir — app-data ONLY, the single model location on
/// every machine. There is deliberately no dev/staged-model or `RIGHTKIT_MODELS_DIR`
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
    // The model name and every required file name are single path components before they are
    // joined onto the models directory.
    reject_path_traversal(subdir)?;
    for name in required {
        reject_path_traversal(name)?;
    }
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

/// Load `<base>/<subdir>.manifest.json` if present.
pub fn load_manifest(base: &Path, subdir: &str) -> Result<Option<ModelManifest>, String> {
    reject_path_traversal(subdir)?;
    let p = base.join(format!("{subdir}.manifest.json"));
    // One capped read through one handle: not a metadata check and a reopen.
    let bytes = match crate::confined_remove::read_capped_at(&p, MAX_MANIFEST_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(reason) => return Err(format!("model manifest could not be read: {reason}")),
    };
    let m: ModelManifest =
        serde_json::from_slice(&bytes).map_err(|_| "model manifest is malformed".to_string())?;
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
    // The manifest comes from a file beside the models: bound what it can ask for.
    if manifest.files.is_empty() || manifest.files.len() > MAX_MANIFEST_FILES {
        return Err("model manifest lists no files or too many".to_string());
    }
    if !manifest
        .base_url
        .trim()
        .to_ascii_lowercase()
        .starts_with("https://")
    {
        return Err("model manifest base url must be https".to_string());
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // Each file is capped at `MAX_MANIFEST_FILE_BYTES`; the bytes still allowed in all shrink
    // with every file, so the manifest as a whole cannot exceed `MAX_MANIFEST_TOTAL_BYTES`.
    let mut budget = MAX_MANIFEST_TOTAL_BYTES;
    for f in &manifest.files {
        reject_path_traversal(&f.name)?;
        let dest = dir.join(&f.name);
        if existing_file_matches(&dest, &f.sha256, MAX_MANIFEST_FILE_BYTES) {
            continue; // already have a verified copy
        }
        let url = format!("{}/{}", manifest.base_url.trim_end_matches('/'), f.name);
        crate::telemetry::log_message("model_download", "fetching a manifest file");
        if budget == 0 {
            return Err("model manifest exceeds its total size limit".to_string());
        }
        let received =
            download_and_verify(&url, &dest, &f.sha256, MAX_MANIFEST_FILE_BYTES.min(budget))?;
        budget = budget.saturating_sub(received);
    }
    Ok(())
}

/// True when `path` is a regular file of at most `max_bytes` bytes whose SHA-256 is `expected`.
/// One handle serves the length and the hash (no link followed, nothing reopened) and no more
/// than `max_bytes + 1` bytes are read.
fn existing_file_matches(path: &Path, expected: &str, max_bytes: u64) -> bool {
    let Ok(Some(file)) = crate::confined_remove::open_file_at(path) else {
        return false;
    };
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.len() <= max_bytes)
    {
        return false;
    }
    rightkit_fs::digest::sha256_reader(&mut (&file).take(max_bytes.saturating_add(1)))
        .is_ok_and(|got| got.eq_ignore_ascii_case(expected))
}

/// Download `url` to `dest` (through a temp file, then a rename) and verify its sha256. At most
/// `max_bytes` are written (`1..=`[`MAX_MANIFEST_FILE_BYTES`]): the body is read through a limit
/// one byte past it, and a longer body fails and removes the temp file. Returns the bytes
/// received.
pub fn download_and_verify(
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    max_bytes: u64,
) -> Result<u64, String> {
    if max_bytes == 0 || max_bytes > MAX_MANIFEST_FILE_BYTES {
        return Err("download size limit is out of range".to_string());
    }
    let tmp = dest.with_extension("part");
    // Global timeout so a stalled mirror can't hang the fetch forever (this runs
    // on the engine-init thread when a model is missing).
    let client = crate::resumable_download::download_client(Duration::from_secs(900));
    let request = rightkit_http::Request::get(url).timeout(Duration::from_secs(900));
    let resp = client
        .client_for(url)
        .stream(&request)
        .map_err(|e| format!("GET {url}: {e}"))?;
    // A stale temp (or a link planted at its name) is removed as itself, then the file is created
    // exclusively: `create_new` never writes through an existing name.
    let _ = crate::confined_remove::remove_file_at(&tmp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|_| "could not create the temporary download file".to_string())?;
    let outcome = copy_hashing(&mut resp.into_reader(), &mut file, max_bytes);
    let synced = file.sync_all();
    drop(file);
    let (received, got) = match outcome {
        Ok(done) if synced.is_ok() => done,
        Ok(_) => {
            let _ = crate::confined_remove::remove_file_at(&tmp);
            return Err("could not flush the downloaded file".to_string());
        }
        Err(message) => {
            let _ = crate::confined_remove::remove_file_at(&tmp);
            return Err(message);
        }
    };
    if !got.eq_ignore_ascii_case(expected_sha256) {
        let _ = crate::confined_remove::remove_file_at(&tmp);
        return Err("sha256 mismatch for a downloaded model file".to_string());
    }
    rightkit_fs::publish_file(&tmp, dest)
        .map_err(|_| "could not install the downloaded model file".to_string())?;
    Ok(received)
}

/// Copy `reader` to `file` through a limit one byte past `max_bytes`, hashing what is written,
/// and return the byte count and the SHA-256 (lowercase hex). A body longer than `max_bytes`
/// fails; nothing past `max_bytes` is written.
fn copy_hashing(
    reader: &mut dyn Read,
    file: &mut fs::File,
    max_bytes: u64,
) -> Result<(u64, String), String> {
    let mut limited = reader.take(max_bytes.saturating_add(1));
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut received = 0u64;
    loop {
        let read = limited
            .read(&mut buffer)
            .map_err(|_| "the model download was interrupted".to_string())?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        received = received.saturating_add(read as u64);
        if received > max_bytes {
            return Err("the model file is larger than its limit".to_string());
        }
        hasher.update(chunk);
        file.write_all(chunk)
            .map_err(|_| "could not write the downloaded file".to_string())?;
    }
    Ok((received, hasher.finalize()))
}

pub use rightkit_models::hash::sha256_file;
