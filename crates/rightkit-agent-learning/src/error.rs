//! Error types for the learning pipeline.

use thiserror::Error;

/// Errors produced by the learning pipeline.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LearningError {
    /// No proposal with the given id exists in the store.
    #[error("proposal not found: {0}")]
    NotFound(String),

    /// The proposal is not in the `Promoted` state and therefore cannot be deployed.
    #[error("proposal not promoted: {0}")]
    NotPromoted(String),

    /// Skill proposals require the async synthesis loop, not deterministic sync deployment.
    #[error("skill proposal requires async synthesis: {0}")]
    SkillRequiresAsyncSynthesis(String),

    /// The proposal is not eligible for promotion (not evaluated, or below threshold).
    #[error("proposal not eligible for promotion: {0}")]
    NotEligible(String),
}
