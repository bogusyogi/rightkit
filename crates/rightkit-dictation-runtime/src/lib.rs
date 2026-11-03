//! Sidecar worker, runtime state machine, JSON-RPC host, and diagnostics.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::doc_lazy_continuation,
    clippy::manual_clamp
)]

pub mod crash_capture;
pub mod engine_paths;
pub mod entry;
pub mod ipc;
pub mod owner_diagnostics;
/// Test-mode perf levers (2026-10-29 audit); release builds compile them to their defaults.
pub mod runtime;
pub mod speech_service;
pub mod windows_audio_lifecycle;
pub mod worker;

#[cfg(target_os = "macos")]
pub use rightkit_dictation_command_host::macos_input;
pub use rightkit_dictation_command_host::{
    app_launch, command_classify, command_dispatch, delivery, focus,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use rightkit_dictation_inference::kws;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use rightkit_dictation_inference::{asr, trigger_audio_cut};
#[cfg(target_os = "macos")]
pub use rightkit_dictation_inference::{av_decode, coreml, coreml_asr, whisper_coreml};
pub use rightkit_dictation_inference::{calibration, file_transcribe, inference_gate, vad};
#[cfg(target_os = "windows")]
pub use rightkit_dictation_inference::{whisper_win, whisper_win_ffi};
pub use rightkit_dictation_text_pipeline::{
    canonical_polish_harness, guardrail_replay, l1c1_serialize, l3_cleanup, screen_vocab, settings,
    text_polish, vocabulary,
};
