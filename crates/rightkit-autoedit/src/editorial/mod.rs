//! CutRight editorial engine (M2): transcript (+ optional audio envelope) ->
//! reviewable [`EditorialPlan`] -> [`CutPlan`](crate::cuts::CutPlan) -> one
//! undoable timeline action group.
//!
//! Builds on `cutright-cuts` (silence, fillers, retakes). Adds:
//! * dead air beyond word gaps: breaths, quiet trailing-off speech, off-mic
//!   speech, over-long pauses;
//! * disfluency: repetitions, stutters, self-corrections, false starts;
//! * retakes completed: takes of a line scored and ranked with explanations,
//!   the best COMPLETE take kept (user can pick another), spoken retake cues;
//! * narrative beats (hook / setup / point / example / CTA) from transcript
//!   structure, no model;
//! * the plan itself: ordered [`EditorialItem`]s with reason, confidence and
//!   evidence, reviewable item by item ([`Review`]) and resolved with
//!   [`EditorialPlan::resolve`] into one action group (undoable).
//!
//! Pure computation, deterministic, portable.

mod beats;
mod config;
mod cross;
mod deadair;
mod disfluency;
mod error;
mod item;
mod plan;
mod retake;
mod util;
pub mod visual;

pub use beats::{Beat, BeatLabel};
pub use config::{
    BeatConfig, DeadAirConfig, DisfluencyConfig, EditorialConfig, RetakePolicy, TakeWeights,
    VisualConfig,
};
pub use cross::{cross_file_retakes, CrossFileRetake};
pub use error::EditorialError;
pub use item::{Category, EditorialItem, Evidence, ItemAction, ItemKind, TakeRef, Tier};
pub use plan::{
    analyze, EditorialPlan, ItemOutcome, ResolvedEdit, RetakeEscalation, Review, SCHEMA,
};
pub use retake::{TakeGroup, TakeInfo};

#[cfg(test)]
mod tests;
