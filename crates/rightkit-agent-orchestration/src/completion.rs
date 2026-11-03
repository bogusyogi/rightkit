//! Completion arbiter — decides a goal's terminal state from its Graceful
//! Degradation Score (GDS) and the victory-gate evidence.
//!
//! Substantial completion is no longer collapsed into a binary pass/fail: a goal
//! that satisfied 8 of 10 weighted predicates scores GDS 0.80 and lands in
//! `PartiallyComplete`, not `Rejected`. Completion (`Accepted`) still requires a
//! jury-approved `goal_verification` artifact AND full GDS — never a worker's
//! self-assessment.

/// The terminal state of a goal run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionState {
    /// GDS == 1.0 and the goal_verification artifact is jury-approved.
    Accepted,
    /// GDS ≥ partial threshold but not fully done.
    PartiallyComplete,
    /// Evidence is missing (no goal_verification, or jury reached no consensus).
    NeedsReview,
    /// GDS below the partial threshold.
    Rejected,
    /// Routed to the boss/human for resolution.
    Escalate,
}

impl CompletionState {
    pub fn as_str(self) -> &'static str {
        match self {
            CompletionState::Accepted => "Accepted",
            CompletionState::PartiallyComplete => "PartiallyComplete",
            CompletionState::NeedsReview => "NeedsReview",
            CompletionState::Rejected => "Rejected",
            CompletionState::Escalate => "Escalate",
        }
    }

    /// Only `Accepted` counts as a fully complete goal.
    pub fn is_complete(self) -> bool {
        matches!(self, CompletionState::Accepted)
    }
}

/// The evidence the arbiter weighs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompletionInput {
    /// Graceful Degradation Score: weighted fraction of satisfied predicates (0–1).
    pub gds: f64,
    /// A goal_verification artifact exists.
    pub verification_present: bool,
    /// The auto-jury reached a decision (consensus, not a tie/below-quorum).
    pub jury_consensus: bool,
    /// The auto-jury approved the goal_verification.
    pub verification_approved: bool,
    /// An explicit escalation condition fired (e.g. repeated failure / time cap).
    pub escalate: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct CompletionConfig {
    /// GDS at or above which a non-accepted goal is `PartiallyComplete`.
    pub partial_threshold: f64,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self {
            partial_threshold: 0.70,
        }
    }
}

/// The arbiter's verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct Arbitration {
    pub state: CompletionState,
    pub gds: f64,
    pub rationale: String,
}

/// Decide the terminal state. Order matters: escalation and missing evidence
/// take precedence over the GDS banding.
pub fn arbitrate(input: &CompletionInput, cfg: &CompletionConfig) -> Arbitration {
    let gds = input.gds.clamp(0.0, 1.0);
    let state = if input.escalate {
        CompletionState::Escalate
    } else if !input.verification_present || !input.jury_consensus {
        CompletionState::NeedsReview
    } else if input.verification_approved && gds >= 1.0 {
        CompletionState::Accepted
    } else if gds >= cfg.partial_threshold {
        CompletionState::PartiallyComplete
    } else {
        CompletionState::Rejected
    };
    let rationale = match state {
        CompletionState::Accepted => "GDS 1.0 and jury-approved verification".into(),
        CompletionState::PartiallyComplete => {
            format!(
                "GDS {gds:.2} ≥ {:.2} but not fully done",
                cfg.partial_threshold
            )
        }
        CompletionState::NeedsReview => "missing goal_verification or no jury consensus".into(),
        CompletionState::Rejected => {
            format!(
                "GDS {gds:.2} below partial threshold {:.2}",
                cfg.partial_threshold
            )
        }
        CompletionState::Escalate => "escalation condition fired".into(),
    };
    Arbitration {
        state,
        gds,
        rationale,
    }
}
