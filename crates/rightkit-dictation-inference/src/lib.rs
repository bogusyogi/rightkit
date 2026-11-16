//! Model inference, speech detection, calibration, and file transcription.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::manual_clamp
)]

/// The TDT/RNN-T beam emission reward: the platform default from `rightkit-asr`.
/// Every target's ONNX and CoreML beam decoders call it.
pub fn beam_emit_reward() -> f32 {
    rightkit_asr::parakeet_core::tdt_beam::default_emit_reward()
}

static ASR_STDERR_PREFIX: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Host option: the prefix of the ASR diagnostic lines written to stderr
/// (`<prefix>WINDOW_STATS_JSON=...`, `<prefix>DECODER_CALLS ...`). Call once at
/// startup; until then the prefix is `RIGHTKIT_ASR_`. Returns false when already
/// set. The `tracing` target of the stage-timing events stays
/// `rightkit_asr_stage_timing`: a target must be a constant.
pub fn configure_asr_stderr_prefix(prefix: &'static str) -> bool {
    ASR_STDERR_PREFIX.set(prefix).is_ok()
}

/// The ASR stderr line prefix in force (see `configure_asr_stderr_prefix`).
pub fn asr_stderr_prefix() -> &'static str {
    ASR_STDERR_PREFIX.get().copied().unwrap_or("RIGHTKIT_ASR_")
}

pub mod asr;
#[cfg(target_os = "macos")]
pub mod av_decode;
// Onboarding calibration drives `asr`, which exists only on macOS and Windows.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod calibration;
// Platform-neutral transcript cleanup; `asr::runtime` names its `Alignment` on every target.
pub mod control_cleanup;
#[cfg(target_os = "macos")]
pub mod coreml;
#[cfg(target_os = "macos")]
pub mod coreml_asr;
pub mod engine_paths;
pub mod file_transcribe;
pub mod inference_gate;
#[cfg(not(target_os = "macos"))]
mod onnx_beam_decode;
// All targets: the keyword-spotting cascade runs on the Rust spotter and builds and tests everywhere.
pub mod kws;
/// Alias module so a host that names the cascade `kws_cascade::KwsCascade` links unchanged.
pub mod kws_cascade {
    pub use crate::kws::*;
    /// The keyword-spotting cascade, under the name some hosts use for `Kws`.
    pub type KwsCascade = Kws;
}
pub mod subtitle_export;
pub mod timeline_recorder;
// Platform-neutral: control_cleanup uses it on every target.
pub mod trigger_audio_cut;
pub mod vad;
pub mod wake_handoff;
#[cfg(target_os = "macos")]
pub mod whisper_coreml;
#[cfg(target_os = "windows")]
pub mod whisper_win;
#[cfg(target_os = "windows")]
pub mod whisper_win_ffi;

/// Age of the most recent ONNX encoder pass (the DirectML GPU pass on Windows),
/// for the worker to stamp at final start (ENG-01 `prior_main_pass_age_ms`).
#[cfg(not(target_os = "macos"))]
pub use rightkit_asr::onnx_tdt::encoder_pass_age as onnx_encoder_pass_age;
#[cfg(target_os = "windows")]
pub use rightkit_asr::onnx_tdt::init_windows_ort;
pub use rightkit_asr::parakeet_core::{TimedToken, TranscriptionResult};
pub use rightkit_dictation_text_pipeline::{screen_vocab, settings, vocabulary};
pub use trigger_audio_cut::{
    resolve_trigger_audio_cut, resolve_trigger_audio_cut_with_switches, strip_control_residue,
    CutResolverSwitches, TriggerAudioCut, KWS_ONSET_CUT_SOURCE,
};
pub use wake_handoff::{
    wake_handoff_audio, wake_handoff_start, IDLE_AUDIO_WINDOW_SAMPLES, WAKE_HANDOFF_MARGIN_SAMPLES,
};
