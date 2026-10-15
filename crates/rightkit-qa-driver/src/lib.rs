//! rightkit-qa-driver: drive a real desktop app like a human, in the background.
//!
//! The generic input code lives in `rightkit-automation` and is re-exported here under its
//! old paths, so existing callers keep compiling: `admission`, `events`, `lease` and
//! `lease_store` are `rightkit_automation::control::*`, `mac` is
//! `rightkit_automation::macos::background`, `offscreen` is `rightkit_automation::offscreen`
//! (plus the Tauri window helpers kept here), `json` is `rightkit_automation::json` and
//! `keys` is `rightkit_automation::keys::appkit`. What stays in this crate is the
//! WebDriver bridge, the macOS CLI and the `qa-control` embedded server.
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
//! the `qa-control` server, `mac::Gated` and the `rightkit-qa-driver` CLI
//! ([`cli`]). The raw macOS primitives are reachable only through the
//! explicitly named `mac::unsafe_ungated` opt-in. The process-wide kill switch of
//! `rightkit-automation` (`policy::kill`) stops every gated effect.
pub use rightkit_automation::control::{admission, events, lease, lease_store};
pub mod cli;
/// QA-only in-app control server. Compiled only with the explicit `qa-control` feature, which no shipped
/// build may enable (see the README section "QA control never ships"). `tauri-plugin` alone does not
/// expose it.
#[cfg(feature = "qa-control")]
pub mod embedded;
pub use rightkit_automation::json;
/// US-layout virtual keycodes and AppKit function-key data (`rightkit_automation::keys::appkit`).
pub use rightkit_automation::keys::appkit as keys;
#[cfg(target_os = "macos")]
pub use rightkit_automation::macos::background as mac;
pub mod offscreen;
pub mod webdriver;
