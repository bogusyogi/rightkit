//! CutRight project layer (media plan step 1).
//!
//! - [`project`]: directory layout, canonical `project.json`/`timeline.json`, init/open/show.
//! - [`lock`]: cross-process exclusive project lock with stale-owner recovery.
//! - [`fsutil`]: atomic file writes.
//! - [`tools`]: ffmpeg/ffprobe resolution from the engine bundle only.
//! - [`probe`]: typed ffprobe JSON boundary.
//! - [`source`]: source registration (copy or reference, BLAKE3, probe, timeline action).
//! - [`ingest`]: robust multi-file import of raw recordings (deep probe, CFR proxy when needed).
//! - [`worker`]: FFmpeg execution with progress, cancellation and timeout.
//! - [`loudness`]: two-pass loudnorm measurement for final renders.
//! - [`reframe_track`]: portable crop-path smoothing (anchor points in, typed path out).
//! - [`render`]: render orchestration (source reverification, encoder choice, partial rename).

pub mod audio_sync;
pub mod error;
pub mod fsutil;
pub mod ingest;
pub mod lock;
pub mod loudness;
pub mod probe;
pub mod project;
pub mod reframe_track;
pub mod render;
pub mod source;
pub mod still;
pub mod tools;
pub mod worker;

pub use error::ProjectError;
pub use lock::ProjectLock;
pub use project::{
    Access, EditOutcome, InitStatus, Project, ProjectMeta, SourceMode, SourceRecord,
};
pub use tools::MediaTools;
