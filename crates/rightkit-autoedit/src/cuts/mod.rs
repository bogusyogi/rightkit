//! CutRight cut engine (M2): transcript (+ optional audio envelope) ->
//! [`CutPlan`] -> timeline actions + source->output [`TimeMap`].
//!
//! Pure computation, portable. Destructive cuts always sit between words
//! (transcript evidence); `NextTokenStart` word ends are refined with VAD and
//! the envelope before any pause is measured.

mod config;
mod envelope;
mod error;
mod plan;
mod realise;
mod retake_rules;
mod style;
mod text;

pub use config::{CutConfig, ManualRemoval, Preset, RetakeConfig};
pub use error::CutError;
pub use plan::{plan_cuts, CutPlan, Decision, KeptRange, Removal, RemovalReason};
pub use realise::{
    apply_group, undo_group, ActionGroup, AppliedGroup, MapSegment, MappedWord, RealiseOptions,
    ReplaceTarget, TimeMap,
};
pub use retake_rules::{
    cut_mask, escalations, is_broken_take, self_talk, soft_filler_gate, take_tier, RetakeRule,
    RuleConfig, RuleHit, Tier, Verdict,
};
pub use style::{EditingStyle, FillerPolicy, GapMs, Pace, Provenance, StyleValue};
