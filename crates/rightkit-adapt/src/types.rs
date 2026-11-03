//! Core data types for the learning pipeline.

use serde::{Deserialize, Serialize};

/// The category of a learning proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalKind {
    /// A durable memory item.
    Memory,
    /// A harness-level rule (e.g. "avoid X").
    HarnessRule,
    /// A reusable multi-step workflow.
    Workflow,
    /// A tweak to an existing skill.
    SkillTweak,
    /// A newly synthesized skill distilled from a successful complex run.
    Skill,
}

/// Lifecycle status of a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    /// Freshly extracted, not yet scored.
    Candidate,
    /// Scored by the evaluator.
    Evaluated,
    /// Accepted and deployed/eligible for deployment.
    Promoted,
    /// Rejected.
    Rejected,
    /// Aged out before promotion.
    Expired,
}

/// A single learning proposal candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Stable identifier, e.g. `prop-0`.
    pub id: String,
    /// Category of the proposal.
    pub kind: ProposalKind,
    /// Short human-readable summary.
    pub summary: String,
    /// Supporting evidence text.
    pub evidence: String,
    /// Score set by the evaluator; 0.0 until evaluated.
    pub score: f64,
    /// Current lifecycle status.
    pub status: ProposalStatus,
    /// Creation timestamp (opaque string).
    pub created_at: String,
    /// How many sessions have elapsed since creation (for forgetting).
    pub age_sessions: u32,
    /// Why this proposal was rejected (None unless status == Rejected). Mirrors
    /// SkillOpt's RejectedEdit reason so rejected approaches aren't silently lost.
    #[serde(default)]
    pub rejection_reason: Option<String>,
}
