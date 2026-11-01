//! `rightkit-reframe` — portable tracked 9:16 reframe (moved from CutRight).
//!
//! Pipeline (all Rust, no Swift, nothing Node/Python at runtime):
//!
//! 1. [`analyze()`](fn@analyze): sample frames with the editing ffmpeg (reduced fps/size, raw RGB), detect
//!    faces with YuNet through ONNX Runtime (CoreML on macOS, DirectML on Windows, CPU
//!    fallback — [`ort_rt`]), track them ([`tracker`]), pick the active speaker from VAD
//!    speech spans plus per-face mouth-region motion energy, fall back to motion saliency
//!    when no face is visible, and emit per-sample [`track::TrackSample`] evidence.
//! 2. [`plan::plan_from_evidence`]: fuse and smooth ([`track`], ported from the old
//!    `reframe_track.rs`: spring + acceleration/velocity clamps, dead zone, safe zone, cut
//!    snapping, gap hold) into a [`plan::ReframePlan`] of crop keyframes in source pixels,
//!    with reviewable anchors. Low tracking confidence yields a *reported* centre-crop
//!    fallback instead.
//! 3. [`plan::ReframePlan::approve_all`] (or per-anchor approval + `approve`): the locked
//!    invariant — no vertical render without an approved plan and approved anchors, bound to
//!    the plan hash.
//! 4. [`render::render`]: ffmpeg `crop` + per-frame `sendcmd` positions, atomic and
//!    idempotent, source BLAKE3 re-verified.

pub mod analyze;
pub mod detect;
pub mod error;
pub mod frame;
pub mod media;
pub mod models;
pub mod ort_rt;
pub mod plan;
pub mod render;
pub mod track;
pub mod tracker;

pub use analyze::{analyze, analyze_with_yunet, speech_spans, AnalyzeOptions, Evidence};
pub use error::{ReframeError, Result};
pub use media::{SampleSpan, Tools};
pub use plan::{plan_from_evidence, ManualAnchor, PlanMode, PlanParams, ReframePlan};
pub use render::{render, RenderOptions, RenderReceipt};
