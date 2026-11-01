//! Portable timeline core for CutRight (media plan step 1).
//!
//! - [`time`]: tick time base and the single frame-rounding rule.
//! - [`model`]: sources, video tracks, audio tracks, overlay extension point.
//! - [`action`]: typed edits, ripple semantics, inverse actions.
//! - [`store`]: revision log, undo/redo, canonical JSON persistence.
//! - [`plan`]: timeline -> encoder-agnostic render plan.
//! - [`title`]: typed title templates -> `drawtext` filters and text files.
//! - [`ffmpeg`]: render plan -> typed FFmpeg arguments (never executed here).

pub mod action;
pub mod error;
pub mod fcpxml;
pub mod ffmpeg;
pub mod model;
pub mod otio;
pub mod plan;
pub mod store;
pub mod time;
pub mod title;

pub use action::{Action, InverseAction};
pub use error::TimelineError;
pub use ffmpeg::{
    build_ffmpeg_command, build_ffmpeg_measure, build_ffmpeg_render, build_ffmpeg_render_styled,
    AudioFinish, BuiltRender, CropAnchor, Encoder, FaceTrack, FaceTracks, FitPolicy, Preset,
    RenderRequest, RenderStyle,
};
pub use model::*;
pub use plan::{compile, RenderPlan, VideoSegment};
pub use store::{ApplyOutcome, Revision, TimelineStore, SCHEMA_VERSION};
pub use time::{Fps, MilliDb, Ticks, TICKS_PER_SECOND};
pub use title::{TextFile, TitleAssets};
