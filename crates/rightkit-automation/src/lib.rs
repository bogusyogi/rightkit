//! rightkit-automation: brand-neutral desktop automation for Rust, on macOS and
//! Windows, with no Python or Node runtime. Moved here from HeardRight's
//! `heardright_platform` and `heardright-command-host` (see `docs/MOVED_FROM.md`);
//! HeardRight's locked rules travel with it (`docs/LOCKED_RULES.md`).
//!
//! Observe: apps, windows, accessibility trees with stable element ids (macOS AX,
//! Windows UIA), the focused element, per-window screenshots (redacted).
//! Act through an input ladder that never goes up a tier unless the caller
//! allowed it and records which tier ran:
//!   1. semantic AX / UIA action (no input events)
//!   2. in-app control channel (`rightkit-control`, over `rightkit-daemon`)
//!   3. per-pid / per-window addressed events (never the global queue)
//!   4. foreground input, only with explicit opt-in
//!
//! Every effect passes, in order: kill switch, allowlist and deny list, **lease
//! fence** (`lease`), freshness guard, secure-field / **credential** / risk gates,
//! the caller's **admission hook** (`admission`, CodeRight EFF-001), then runs and
//! is *settled*; receipts name the tier and the gate that refused. Lifecycle events
//! (`events`: started, stopped, target exited, denied, lease granted/fenced) go to the
//! caller's journal; counters are content-free; nothing is uploaded.
//! Product policy (dictation, command classification, pill UI) stays in the product.
//!
//! Threading (macOS): NSWorkspace publishes the app list and frontmost app through
//! the main run loop. Call from the main thread of a CLI/test, or from any thread of
//! an app whose main loop is running.
//!
//! Hosting: in-process only, no daemon. `launch` uses the short-lived system `open`
//! (macOS) and, only if `CGWindowListCreateImage` is missing, `screencapture`; both
//! are waited children (a strict zero-child profile should not call `launch`).

pub mod admission;
pub mod delivery;
pub mod engine;
pub mod events;
pub mod focus;
pub mod in_app;
pub mod json;
pub mod keys;
pub mod lease;
pub mod mock;
pub mod model;
#[cfg(feature = "in-app")]
pub mod paths;
pub mod platform;
pub mod png;
pub mod policy;
pub mod proof;
pub mod redact;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod win;

pub use admission::{Admission, Admit, AllowAll, EffectKind, EffectRequest, Settled};
pub use delivery::{DeliveryOptions, DeliveryOutcome, DeliveryRecord, FocusPin, Why};
pub use engine::{ActOptions, ActOutcome, Automation, Guard};
pub use events::{DeniedBy, Journal, LifecycleEvent, MemoryJournal, Metrics};
pub use focus::{FocusReport, FocusWatch};
pub use lease::{Fence, Lease, LeaseRequest, LeaseStatus};
pub use model::*;
pub use platform::{ClipSnapshot, InputEvent, Platform};
pub use policy::{
    classify_risk, credential_label, kill, killed, looks_secret, redact, reset_kill, Attempt,
    EvidenceSink, Indicator, JsonlEvidence, MemoryEvidence, MemoryIndicator, Policy, Receipt,
    ResultKind, SessionEvent,
};
pub use redact::{MaskText, Redactor, ShotContext, ShotReport};

#[cfg(target_os = "macos")]
pub type NativePlatform = macos::MacPlatform;
#[cfg(windows)]
pub type NativePlatform = win::WinPlatform;

/// The real backend for this OS.
#[cfg(any(target_os = "macos", windows))]
pub fn native(policy: Policy) -> Automation<NativePlatform> {
    Automation::new(NativePlatform::new(), policy)
}
