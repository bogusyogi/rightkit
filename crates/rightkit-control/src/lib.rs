//! rightkit-control: drive a real desktop app like a human, in the background.
//!
//! macOS: embedded targets receive main-thread AppKit `NSEvent`s through the
//! WKWebView responder, including hidden, non-key windows. Non-embedded targets
//! use per-pid `CGEventPostToPid` (never the global HID tap); inactive-window
//! first-mouse handling can discard their press. Neither path activates apps
//! or moves the user's cursor.
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
//! - [`offscreen`]: `launch_offscreen` for recording scenarios (placement outside all
//!   displays, qualification, process-activity/EcoQoS defences).
//!
//! - [`lease_store`]: optional durable lease state (store trait plus an
//!   atomic file store), execution identity and `Running | Exited | Unknown`.
//!
//! Every effectful entry point routes through the gate: `webdriver::Server`,
//! the `tauri-plugin` server, `mac::Gated` and the `rightkit-control` CLI
//! ([`cli`]). The raw macOS primitives are reachable only through the
//! explicitly named `mac::unsafe_ungated` opt-in.
pub mod admission;
pub mod cli;
#[cfg(feature = "tauri-plugin")]
pub mod embedded;
pub mod events;
pub mod json;
pub mod keys;
pub mod lease;
pub mod lease_store;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod offscreen;
pub mod webdriver;
