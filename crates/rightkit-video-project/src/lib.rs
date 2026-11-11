//! Video project layer for Right Suite video apps.
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
//! - `engine` (feature `engine-commands`): the support layer engine command modules share (JSON and
//!   transcript files, analysis layout, clip ranges, flat settings files, project font, media tools).
//! - `channel_recipe` (feature `channel-recipe`): the typed check of a channel recipe body.
//! - `cmd` (feature `engine-commands`): `projects.create`, `slot.fill`, `storage.status` and
//!   `storage.clear` handlers.

pub mod audio_sync;
#[cfg(feature = "channel-recipe")]
pub mod channel_recipe;
#[cfg(feature = "engine-commands")]
pub mod cmd;
#[cfg(feature = "engine-commands")]
pub mod engine;
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
