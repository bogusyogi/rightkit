//! Dictation core: the GUI-free half of a push-to-talk dictation app.
//!
//! - [`engine`]: the versioned JSON wire protocol between the app shell and its speech engine
//!   process (frames, payloads, validation, launch frame, delivery policy), secret-bearing
//!   strings that wipe on drop, and the provider key handoff to the engine.
//! - [`state`]: the recording session state machine.
//! - [`wake`]: wake word listener state.
//! - [`mic_selection`]: choosing a capture device from the available inputs.
//! - [`audio_conditioning`]: capture-side conditioning of PCM before ASR.
//!
//! - [`settings`]: the versioned settings document (schema, defaults, migration, patches).
//! - [`app_paths`]: the per-user app-data root for the folder name the host configures.
//! - [`redact_diagnostic_event`], [`redact_payload_value`], [`redact_payload_text`]: redaction
//!   before anything reaches logs or telemetry.
//!
//! Transcript records and history are `rightkit-transcript-library`; OS capture, delivery and the engine
//! process itself stay in the host.

pub mod app_paths;
pub mod audio_conditioning;
pub mod engine;
pub mod mic_selection;
mod redact;
pub mod settings;
pub mod state;
pub mod wake;

pub use redact::{redact_diagnostic_event, redact_payload_text, redact_payload_value};
