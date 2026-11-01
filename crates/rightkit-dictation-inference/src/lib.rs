//! Model inference, speech detection, calibration, and file transcription.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::manual_clamp
)]

#[cfg(any(target_os = "macos", target_os = "windows"))]
/// The TDT/RNN-T beam emission reward: the platform default from `rightkit-asr`, overridable in
/// test builds by `RIGHTKIT_ASR_BEAM_EMIT_REWARD` (a finite, non-negative number).
pub fn beam_emit_reward() -> f32 {
    static REWARD: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *REWARD.get_or_init(|| {
        rightkit_dictation_core::dev_var!("RIGHTKIT_ASR_BEAM_EMIT_REWARD")
            .ok()
            .and_then(|raw| raw.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or_else(rightkit_asr::parakeet_core::tdt_beam::default_emit_reward)
    })
}

pub mod asr;
#[cfg(target_os = "macos")]
pub mod av_decode;
// Onboarding calibration drives `asr`, which exists only on macOS and Windows.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod calibration;
#[cfg(any(target_os = "macos", target_os = "windows"))]
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
// All targets: the cascade policy and the Rust backend build and test everywhere; the
// Sherpa C API backend loads a bundled runtime on macOS and Windows only.
pub mod sherpa_kws;
pub mod subtitle_export;
pub mod timeline_recorder;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod trigger_audio_cut;
pub mod vad;
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
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use trigger_audio_cut::{resolve_trigger_audio_cut, TriggerAudioCut, KWS_ONSET_CUT_SOURCE};
