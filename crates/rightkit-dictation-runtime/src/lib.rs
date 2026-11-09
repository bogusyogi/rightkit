//! Sidecar worker, runtime state machine, JSON-RPC host, and diagnostics.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::doc_lazy_continuation,
    clippy::manual_clamp
)]

// The resident engine (runtime, worker, IPC host, speech service and the entry point) is built
// only on macOS and Windows: `calibration` exists only there, and the finalize path builds its
// polish contexts under the same gate. Those modules, and the roots only they use, carry the same
// gate; Linux builds and lints the portable remainder (crash capture, owner diagnostics, the
// lifecycle stub and the re-exports).
pub mod crash_capture;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod engine_paths;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod entry;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod ipc;
pub mod owner_diagnostics;
/// Test-mode perf levers (2026-10-29 audit); release builds compile them to their defaults.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod runtime;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod speech_service;
pub mod windows_audio_lifecycle;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod worker;

#[cfg(target_os = "macos")]
pub use rightkit_dictation_command_host::macos_input;
pub use rightkit_dictation_command_host::{
    app_launch, command_classify, command_dispatch, delivery, focus,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use rightkit_dictation_inference::kws;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use rightkit_dictation_inference::{asr, calibration, trigger_audio_cut};
#[cfg(target_os = "macos")]
pub use rightkit_dictation_inference::{av_decode, coreml, coreml_asr, whisper_coreml};
pub use rightkit_dictation_inference::{file_transcribe, inference_gate, vad};
#[cfg(target_os = "windows")]
pub use rightkit_dictation_inference::{whisper_win, whisper_win_ffi};
pub use rightkit_dictation_text_pipeline::{
    canonical_polish_harness, guardrail_replay, l1c1_serialize, l3_cleanup, screen_vocab, settings,
    text_polish, vocabulary,
};
