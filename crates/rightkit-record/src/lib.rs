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

#![forbid(unsafe_code)]

pub mod backend;
mod bundle;
pub mod clock;
pub mod error;
pub mod fake;
pub mod hash;
pub mod plan;
pub mod reader;
pub mod recorder;
pub mod redact;
pub mod support;
pub mod verify;

pub use backend::{
    track_file, track_id, Backend, BackendPlan, Failure, Finished, FrameSample, HelperEvent,
    HelperIdentity, Permission, Source, StartInfo, TrackOutput,
};
pub use bundle::{Phase, Recovered};
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::{Code, RecordError, Result};
pub use plan::{PixelMasking, ScenarioSpec, ScenarioStep, StartRequest, TargetSpec};
pub use reader::{BundleReader, SourceFrame, StepSpan};
pub use recorder::{Doctor, Outcome, Recorder, StateSupport, Status};
pub use redact::{FieldMode, RedactionPolicy};
pub use support::Support;
pub use verify::{VerifyOptions, VerifyReport};
