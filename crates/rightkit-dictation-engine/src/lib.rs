//! `rightkit-dictation-engine` library — exposes the in-process types that both the
//! sidecar binary (`main.rs`) and the integration tests (`tests/` dir) need.
//!
//! The binary entry point is intentionally separate from the library so the
//! tests can construct a `runtime::EngineRuntime` directly without spawning
//! a subprocess or driving stdin/stdout.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::doc_lazy_continuation,
    clippy::manual_clamp
)] // intentional: large variants live on cold error paths, a few arg-heavy platform fns, doc-comment style

pub use rightkit_dictation_runtime::{crash_capture, owner_diagnostics};
// The resident engine exists only where the runtime does (macOS and Windows).
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use rightkit_dictation_runtime::{ipc, runtime, worker};

#[cfg(target_os = "macos")]
pub use rightkit_dictation_command_host::macos_input;
pub use rightkit_dictation_command_host::{
    app_launch, command_classify, command_dispatch, delivery, focus,
};

// All targets: the keyword-spotting cascade runs on the Rust spotter and builds everywhere.
pub use rightkit_dictation_inference::kws;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use rightkit_dictation_inference::{asr, calibration, trigger_audio_cut};
#[cfg(target_os = "macos")]
pub use rightkit_dictation_inference::{av_decode, coreml, coreml_asr, whisper_coreml};
pub use rightkit_dictation_inference::{file_transcribe, inference_gate, vad};
#[cfg(target_os = "windows")]
pub use rightkit_dictation_inference::{whisper_win, whisper_win_ffi};

// Preserve public sidecar library paths while implementation lives in its
// dedicated compile unit.
pub use rightkit_dictation_text_pipeline::{
    canonical_polish_harness, guardrail_replay, l1c1_serialize, l3_cleanup, screen_vocab, settings,
    text_polish, vocabulary,
};
