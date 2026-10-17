//! rightkit-review: the shared review capability for the content pipeline
//! (GenRight G6, CutRight C10, PostRight P4).
//!
//! The pieces, in the order a review uses them:
//!
//! 1. [`Subject`] describes one artifact and the brief declarations it is
//!    checked against. Measurements arrive as data; this crate never decodes media.
//! 2. [`RuleSet`] holds typed deterministic rules, for media and text
//!    (`RuleKind::Extension` admits consumer rules through a [`RuleRegistry`]).
//!    Each rule returns one
//!    [`Verdict`]: verified, failed, unverified, inconclusive, held, or not
//!    applicable. A declared hold in the brief exempts a deliberate pause,
//!    silence, or static region.
//! 3. [`disposition`] turns findings into pass, hold, or block. Failures
//!    block, required unknowns block, and a held finding routes to a human.
//! 4. [`run_repair_loop`] repairs a blocked candidate through a [`Repairer`],
//!    capped by [`RepairCap`] at no more than [`HARD_MAX_REPAIR_ATTEMPTS`].
//!    It returns the last inspectable candidate when the cap is reached.
//! 5. [`Judge`] is an advisory interface. Its scores are recorded and cannot
//!    change the disposition. No model is called in this crate.
//! 6. [`VerdictRecord`] is the versioned audit record. It is refused when the
//!    schema or version is not one this build reads.
//!    A record carries a [`PolicyBinding`] (policy hash and per-rule check versions);
//!    [`VerdictRecord::verify_current`] refuses a stored verdict once either changed.
//! 7. [`ladder`] is the autonomy ladder (reviewed, review-light, autonomous) per
//!    content type and platform, with overridable thresholds and a versioned state.

#![forbid(unsafe_code)]

mod binding;
mod error;
mod extension;
mod judge;
pub mod ladder;
mod record;
mod repair;
mod rules;
mod subject;
mod text;
mod verdict;

pub use binding::{check_version, PolicyBinding, BUILTIN_CHECK_VERSION, EXTENSION_CHECK_VERSION};
pub use error::ReviewError;
pub use extension::{ExtensionRule, RuleRegistry};
pub use judge::{Judge, JudgeRecord, JudgeScore, NoJudge};
pub use record::{Attempt, VerdictRecord, VERDICT_RECORD_SCHEMA, VERDICT_RECORD_VERSION};
pub use repair::{
    run_repair_loop, run_repair_loop_with, LoopOutcome, RepairCap, Repairer, StopReason,
    HARD_MAX_REPAIR_ATTEMPTS,
};
pub use rules::{rule_ids, ApprovedProof, Rule, RuleKind, RuleSet};
pub use subject::{
    ArtifactDescription, ArtifactKind, ClaimAnnotation, Declarations, DeclaredWindow,
    DeterminismProbe, FrameStat, Subject, TextBody, TextRegion, WindowKind,
};
pub use verdict::{disposition, Disposition, Finding, Verdict};
