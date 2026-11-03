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
pub mod focus;
pub mod host;
#[cfg(target_os = "macos")]
pub mod macos_input;
pub mod pulse_handoff;
mod user_dirs;

pub use rightkit_dictation_text_pipeline::{l3_cleanup, settings};
