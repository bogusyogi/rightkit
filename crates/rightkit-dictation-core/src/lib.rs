//! Dictation core: the GUI-free half of a push-to-talk dictation app.
//!
//! - [`engine`]: the versioned JSON wire protocol between the app shell and its speech engine
//!   process (frames, payloads, validation, launch frame, delivery policy), secret-bearing
//!   strings that wipe on drop, and the provider key handoff to the engine.
//! - [`state`] and [`controller`]: the recording session state machine and the controller that
//!   turns engine results into delivery records and history entries.
//! - [`wake`]: wake word listener state.
//! - [`mic_selection`]: choosing a capture device from the available inputs.
//! - [`audio_conditioning`]: capture-side conditioning of PCM before ASR.
//!
//! - [`settings`]: the versioned settings document (schema, defaults, migration, patches).
//! - [`app_paths`]: the per-user app-data root for the folder name the host configures.
//! - [`redact_diagnostic_event`], [`redact_payload_value`], [`redact_payload_text`]: redaction
//!   before anything reaches logs or telemetry.
//! - [`dev_var!`] and [`dev_var_os!`]: environment switches that exist only in test builds.
//!
//! Transcript records and history are `rightkit-transcript-library`; OS capture, delivery and the engine
//! process itself stay in the host.

/// `std::env::var(name)` in test and `test-mode` builds of the CALLING crate;
/// `Err(VarError::NotPresent)` otherwise, and a release build does not even contain the name.
/// Every crate that uses it declares its own `test-mode` feature.
#[macro_export]
macro_rules! dev_var {
    ($name:expr) => {{
        #[cfg(feature = "test-mode")]
        let value = ::std::env::var($name);
        #[cfg(not(feature = "test-mode"))]
        let value: ::std::result::Result<::std::string::String, ::std::env::VarError> =
            ::std::result::Result::Err(::std::env::VarError::NotPresent);
        value
    }};
}

/// `std::env::var_os(name)` in test and `test-mode` builds of the calling crate; `None`
/// otherwise.
#[macro_export]
macro_rules! dev_var_os {
    ($name:expr) => {{
        #[cfg(feature = "test-mode")]
        let value = ::std::env::var_os($name);
        #[cfg(not(feature = "test-mode"))]
        let value: ::std::option::Option<::std::ffi::OsString> = ::std::option::Option::None;
        value
    }};
}

pub mod app_paths;
pub mod audio_conditioning;
pub mod controller;
pub mod engine;
pub mod mic_selection;
mod redact;
pub mod settings;
pub mod state;
pub mod wake;

pub use redact::{redact_diagnostic_event, redact_payload_text, redact_payload_value};
