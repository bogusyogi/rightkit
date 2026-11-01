//! Completion arbiter — decides a goal's terminal state from its Graceful
//! Degradation Score (GDS) and the victory-gate evidence.
//!
//! Substantial completion is no longer collapsed into a binary pass/fail: a goal
//! that satisfied 8 of 10 weighted predicates scores GDS 0.80 and lands in
//! `PartiallyComplete`, not `Rejected`, provided its verification was approved.
//! A failed verification is `Rejected` however many tasks finished. Completion
//! (`Accepted`) still requires a jury-approved `goal_verification` artifact AND
//! full GDS — never a worker's self-assessment.

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
    } else if !input.verification_approved {
        // A verification that failed (the independent signoff answered FAIL, or
        // the jurors rejected after a pass) is a rejection, not a partial
        // completion, however many tasks finished (orchestration rule 12, D10).
        CompletionState::Rejected
    } else if gds >= 1.0 {
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
        CompletionState::Rejected if !input.verification_approved => {
            format!("verification failed (GDS {gds:.2})")
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CompletionInput {
        CompletionInput {
            gds: 1.0,
            verification_present: true,
            jury_consensus: true,
            verification_approved: true,
            escalate: false,
        }
    }

    #[test]
    fn full_gds_and_approval_is_accepted() {
        let a = arbitrate(&base(), &CompletionConfig::default());
        assert_eq!(a.state, CompletionState::Accepted);
        assert!(a.state.is_complete());
    }

    #[test]
    fn substantial_but_incomplete_is_partial_not_rejected() {
        let mut i = base();
        i.gds = 0.8;
        let a = arbitrate(&i, &CompletionConfig::default());
        assert_eq!(a.state, CompletionState::PartiallyComplete);
        assert!(!a.state.is_complete());
    }

    #[test]
    fn failed_verification_is_rejected_however_many_tasks_finished() {
        let mut i = base();
        i.gds = 0.8;
        i.verification_approved = false; // the independent signoff answered FAIL
        let a = arbitrate(&i, &CompletionConfig::default());
        assert_eq!(a.state, CompletionState::Rejected);
        assert!(a.rationale.contains("verification failed"));
    }

    #[test]
    fn low_gds_is_rejected() {
        let mut i = base();
        i.gds = 0.4;
        i.verification_approved = false;
        assert_eq!(
            arbitrate(&i, &CompletionConfig::default()).state,
            CompletionState::Rejected
        );
    }

    #[test]
    fn missing_verification_is_needs_review() {
        let mut i = base();
        i.verification_present = false;
        assert_eq!(
            arbitrate(&i, &CompletionConfig::default()).state,
            CompletionState::NeedsReview
        );
    }

    #[test]
    fn no_jury_consensus_is_needs_review() {
        let mut i = base();
        i.jury_consensus = false;
        assert_eq!(
            arbitrate(&i, &CompletionConfig::default()).state,
            CompletionState::NeedsReview
        );
    }

    #[test]
    fn escalation_takes_precedence() {
        let mut i = base();
        i.escalate = true;
        assert_eq!(
            arbitrate(&i, &CompletionConfig::default()).state,
            CompletionState::Escalate
        );
    }

    #[test]
    fn approved_but_not_full_gds_is_partial() {
        let mut i = base();
        i.gds = 0.9; // approved but predicates not all satisfied
        assert_eq!(
            arbitrate(&i, &CompletionConfig::default()).state,
            CompletionState::PartiallyComplete
        );
    }
}
