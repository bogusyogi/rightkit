//! Command recognition, focus capture, delivery, launch, and OS adapters.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::manual_clamp
)]

pub mod app_launch;
pub mod command_classify;
pub mod command_dispatch;
pub mod delivery;
pub mod evidence_gate;
pub mod focus;
pub mod gated_writes;
pub mod host;
#[cfg(target_os = "macos")]
pub mod macos_input;
pub mod paste_delivery;
pub mod paste_gate;
pub mod pulse_handoff;
pub mod restore_gate;
pub mod streaming;
mod user_dirs;

pub use rightkit_dictation_text_pipeline::{l3_cleanup, settings};
