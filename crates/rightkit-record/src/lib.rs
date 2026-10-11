//! `rightkit-record`: the platform-neutral core of the RightKit recorder (RK3).
//!
//! The core owns the recording lifecycle, the session bundle, redaction,
//! verification and the frame reader. It never captures pixels. A per-OS helper
//! implements [`Backend`] and reports frames, geometry, drops and failures as
//! [`HelperEvent`]s.
//!
//! Lifecycle: a take runs `starting -> recording -> stopping`, then ends
//! `complete`, `quarantined` or `invalid`. `complete` needs a clean stop, an
//! accepted readiness marker, and a manifest that validates. Helper death, disk
//! full, target exit and permission revocation each end the take in a terminal
//! state that is never `complete`. A stale take found by [`Recorder::open`] is
//! sealed the same way.
//!
//! Bundle files: `scenario.json`, `events.ndjson`, `clock.json`, `capture.json`,
//! `clean.mp4` (or a track), and `hashes.json`, which is the record-session
//! manifest. The bundle is read-only after stop.

// Unsafe code is forbidden in the platform-neutral core. The Windows capture
// module talks to COM, Direct3D and Media Foundation, so it alone allows it.
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![cfg_attr(windows, deny(unsafe_code))]

pub mod backend;
mod bundle;
pub mod clock;
pub mod error;
pub mod fake;
pub mod hash;
pub mod media;
pub mod plan;
pub mod process;
pub mod protocol;
pub mod reader;
pub mod recorder;
pub mod redact;
pub mod support;
pub mod verify;

// Pure helpers the Windows capture uses. Compiled for tests on every OS.
#[cfg(any(windows, test))]
mod codec;
#[cfg(any(windows, test))]
mod pacer;

/// Windows.Graphics.Capture backend, the Windows side of the [`Backend`] seam.
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;

/// ScreenCaptureKit backend, the macOS side of the [`Backend`] seam. The capture runs in a Swift
/// helper (`helpers/macos`); this module launches it and speaks the helper protocol.
#[cfg(target_os = "macos")]
pub mod macos;

pub use backend::{
    track_file, track_id, Backend, BackendPlan, Codec, Failure, Finished, FrameSample, HelperEvent,
    HelperIdentity, Permission, Source, StartInfo, TrackOutput,
};
pub use bundle::{Phase, Recovered};
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::{Code, RecordError, Result};
pub use media::{probe_mp4, MediaProbe, RecoveredMedia};
pub use plan::{PixelMasking, ScenarioSpec, ScenarioStep, StartRequest, TargetSpec};
pub use process::{serve_backend, ProcessBackend};
pub use reader::{BundleReader, ClockMap, CursorAsset, CursorAssetsStatus, SourceFrame, StepSpan};
pub use recorder::{Doctor, Outcome, Recorder, StateSupport, Status};
pub use redact::{
    FieldMode, FixedKeySource, HashKey, KeyHandle, KeySource, RedactionPolicy, SecretsKeySource,
};
pub use support::Support;
pub use verify::{VerifyOptions, VerifyReport};
