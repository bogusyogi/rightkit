//! rightkit-control: drive a real desktop app like a human, in the background.
//!
//! macOS: input is posted to the target pid with `CGEventPostToPid` (never the
//! global HID tap), so the frontmost app and the user's cursor are untouched.
//! State is read through the Accessibility API and window captures.
//!
//! Control-plane guarantees (CodeRight EFF-001 / PTY-002):
//! - [`admission::EffectGate`]: every effectful request is validated, approved
//!   by a host [`admission::AdmissionHook`], executed, and settled; a denial
//!   happens before any side effect.
//! - [`lease::InputLease`]: input carries `(epoch, sequence)`; stale epochs are
//!   fenced, duplicates are deduplicated, unacknowledged input must be
//!   explicitly reconciled. [`lease::ControlSession`] composes both.
//! - [`events::ControlEvent`]: content-free lifecycle events for the journal.
//!
//! The raw `mac` primitives are ungated building blocks; host-facing surfaces
//! (`webdriver::Server`, the `tauri-plugin` server) route through the gate.
pub mod admission;
#[cfg(feature = "tauri-plugin")]
pub mod embedded;
pub mod events;
pub mod json;
pub mod keys;
pub mod lease;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod webdriver;
