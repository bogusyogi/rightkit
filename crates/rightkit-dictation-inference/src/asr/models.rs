// Model resolution & context bias: backend/subdir selection, per-platform
// bundle paths and loaders, and the context-bias install/refresh helpers.
use std::path::{Path, PathBuf};

use super::runtime::AsrRuntime;

// Used by the ONNX model-dir loader, which is gated off macOS, and by the
// subdir tests, which run everywhere. Hence `not(macos) OR test`.
#[cfg(not(target_os = "macos"))]
pub(crate) const UNIFIED_MODEL_SUBDIR: &str = "unified_static15_b128_sym_bits4_timestamp_hybrid";
#[cfg(not(target_os = "macos"))]
pub(crate) const TDT_MODEL_SUBDIR: &str = "parakeet_tdt_ultra_static1500_fp16int4_20261022";
#[cfg(target_os = "macos")]
const COREML_UNIFIED_MODEL_SUBDIR: &str = "parakeet-unified-en-0.6b";
#[cfg(target_os = "macos")]
const COREML_TDT_MODEL_SUBDIR: &str = "parakeet-tdt-ultra";

#[cfg(target_os = "macos")]
pub(crate) fn try_load_coreml_parakeet(models_dir: &Path) -> Result<Option<AsrRuntime>, String> {
    let backend = configured_backend();
    if !matches!(
        backend.as_str(),
        "parakeet-unified" | "parakeet" | "parakeet-rnnt" | "rnnt" | "parakeet-tdt"
    ) {
        return Ok(None);
    }

    let dir = coreml_parakeet_bundle_dir(models_dir);
    if !dir.join("pipeline.json").exists() {
        tracing::warn!("CoreML Parakeet bundle unavailable at {}", dir.display());
        return Ok(None);
    }

    let mut model = crate::coreml_asr::CoreMlParakeet::load(&dir)
        .map_err(|e| format!("coreml parakeet load ({}): {e}", dir.display()))?;
    apply_coreml_context_bias(&mut model);
    tracing::info!("CoreML Parakeet model loaded from {}", dir.display());
    Ok(Some(AsrRuntime::CoreMlParakeet(model)))
}

#[cfg(target_os = "macos")]
pub(crate) fn try_load_coreml_whisper(models_dir: &Path, lang: &str) -> Result<AsrRuntime, String> {
    let role = "whisper-multi";
    let dir = whisper_coreml_dir(models_dir);
    if !dir.join("AudioEncoder.mlmodelc").exists() || !dir.join("TextDecoder.mlmodelc").exists() {
        return Err(format!(
            "native whisper bundle missing role={role} dir={}",
            dir.display()
        ));
    }
    let engine = crate::whisper_coreml::WhisperCoreMl::load(&dir)?;
    let lang_tok = engine.lang_token(lang);
    tracing::info!(
        "Whisper CoreML model loaded role={} lang={} dir={}",
        role,
        lang,
        dir.display()
    );
    Ok(AsrRuntime::WhisperCoreMl { engine, lang_tok })
}

#[cfg(target_os = "windows")]
pub(crate) fn try_load_whisper_win(models_dir: &Path, lang: String) -> Result<AsrRuntime, String> {
    let engine = crate::whisper_win::WhisperWin::load(models_dir)?;
    tracing::info!("Windows Whisper model loaded lang={}", lang);
    Ok(AsrRuntime::WhisperWin { engine, lang })
}

#[cfg(target_os = "macos")]
fn coreml_parakeet_bundle_dir(models_dir: &Path) -> PathBuf {
    if let Some(dir) = crate::engine_paths::launch_model_choice().coreml_model_dir {
        return dir;
    }
    let backend = configured_backend();
    if backend == "parakeet-tdt" {
        models_dir.join(COREML_TDT_MODEL_SUBDIR)
    } else {
        models_dir.join(COREML_UNIFIED_MODEL_SUBDIR)
    }
}

#[cfg(target_os = "macos")]
fn whisper_coreml_dir(models_dir: &Path) -> PathBuf {
    // Whisper is NOT bundled — it's a Pro on-demand download to app-data. The
    // The engine-startup resolved fallback is a *bundled-layout* path that doesn't
    // exist for this case, so the on-disk downloaded copy MUST win first. Only if
    // it's absent do we use the resolved fallback (test-mode builds may override
    // it), then the models-base fallback.
    let downloaded = crate::settings::app_data_root()
        .join("models")
        .join("whisper-multi");
    if downloaded.join("AudioEncoder.mlmodelc").exists() {
        return downloaded;
    }
    if let Some(dir) = crate::engine_paths::whisper_coreml_dir() {
        return dir;
    }
    models_dir.join("coreml").join("whisper-multi")
}

pub(crate) fn configured_backend() -> String {
    crate::engine_paths::launch_model_choice()
        .backend
        .or_else(|| Some(crate::settings::asr_backend()))
        .unwrap_or_else(|| rightkit_dictation_core::settings::DEFAULT_ASR_BACKEND.to_string())
        .trim()
        .to_ascii_lowercase()
}

// Only the ONNX model-dir loader calls this, and that is gated off macOS. Its
// `_alias` sibling below is additionally exercised by tests on every platform.
#[cfg(not(target_os = "macos"))]
pub(crate) fn model_subdir_for(alias: &str) -> String {
    if let Some(subdir) = crate::engine_paths::launch_model_choice().asr_model_subdir {
        return subdir;
    }
    model_subdir_for_alias(alias).to_string()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn model_subdir_for_alias(alias: &str) -> &'static str {
    match alias.trim().to_ascii_lowercase().as_str() {
        "parakeet-unified" | "parakeet" | "parakeet-rnnt" | "rnnt" => UNIFIED_MODEL_SUBDIR,
        "parakeet-tdt" => TDT_MODEL_SUBDIR,
        _ => UNIFIED_MODEL_SUBDIR,
    }
}

/// Re-install decode-time bias for ONE utterance, folding in the current
/// screen-context harvest (screen_vocab). Called from the worker right before
/// each final decode. This is intentionally unconditional: command probes use
/// a stronger score on the same runtime, so hash-only caching could leave probe
/// bias installed for final dictation. The slot is still re-installed every
/// call; only the token-id table BUILD is memoised, inside
/// `ContextBias::from_phrases` / `set_context_bias_phrases`, keyed by the full
/// (vocabulary, terms, score) content (perf audit ENG-02).
pub(crate) const DEFAULT_CONTEXT_BIAS_SCORE: f32 = 1.0;
pub(crate) const PROBE_CONTEXT_BIAS_SCORE: f32 = 5.0;

pub fn apply_utterance_bias(model: &mut AsrRuntime) {
    let Some(score) = context_bias_score() else {
        return;
    };
    let screen = crate::screen_vocab::current_terms();
    let mut terms = context_bias_terms();
    // Base terms outrank screen terms at the 512 ceiling; dedupe casefolded.
    let existing: std::collections::HashSet<String> =
        terms.iter().map(|t| t.to_lowercase()).collect();
    terms.extend(
        screen
            .into_iter()
            .filter(|t| !existing.contains(&t.to_lowercase())),
    );
    terms.truncate(512);
    let requested = terms.len();
    let installed = match model {
        #[cfg(not(target_os = "macos"))]
        AsrRuntime::Parakeet(m, slot) => {
            // Bias now lives first-party next to the model, because the ONNX
            // beam is first-party and needs it per hypothesis. The vendor is no
            // longer told about bias at all.
            let bias = rightkit_asr::parakeet_core::bias::ContextBias::from_phrases(
                m.vocab_pieces(),
                terms.iter(),
                score,
            );
            let installed = bias.len();
            *slot = (!bias.is_empty()).then_some(bias);
            installed
        }
        #[cfg(target_os = "macos")]
        AsrRuntime::CoreMlParakeet(m) => m.set_context_bias_phrases(terms.iter(), score),
        // Whisper lanes have no phrase-bias mechanism.
        #[allow(unreachable_patterns)]
        _ => return,
    };
    log_bias_line(
        &LAST_UTTERANCE_BIAS_LINE,
        "utterance bias refreshed",
        requested,
        installed,
        score,
    );
}

static LAST_UTTERANCE_BIAS_LINE: std::sync::Mutex<Option<(usize, usize, u32)>> =
    std::sync::Mutex::new(None);
static LAST_PROBE_BIAS_LINE: std::sync::Mutex<Option<(usize, usize, u32)>> =
    std::sync::Mutex::new(None);

/// Per-decode bias lines (perf audit ENG-17): these ran at info before EVERY
/// decode (final, probe, keep-warm, Static-15, alignment) and crossed stderr,
/// shell redaction and the telemetry queue. The text is unchanged; it goes out
/// at info only when it differs from the previous line of the same kind, and at
/// debug otherwise. No test, script or telemetry consumer reads these lines.
fn log_bias_line(
    last: &std::sync::Mutex<Option<(usize, usize, u32)>>,
    what: &str,
    requested: usize,
    installed: usize,
    score: f32,
) {
    let key = (requested, installed, score.to_bits());
    let changed = match last.lock() {
        Ok(mut last) => last.replace(key) != Some(key),
        Err(_) => true,
    };
    if changed {
        tracing::info!(
            "{what}: requested={} installed={} score={:.2}",
            requested,
            installed,
            score
        );
    } else {
        tracing::debug!(
            "{what}: requested={} installed={} score={:.2}",
            requested,
            installed,
            score
        );
    }
}

/// Give command/trigger probes maximum vocabulary bias. Final work restores its
/// default score through `apply_utterance_bias` before decoding.
pub fn apply_probe_context_bias(model: &mut AsrRuntime) {
    if context_bias_score().is_none() {
        return;
    }
    let terms = context_bias_terms();
    let requested = terms.len();
    let installed = match model {
        #[cfg(not(target_os = "macos"))]
        AsrRuntime::Parakeet(m, slot) => {
            let bias = rightkit_asr::parakeet_core::bias::ContextBias::from_phrases(
                m.vocab_pieces(),
                terms.iter(),
                PROBE_CONTEXT_BIAS_SCORE,
            );
            let installed = bias.len();
            *slot = (!bias.is_empty()).then_some(bias);
            installed
        }
        #[cfg(target_os = "macos")]
        AsrRuntime::CoreMlParakeet(m) => {
            m.set_context_bias_phrases(terms.iter(), PROBE_CONTEXT_BIAS_SCORE)
        }
        #[allow(unreachable_patterns)]
        _ => return,
    };
    log_bias_line(
        &LAST_PROBE_BIAS_LINE,
        "ASR probe context bias configured",
        requested,
        installed,
        PROBE_CONTEXT_BIAS_SCORE,
    );
}

/// Load-time bias for the ONNX runtime. Returns the bias to store alongside the
/// model instead of mutating it: the vendor kept bias inside the decoder, but
/// the first-party beam applies it per hypothesis, so it lives next to the model
/// in `AsrRuntime::Parakeet`. `apply_utterance_bias` refreshes the same slot.
#[cfg(not(target_os = "macos"))]
pub(crate) fn initial_context_bias(
    model: &rightkit_asr::onnx_tdt::OnnxAsr,
) -> Option<rightkit_asr::parakeet_core::bias::ContextBias> {
    let score = context_bias_score()?;
    let terms = context_bias_terms();
    let requested = terms.len();
    let bias = rightkit_asr::parakeet_core::bias::ContextBias::from_phrases(
        model.vocab_pieces(),
        terms.iter(),
        score,
    );
    let installed = bias.len();
    tracing::info!(
        "ASR context bias configured: requested={} installed={} score={:.2}",
        requested,
        installed,
        score
    );
    (!bias.is_empty()).then_some(bias)
}

#[cfg(target_os = "macos")]
fn apply_coreml_context_bias(model: &mut crate::coreml_asr::CoreMlParakeet) {
    let Some(score) = context_bias_score() else {
        return;
    };
    let terms = context_bias_terms();
    let requested = terms.len();
    let installed = model.set_context_bias_phrases(terms.iter(), score);
    tracing::info!(
        "ASR CoreML context bias configured: requested={} installed={} score={:.2}",
        requested,
        installed,
        score
    );
}

fn context_bias_score() -> Option<f32> {
    if matches!(
        rightkit_dictation_core::dev_var!("RIGHTKIT_DISABLE_ASR_CONTEXT_BIAS")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("on")
    ) {
        return None;
    }
    let score = rightkit_dictation_core::dev_var!("RIGHTKIT_ASR_CONTEXT_BIAS_SCORE")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(DEFAULT_CONTEXT_BIAS_SCORE);
    score
        .is_finite()
        .then_some(score)
        .filter(|s| *s > 0.0 && *s <= 5.0)
}

pub(crate) fn context_bias_terms() -> Vec<String> {
    // Intentional product vocabulary: bias the decoder toward the the app
    // brand instead of homophones such as "herd right" when users name the app.
    let mut terms: Vec<String> = crate::vocabulary::built_in_terms()
        .iter()
        .map(|s| s.to_string())
        .collect();
    terms.extend(crate::vocabulary::terms());
    let max_terms = rightkit_dictation_core::dev_var!("RIGHTKIT_ASR_CONTEXT_BIAS_MAX_TERMS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(512);
    terms.truncate(max_terms);
    terms
}

#[cfg(target_os = "windows")]
pub(crate) fn probe_directml_embedded() -> bool {
    // The vendored crate registers DirectML with a silent CPU fallback; a
    // real probe requires an ORT session. We use the same lightweight
    // heuristic the sidecar uses: trust the env override or default
    // to DML on Windows and let model load fail loudly if the device is absent.
    !matches!(
        rightkit_dictation_core::dev_var!("RIGHTKIT_ASR_EP")
            .ok()
            .as_deref(),
        Some("cpu") | Some("CPU")
    )
}

/// Resolve the models base directory. Models live in EXACTLY ONE place — the
/// app-data models dir — on every machine, dev or installed. There is no dev /
/// sibling-of-exe / cwd lookup. Precedence:
///  1. A test-mode override (compiled out of release builds).
///  2. The optional `hint` (a positional arg; the engine entry passes one only in
///     test-mode builds, so a release engine never takes its models path from argv).
///  3. The root the engine resolved at startup (`engine_paths::install`), which is
///     the same `<app-data>/models` the shell uses.
///  4. `<app-data>/models` — the same location, for callers that never installed.
pub fn models_base(hint: Option<&Path>) -> PathBuf {
    if let Some(dir) = dev_models_base() {
        return dir;
    }
    if let Some(hint) = hint {
        return hint.to_path_buf();
    }
    crate::engine_paths::installed()
        .map(|paths| paths.models_base.clone())
        .unwrap_or_else(|| crate::settings::app_data_root().join("models"))
}

fn dev_models_base() -> Option<PathBuf> {
    rightkit_dictation_core::dev_var_os!("RIGHTKIT_MODELS_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
