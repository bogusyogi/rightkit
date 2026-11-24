//! Voice command recognition for dictation apps.
//!
//! - [`command`]: the typed [`CommandAction`] a recognizer hands a dispatcher (pure serde; no raw
//!   shell strings ever cross the boundary).
//! - [`command_recognition`]: decides whether a finished transcript is a standalone command and
//!   which action it requests: direct phrases, spoken chords, app launch and macOS Shortcuts
//!   queries, plus the discoverable command catalog. No OS side effects.
//! - [`compound`]: "X and Y" and ordinal targets ("go to the first chat") as a plan of steps, run against
//!   a host that supplies the target resolver and verifies each target before acting.
//! - [`external_url`]: exact-match allowlist check for links a webview asks the shell to open.
//!
//! Dispatch (sending keys, launching apps, power operations) stays in the host application.

pub mod command;
pub mod command_recognition;
pub mod compound;
pub mod external_url;

pub use command::CommandAction;
pub use external_url::is_allowed_external_url;
