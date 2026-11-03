//! Whisper multilingual model — Pro, downloaded on demand (not bundled).
//!
//! Parakeet (free) auto-detects 25 European languages but has NO language input,
//! so locking to a specific language requires Whisper. Whisper is NOT bundled in
//! the installer; a Pro user downloads it once from Cloudflare R2 the first time
//! they pick a locked language.
//!
//! Two DIFFERENT archive formats land in app-data, per platform:
//! - macOS: CoreML `.mlmodelc` bundle → `<app-data>/models/whisper-multi`,
//!   where the engine's `whisper_coreml_dir` looks for it. Needs the one-time
//!   ANE compile ("prepare") step below.
//! - Windows: a whisper.cpp GGUF file → `<app-data>/models/whisper-win`, where
//!   `rightkit-dictation-engine`'s `whisper_win::resolve_whisper_model` looks for it.
//!   No prepare step (Vulkan CLI loads the GGUF directly on first inference).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

/// In-process transaction gate for model lifecycle operations.
///
/// Download commands run on independent blocking workers. A per-model gate is
/// required because resume, checksum verification, archive replacement, and
/// activation all share on-disk state. The lock entries intentionally remain
/// allocated for process lifetime; model names are a small, bounded set.
static MODEL_OPERATION_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

/// Return the serialization gate for one model's download/install transaction.
pub fn model_operation_lock(model: &str) -> Arc<Mutex<()>> {
    let locks = MODEL_OPERATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        locks
            .entry(model.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

/// Build a resumable archive path scoped to exact model contents.
///
/// A single fixed `.part` path can contain bytes for a different revision
/// after a route/checksum change. Including the verified artifact digest keeps
/// resumptions isolated while retaining restart-resume behavior.
pub fn partial_archive_path(
    models_root: &Path,
    model_name: &str,
    expected_sha256: &str,
) -> PathBuf {
    models_root.join(format!(
        ".{model_name}.{}.tar.gz.part",
        expected_sha256.trim().to_ascii_lowercase()
    ))
}

pub trait WhisperHost: Send + Sync {
    fn resolve_engine_bin(&self) -> Option<PathBuf>;
    fn has_pro(&self) -> bool;
    fn signed_entitlement_token(&self) -> Option<String>;
    fn emit_progress(&self, phase: &str, received: u64, total: u64, message: &str);
}

/// Gated Cloudflare Worker URL for the Whisper archive (Pro users only). The
/// Worker (`cloudflare-workers/model-serve/`) authorizes the download by
/// VERIFYING the caller's server-signed Ed25519 entitlement token — the same
/// `signed_token` the Rust Pro gate checks (`license::has_pro`). No shared
/// secret ships in the binary, so it can't be extracted to bypass the paywall.
#[cfg(target_os = "macos")]
const WHISPER_ARCHIVE_URL: &str =
    "https://heardright-models.adrdsouza.workers.dev/whisper-multi.tar.gz";
#[cfg(target_os = "macos")]
const WHISPER_ARCHIVE_SHA256: &str =
    "046ce2aea772e2b102a31bfb52143b58b8ab97637dadc661d4723e4fd5a7f1d7";

/// Windows whisper.cpp GGML archive — the decided ship model
/// `whisper_turbo_q5_k.bin` (large-v3-turbo, q5_k) from
/// `model_registry/final/asr/`, packaged as a single top-level `whisper-win/`
/// dir. Uploaded to the private R2 bucket `rightapps-updates` at
/// `heardright/whisper-win.tar.gz` (2026-07-03) and served Pro-gated through the
/// same Worker as the macOS CoreML archive.
#[cfg(not(target_os = "macos"))]
const WHISPER_ARCHIVE_URL: &str =
    "https://heardright-models.adrdsouza.workers.dev/whisper-win.tar.gz";
#[cfg(not(target_os = "macos"))]
const WHISPER_ARCHIVE_SHA256: &str =
    "e4fa834a3b6000ca51b71ac42799277b33496e2f86c43e95b3da57b170f76f9f";

fn url_configured() -> bool {
    !WHISPER_ARCHIVE_URL.contains("CHANGE-ME")
}

fn models_dir() -> PathBuf {
    crate::app_paths::app_data_file("models")
}

#[cfg(target_os = "macos")]
fn whisper_dir_name() -> &'static str {
    "whisper-multi"
}

#[cfg(not(target_os = "macos"))]
fn whisper_dir_name() -> &'static str {
    "whisper-win"
}

fn whisper_dir() -> PathBuf {
    models_dir().join(whisper_dir_name())
}

/// Self-heals a process death that landed between
/// `archive_install::install_single_top_level_tar_gz`'s two renames (the
/// previously-installed Whisper archive moved aside, but the replacement
/// never got moved into `whisper_dir()`) by promoting the orphaned aside
/// back before `installed()` trusts what's on disk. See
/// `archive_install::recover_orphaned_previous_install` and
/// `model_paths::local_asr_ready` for the same pattern applied to the
/// primary ASR models.
fn recover_orphaned_install() {
    match crate::archive_install::recover_orphaned_previous_install(
        &models_dir(),
        whisper_dir_name(),
    ) {
        Ok(true) => crate::telemetry::log_message(
            "model_install_recovered",
            &format!(
                "promoted orphaned previous Whisper install back to {} after an interrupted replacement",
                whisper_dir().display()
            ),
        ),
        Ok(false) => {}
        Err(err) => crate::telemetry::log_message(
            "model_install_recovery_failed",
            &format!("failed to recover orphaned previous Whisper install: {err}"),
        ),
    }
}

#[cfg(target_os = "macos")]
fn installed() -> bool {
    recover_orphaned_install();
    let d = whisper_dir();
    d.join("AudioEncoder.mlmodelc").exists() && d.join("TextDecoder.mlmodelc").exists()
}

/// Windows: "installed" means at least one `.gguf` file landed in the whisper
/// dir (the archive contains exactly one; `whisper_win::resolve_whisper_model`
/// picks it up by extension, not a fixed name, so this mirrors that lookup).
#[cfg(not(target_os = "macos"))]
fn installed() -> bool {
    recover_orphaned_install();
    let d = whisper_dir();
    let Ok(entries) = fs::read_dir(&d) else {
        return false;
    };
    entries
        .filter_map(|e| e.ok())
        .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("gguf"))
}

/// Marker written after the one-time ANE compile (the ~3-6 min "Preparing"
/// step) succeeds. The model is only fully USABLE once this exists — until then
/// loading it would pay the cold compile and block the engine.
fn prepared_marker() -> PathBuf {
    whisper_dir().join(".ane-prepared")
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct PreparedMarker {
    version: u32,
    os_build: String,
    engine_bin: String,
    engine_len: u64,
    engine_modified_s: u64,
    model_fingerprint: String,
}

#[derive(Clone, serde::Serialize)]
pub struct WhisperModelStatus {
    installed: bool,
    prepared: bool,
}

fn prepared_marker_now(host: &dyn WhisperHost) -> Result<PreparedMarker, String> {
    let bin = host
        .resolve_engine_bin()
        .ok_or_else(|| "engine binary not found for model prepare".to_string())?;
    let meta = fs::metadata(&bin).map_err(|e| format!("stat {}: {e}", bin.display()))?;
    Ok(PreparedMarker {
        version: 1,
        os_build: os_build(),
        engine_bin: bin.display().to_string(),
        engine_len: meta.len(),
        engine_modified_s: modified_s(&meta),
        model_fingerprint: model_fingerprint()?,
    })
}

fn prepared(host: &dyn WhisperHost) -> bool {
    let Ok(raw) = fs::read_to_string(prepared_marker()) else {
        return false;
    };
    let Ok(saved) = serde_json::from_str::<PreparedMarker>(&raw) else {
        return false;
    };
    let Ok(now) = prepared_marker_now(host) else {
        return false;
    };
    marker_matches(&saved, &now)
}

fn marker_matches(saved: &PreparedMarker, now: &PreparedMarker) -> bool {
    saved.version == now.version
        && saved.os_build == now.os_build
        && saved.engine_bin == now.engine_bin
        && saved.engine_len == now.engine_len
        && saved.engine_modified_s == now.engine_modified_s
        && saved.model_fingerprint == now.model_fingerprint
}

#[cfg(target_os = "macos")]
fn model_fingerprint() -> Result<String, String> {
    let d = whisper_dir();
    let mut parts = Vec::new();
    for rel in [
        "AudioEncoder.mlmodelc/model.mil",
        "AudioEncoder.mlmodelc/coremldata.bin",
        "AudioEncoder.mlmodelc/weights/weight.bin",
        "TextDecoder.mlmodelc/model.mil",
        "TextDecoder.mlmodelc/coremldata.bin",
        "TextDecoder.mlmodelc/weights/weight.bin",
        "tokenizer.json",
        "config.json",
        "generation_config.json",
    ] {
        let path = d.join(rel);
        let meta = fs::metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        parts.push(format!("{rel}:{}:{}", meta.len(), modified_s(&meta)));
    }
    Ok(rightkit_fs::digest::sha256_bytes(
        parts.join("|").as_bytes(),
    ))
}

/// Windows has no ANE-compile step, so `prepared_marker_now`/`marker_matches`
/// are dead weight there (`prepare_whisper` below just writes the marker
/// unconditionally) — but `prepared_marker_now()` still calls this to build
/// the struct, so it needs a cheap real implementation: fingerprint the one
/// `.gguf` file's size + mtime instead of a fixed CoreML file list.
#[cfg(not(target_os = "macos"))]
fn model_fingerprint() -> Result<String, String> {
    let d = whisper_dir();
    let entries = fs::read_dir(&d).map_err(|e| format!("read {}: {e}", d.display()))?;
    let gguf = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|s| s.to_str()) == Some("gguf"))
        .ok_or_else(|| format!("no .gguf file in {}", d.display()))?;
    let meta = fs::metadata(&gguf).map_err(|e| format!("stat {}: {e}", gguf.display()))?;
    let part = format!(
        "{}:{}:{}",
        gguf.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        meta.len(),
        modified_s(&meta)
    );
    Ok(rightkit_fs::digest::sha256_bytes(part.as_bytes()))
}

fn modified_s(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn os_build() -> String {
    crate::system_identity::current().os_build
}

#[cfg(not(target_os = "macos"))]
fn os_build() -> String {
    "non-macos".to_string()
}

/// Run the engine's `prepare-whisper` subcommand to trigger + cache the ANE
/// compile, with a "preparing" progress phase. macOS only (ANE).
#[cfg(target_os = "macos")]
fn prepare_whisper(host: &dyn WhisperHost) -> Result<(), String> {
    emit(
        host,
        "preparing",
        0,
        0,
        "Preparing model… one-time, ~3-6 min.",
    );
    let bin = host
        .resolve_engine_bin()
        .ok_or_else(|| "engine binary not found for model prepare".to_string())?;
    let mut owned = rightkit_child_process::OwnedCommand::new(&bin);
    // The engine derives the Whisper directory itself (`<models base>/whisper-multi`); a
    // directory argument would be an argv-selected model path.
    owned.command_mut().arg("prepare-whisper");
    let status = owned
        .spawn()
        .and_then(|mut child| child.wait())
        .map_err(|e| format!("prepare (ANE compile) spawn: {e}"))?;
    if !status.success() {
        let m = format!("model preparation failed ({status})");
        emit(host, "error", 0, 0, &m);
        return Err(m);
    }
    let marker = prepared_marker_now(host)?;
    rightkit_fs::atomic_write(
        &prepared_marker(),
        &serde_json::to_vec_pretty(&marker).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write prepared marker: {e}"))?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn prepare_whisper(host: &dyn WhisperHost) -> Result<(), String> {
    let marker = prepared_marker_now(host)?;
    rightkit_fs::atomic_write(
        &prepared_marker(),
        &serde_json::to_vec_pretty(&marker).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write prepared marker: {e}"))?;
    Ok(())
}

fn emit(host: &dyn WhisperHost, phase: &str, received: u64, total: u64, message: &str) {
    host.emit_progress(phase, received, total, message);
}

/// True only if the Whisper model is fully USABLE — downloaded AND the one-time
/// ANE compile has completed. The UI gates language selection on this so a user
/// can't pick Whisper before it's ready to load instantly.
pub fn whisper_model_installed(host: &dyn WhisperHost) -> bool {
    installed() && prepared(host)
}

pub fn whisper_model_status(host: &dyn WhisperHost) -> WhisperModelStatus {
    let has_files = installed();
    WhisperModelStatus {
        installed: has_files,
        prepared: has_files && prepared(host),
    }
}

/// Download + install the Whisper model from R2 (Pro). Idempotent: a no-op if
/// already present. Emits `whisper-model` progress events throughout.
pub fn download_whisper_model_blocking(host: &dyn WhisperHost) -> Result<(), String> {
    if !host.has_pro() {
        return Err("Locked-language transcription is a Pro feature.".into());
    }
    let operation_lock = model_operation_lock(whisper_dir_name());
    let _operation_guard = operation_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if installed() && prepared(host) {
        emit(host, "installed", 1, 1, "Already installed.");
        return Ok(());
    }
    // Files already on disk (downloaded earlier) but the ANE cache was never
    // compiled — skip straight to the prepare step.
    if installed() {
        prepare_whisper(host)?;
        emit(host, "installed", 1, 1, "Model ready.");
        return Ok(());
    }
    if !url_configured() {
        return Err("Model download source is not configured yet.".into());
    }

    let models = models_dir();
    fs::create_dir_all(&models).map_err(|e| format!("create models dir: {e}"))?;
    let tmp = partial_archive_path(&models, whisper_dir_name(), WHISPER_ARCHIVE_SHA256);

    // --- download (streamed, with progress) ---
    let agent = crate::resumable_download::download_client(std::time::Duration::from_secs(3600));
    // Authorize with the user's server-signed Pro entitlement token; the Worker
    // verifies its Ed25519 signature + `pro` tier + expiry. No shared secret.
    let pro_token = host.signed_entitlement_token().ok_or_else(|| {
        let m =
            "Your Pro licence needs a refresh before the model can download — reactivate it in Settings."
                .to_string();
        emit(host, "error", 0, 0, &m);
        m
    })?;
    let authorization = format!("Bearer {pro_token}");
    let mut last_emit: u64 = 0;
    let download = crate::resumable_download::download_resumable(
        &agent,
        crate::resumable_download::DownloadRequest {
            url: WHISPER_ARCHIVE_URL,
            part_path: &tmp,
            expected_sha256: WHISPER_ARCHIVE_SHA256,
            authorization: Some(&authorization),
        },
        |received, total| {
            if received < last_emit {
                last_emit = 0;
            }
            if received.saturating_sub(last_emit) > 4_000_000 {
                emit(host, "downloading", received, total, "Downloading model…");
                last_emit = received;
            }
        },
    )
    .inspect_err(|m| {
        emit(host, "error", 0, 0, m);
    })?;
    let received = download.received;
    let total = download.total;

    // --- extract (tar) ---
    emit(host, "extracting", received, total, "Installing model…");
    if let Err(err) = crate::archive_install::install_single_top_level_tar_gz(
        &download.archive_path,
        &models,
        whisper_dir_name(),
    ) {
        emit(host, "error", received, total, &err);
        return Err(err);
    }
    crate::resumable_download::discard_partial(&tmp);
    if !installed() {
        let m = "Model archive did not contain the expected files.".to_string();
        emit(host, "error", received, total, &m);
        return Err(m);
    }
    // --- prepare (one-time ANE compile) — runs right after download ---
    prepare_whisper(host)?;
    emit(host, "installed", received, total, "Model ready.");
    Ok(())
}
