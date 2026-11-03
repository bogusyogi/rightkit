// Canonical ASR model-location resolution — the ONE source of truth.
//
// Activated Parakeet resolves to its exact Orthic Labs shared-store variant,
// then the app-private legacy directory. Bundled KWS is separate.

use std::path::{Path, PathBuf};

// Windows TDT rotation. CURRENT is Parakeet Ultra (2026-10-22). Retired ids,
// newest first: PREVIOUS (static1500/QInt8, 2026-07-22), OLDER (dynamic fp16
// storage, 2026-07-20), OLDEST (b64 bits4 protect). Same scheme as the
// 2026-07-22 switch: the activating marker is transaction-local, and only the
// activated marker written after a successful native load makes CURRENT win
// over a retired model that is still on disk.
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_OLDEST_SUBDIR: &str = "parakeet_tdt_v3_b64_sym_bits4_protect";
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_OLDER_SUBDIR: &str = "parakeet_tdt_v3_dynamic_b64_fp16storage_20260720";
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_PREVIOUS_SUBDIR: &str = "parakeet_tdt_v3_static1500_qint8_20260722";
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_CURRENT_SUBDIR: &str = crate::ultra_model::ULTRA_WINDOWS_ID;
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_ACTIVATED_MARKER: &str = ".windows-tdt-ultra-fp16int4-activated-20261022";
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_ACTIVATING_MARKER: &str = ".windows-tdt-ultra-fp16int4-activating-20261022";
// Markers of the retired revisions; removed once the retired directories are.
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_PREVIOUS_ACTIVATED_MARKER: &str =
    ".windows-tdt-static1500-qint8-activated-20260722";
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_PREVIOUS_ACTIVATING_MARKER: &str =
    ".windows-tdt-static1500-qint8-activating-20260722";
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_OLDER_ACTIVATED_MARKER: &str = ".windows-tdt-compact-activated-20260720";
#[cfg(not(target_os = "macos"))]
pub const WINDOWS_TDT_OLDER_ACTIVATING_MARKER: &str = ".windows-tdt-compact-activating-20260720";

// macOS TDT rotation: CURRENT is Parakeet Ultra; the one retired id is the
// CoreML TDT v3 bundle. Its install subdir differs from its shared-store id.
pub const MACOS_TDT_CURRENT_SUBDIR: &str = crate::ultra_model::ULTRA_MACOS_ID;
pub const MACOS_TDT_PREVIOUS_SUBDIR: &str = "parakeet-tdt-v3";
pub const MACOS_TDT_PREVIOUS_STORE_ID: &str = "parakeet-tdt-v3-coreml-compiled-5f8888117f47";

/// Retired TDT models on this platform, newest first, as
/// `(install subdir, shared-store id)`. Nothing in this list is ever downloaded
/// again; each is kept as a working fallback until Ultra is verified, then
/// reclaimed (`retired_model_cleanup`).
#[cfg(not(target_os = "macos"))]
pub const RETIRED_TDT_MODELS: &[(&str, &str)] = &[
    (WINDOWS_TDT_PREVIOUS_SUBDIR, WINDOWS_TDT_PREVIOUS_SUBDIR),
    (WINDOWS_TDT_OLDER_SUBDIR, WINDOWS_TDT_OLDER_SUBDIR),
    (WINDOWS_TDT_OLDEST_SUBDIR, WINDOWS_TDT_OLDEST_SUBDIR),
];
#[cfg(target_os = "macos")]
pub const RETIRED_TDT_MODELS: &[(&str, &str)] =
    &[(MACOS_TDT_PREVIOUS_SUBDIR, MACOS_TDT_PREVIOUS_STORE_ID)];

pub const KWS_SUBDIR: &str = "kws";

/// Where primary ASR models live, and the engine's fallback base.
pub fn install_base() -> PathBuf {
    crate::app_paths::app_data_file("models")
}

/// Activated shared model first (this process's own activation, else the persisted activation
/// record), then complete legacy app-data copy. For path selection and status; deletion and
/// finalizing gates use [`resolve_model_dir_verified`].
pub fn resolve_model_dir(subdir: &str) -> Option<PathBuf> {
    if let Some(dir) = crate::shared::resolve(subdir) {
        return Some(dir);
    }
    resolve_model_dir_outside_shared(subdir)
}

/// [`resolve_model_dir`] that trusts the shared copy only after a prepare verified it in this
/// process. A relaunched process has no such activation until `prepare` runs, so a gate that
/// deletes or finalizes something must not rest on the persisted record alone.
pub fn resolve_model_dir_verified(subdir: &str) -> Option<PathBuf> {
    if let Some(dir) = crate::shared::resolve_verified(subdir) {
        return Some(dir);
    }
    resolve_model_dir_outside_shared(subdir)
}

fn resolve_model_dir_outside_shared(subdir: &str) -> Option<PathBuf> {
    let dir = install_base().join(subdir);
    if local_asr_ready(&dir, subdir) {
        return Some(dir);
    }
    // Last resort: a retired model that only the shared store still holds.
    crate::shared::resolve_retired(subdir)
}

/// The base dir handed to the engine sidecar (it appends the subdir itself).
/// Supervisor also passes the resolved model basename / CoreML bundle path:
/// immutable shared IDs need not equal historical app-private directory names.
pub fn resolve_models_base(subdir: &str) -> PathBuf {
    resolve_model_dir(subdir)
        .and_then(|dir| dir.parent().map(Path::to_path_buf))
        .unwrap_or_else(install_base)
}

/// The on-disk subdir name for a backend alias, per platform model layout.
pub fn subdir_for_backend(backend: &str) -> &'static str {
    let tdt = backend.trim().eq_ignore_ascii_case("parakeet-tdt");
    #[cfg(target_os = "macos")]
    {
        if tdt {
            macos_tdt_subdir()
        } else {
            "parakeet-unified-en-0.6b"
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if tdt {
            windows_tdt_subdir()
        } else {
            "unified_static15_b128_sym_bits4_timestamp_hybrid"
        }
    }
}

/// Ultra once the shared store activated it; else the best complete app-data
/// revision; else a retired model that only the shared store holds (installs
/// made after the shared store went live have no app-data copy); else Ultra,
/// which is the download target.
#[cfg(not(target_os = "macos"))]
fn windows_tdt_subdir() -> &'static str {
    if crate::shared::resolve(WINDOWS_TDT_CURRENT_SUBDIR).is_some() {
        return WINDOWS_TDT_CURRENT_SUBDIR;
    }
    let root = install_base();
    let local = windows_tdt_subdir_for_root(&root);
    if local_asr_ready(&root.join(local), local) {
        return local;
    }
    crate::shared::retired_subdir_in_store().unwrap_or(local)
}

#[cfg(target_os = "macos")]
fn macos_tdt_subdir() -> &'static str {
    if crate::shared::resolve(MACOS_TDT_CURRENT_SUBDIR).is_some() {
        return MACOS_TDT_CURRENT_SUBDIR;
    }
    let root = install_base();
    let local = macos_tdt_subdir_for_root(&root);
    if local_asr_ready(&root.join(local), local) {
        return local;
    }
    crate::shared::retired_subdir_in_store().unwrap_or(local)
}

/// Select the verified Ultra Windows TDT revision when it is complete and
/// activated, but preserve the newest available retired model as a working
/// fallback during migration. The activating marker is transaction-local;
/// historical activation markers never authorize this revision.
#[cfg(not(target_os = "macos"))]
pub fn windows_tdt_subdir_for_root(models_root: &Path) -> &'static str {
    let current_ready = local_asr_ready(
        &models_root.join(WINDOWS_TDT_CURRENT_SUBDIR),
        WINDOWS_TDT_CURRENT_SUBDIR,
    );
    let retired = RETIRED_TDT_MODELS
        .iter()
        .map(|(subdir, _)| *subdir)
        .find(|subdir| local_asr_ready(&models_root.join(subdir), subdir));
    let selected_for_activation = models_root.join(WINDOWS_TDT_ACTIVATED_MARKER).is_file()
        || models_root.join(WINDOWS_TDT_ACTIVATING_MARKER).is_file();
    match retired {
        Some(retired) if !(current_ready && selected_for_activation) => retired,
        _ => WINDOWS_TDT_CURRENT_SUBDIR,
    }
}

/// macOS counterpart. A complete retired CoreML bundle stays selected until the
/// shared store has activated Ultra (`macos_tdt_subdir` checks that first); a
/// local Ultra directory is chosen on its own only when no retired bundle exists.
pub fn macos_tdt_subdir_for_root(models_root: &Path) -> &'static str {
    let previous = models_root.join(MACOS_TDT_PREVIOUS_SUBDIR);
    if local_asr_ready(&previous, MACOS_TDT_PREVIOUS_SUBDIR) {
        MACOS_TDT_PREVIOUS_SUBDIR
    } else {
        MACOS_TDT_CURRENT_SUBDIR
    }
}

/// True when `dir` holds the complete fileset for the model layout `subdir`.
///
/// Before checking, self-heals a process death that landed between
/// `archive_install::install_single_top_level_tar_gz_as`'s two renames (the
/// existing model moved aside, but the replacement never got moved into
/// `dir`) by promoting the orphaned aside back — see
/// `archive_install::recover_orphaned_previous_install`. This is the read
/// path every model resolution and engine-startup readiness check goes
/// through (`resolve_model_dir`, `windows_tdt_subdir_for_root`), so recovery
/// runs the next time anything asks "is this model installed?" without
/// requiring a fresh download. A no-op (cheap `exists()` check) for the
/// overwhelming majority of calls, where no aside is present. The
/// Windows-TDT versioned subdirs use a different side-by-side-plus-marker
/// scheme with its own dedicated recovery
/// (`recover_interrupted_windows_tdt_activation`) and never produce an aside
/// under this scheme, so this call is a guaranteed no-op for them.
pub fn local_asr_ready(dir: &Path, subdir: &str) -> bool {
    if let Some(dest_root) = dir.parent() {
        match crate::archive_install::recover_orphaned_previous_install(dest_root, subdir) {
            Ok(true) => crate::telemetry::log_message(
                "model_install_recovered",
                &format!(
                    "promoted orphaned previous install back to {} after an interrupted replacement",
                    dir.display()
                ),
            ),
            Ok(false) => {}
            Err(err) => crate::telemetry::log_message(
                "model_install_recovery_failed",
                &format!("failed to recover orphaned previous install at {}: {err}", dir.display()),
            ),
        }
    }
    if !dir.is_dir() {
        return false;
    }
    match subdir {
        "parakeet-unified-en-0.6b" | "parakeet-tdt-v3" | "parakeet-tdt-ultra" => {
            dir.join("pipeline.json").is_file()
                && dir.join("vocab.txt").is_file()
                && stage_exists(dir, "MelSpectrogram")
                && stage_exists(dir, "AudioEncoder")
                && stage_exists(dir, "TextDecoder")
                && stage_exists(dir, "Joint")
        }
        "unified_static15_b128_sym_bits4_timestamp_hybrid" => {
            dir.join("config.json").is_file()
                && dir.join("encoder-model.onnx").is_file()
                && dir.join("decoder_joint-model.int8.onnx").is_file()
                && dir.join("tokenizer.model").is_file()
        }
        crate::ultra_model::ULTRA_WINDOWS_ID => crate::ultra_model::ULTRA_WINDOWS_REQUIRED_FILES
            .iter()
            .all(|file| dir.join(file).is_file()),
        "parakeet_tdt_v3_b64_sym_bits4_protect"
        | "parakeet_tdt_v3_dynamic_b64_fp16storage_20260720" => {
            dir.join("config.json").is_file()
                && dir.join("encoder-model.onnx").is_file()
                && dir.join("decoder_joint-model.onnx").is_file()
                && dir.join("vocab.txt").is_file()
        }
        "parakeet_tdt_v3_static1500_qint8_20260722" => {
            dir.join("config.json").is_file()
                && dir.join("encoder-model.onnx").is_file()
                && dir.join("decoder_joint-model.onnx").is_file()
                && dir.join("mel_nemo.onnx").is_file()
                && dir.join("mel_nemo.onnx.data").is_file()
                && dir.join("vocab.txt").is_file()
        }
        _ => false,
    }
}

/// One resolution candidate, tagged so telemetry can show which kind of
/// location actually won: a shipped bundle location, an exe-relative
/// (non-bundle) location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KwsCandidateKind {
    Bundle,
    ExeRelative,
}

pub struct KwsCandidate {
    pub kind: KwsCandidateKind,
    pub path: PathBuf,
}

/// Build the ordered candidate list for KWS resolution. Executable-
/// relative / macOS bundle locations are tried FIRST because that is what
/// ships to users. No build-time source-checkout path is a candidate, so a binary built on a
/// developer machine never resolves into that developer's source tree.
pub fn kws_candidates(exe: Option<&Path>) -> Vec<KwsCandidate> {
    let mut candidates = Vec::new();
    if let Some(exe) = exe {
        if let Some(dir) = exe.parent() {
            candidates.push(KwsCandidate {
                kind: KwsCandidateKind::ExeRelative,
                path: dir.join("kws"),
            });
            candidates.push(KwsCandidate {
                kind: KwsCandidateKind::ExeRelative,
                path: dir.join("resources/kws"),
            });
            #[cfg(target_os = "macos")]
            if let Some(contents) = dir.parent() {
                candidates.push(KwsCandidate {
                    kind: KwsCandidateKind::Bundle,
                    path: contents.join("Resources/kws"),
                });
                candidates.push(KwsCandidate {
                    kind: KwsCandidateKind::Bundle,
                    path: contents.join("Resources/resources/kws"),
                });
            }
        }
    }
    candidates
}

/// Pure selection over a candidate list plus an existence/readiness
/// predicate — kept separate from `kws_dir` so ordering can be unit
/// tested without a real bundle or dev checkout on disk.
pub fn select_kws_dir(
    candidates: Vec<KwsCandidate>,
    mut ready: impl FnMut(&Path) -> bool,
) -> (PathBuf, KwsCandidateKind) {
    let fallback = candidates.first().map(|c| (c.path.clone(), c.kind));
    let selected = candidates
        .into_iter()
        .find(|c| ready(&c.path))
        .map(|c| (c.path, c.kind));
    selected
        .or(fallback)
        .unwrap_or_else(|| (PathBuf::from("resources/kws"), KwsCandidateKind::Bundle))
}

/// Resolve bundled KWS resources from installed (bundle / exe-
/// relative) layouts first, falling back to the dev source checkout only in
/// debug builds. Logs which candidate kind was selected so a mis-resolution
/// like "release binary loading from a developer's source tree" is obvious
/// in telemetry.
pub fn kws_dir() -> PathBuf {
    let exe = std::env::current_exe().ok();
    let candidates = kws_candidates(exe.as_deref());
    let (dir, kind) = select_kws_dir(candidates, kws_ready);
    crate::telemetry::log_message(
        "kws_dir_resolved",
        &format!("dir={} candidate_kind={kind:?}", dir.display()),
    );
    dir
}

/// True only when every selected chunk-16 KWS runtime artifact is present.
pub fn kws_ready(dir: &Path) -> bool {
    dir.join("encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx")
        .is_file()
        && dir
            .join("decoder-epoch-13-avg-2-chunk-16-left-64.onnx")
            .is_file()
        && dir
            .join("joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx")
            .is_file()
        && dir.join("tokens.txt").is_file()
        && dir.join("keywords.txt").is_file()
}

pub fn kws_missing() -> bool {
    !kws_ready(&kws_dir())
}

/// The bundled ffmpeg for an executable at `exe`, in the layout the installers write: beside the
/// executable under `runtime/` (Windows NSIS, macOS dev), under `resources/runtime/`, or in the
/// macOS bundle's `Contents/Resources/runtime/`. The engine resolves it from its own location, so
/// the shell passes it nothing.
pub fn bundled_ffmpeg_path_from(exe: Option<&Path>) -> Option<PathBuf> {
    let name = if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let exe_dir = exe?.parent()?;
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut candidates = vec![
        exe_dir.join("runtime").join(name),
        exe_dir.join("resources/runtime").join(name),
    ];
    #[cfg(target_os = "macos")]
    if let Some(contents) = exe_dir.parent() {
        candidates.push(contents.join("Resources").join("runtime").join(name));
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// [`bundled_ffmpeg_path_from`] for this process's executable.
pub fn bundled_ffmpeg_path() -> Option<PathBuf> {
    bundled_ffmpeg_path_from(std::env::current_exe().ok().as_deref())
}

/// Compiled CoreML model cache. It lives in app data so Apple's compiler output persists.
pub fn coreml_compiled_cache_dir() -> PathBuf {
    crate::app_paths::app_data_file("coreml-cache")
}

/// The Whisper CoreML bundle location the engine falls back to under the models base.
pub fn whisper_coreml_fallback_dir() -> PathBuf {
    install_base().join("coreml").join("whisper-multi")
}

fn stage_exists(dir: &Path, stem: &str) -> bool {
    dir.join(format!("{stem}.mlmodelc")).is_dir()
}
