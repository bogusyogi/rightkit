//! Paste delivery: the clipboard transaction with proof-of-paste confirmation,
//! the delivery state machine with its copy fallbacks, and stop-time
//! destination capture and pinning (UIA pin, WezTerm pane pin or native window
//! on Windows; AX focus pin on macOS).
//!
//! The pure record shapes, ids and fail-closed rule functions live in
//! `rightkit_transcript_library::delivery` and are re-exported here. Every
//! app-specific value comes in through [`DeliveryHost`].

pub use rightkit_transcript_library::delivery::*;

mod backend;
mod controls;
mod fallback;
mod host;
mod targets;

pub use backend::*;
pub use controls::*;
pub use fallback::*;
pub use host::*;
pub use targets::*;
