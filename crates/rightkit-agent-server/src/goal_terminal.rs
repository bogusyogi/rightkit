//! Goal terminal arbitration (F2).
//!
//! Maps the conductor's jury-gated victory-gate verdict (`CompletionState`,
//! produced by `run_victory_gate` which runs the goal_verification jury) to a
//! persistent `GoalStatus`. `Achieved` is reachable ONLY here — never from the
//! engine's turn-end continuation loop, which can only `blocked`/`budget-limited`.

use rightkit_agent_engine_storage::GoalStatus;
use rightkit_agent_orchestration::coding::CompletionState;

pub fn goal_status_from_completion(state: CompletionState) -> GoalStatus {
    match state {
        CompletionState::Accepted => GoalStatus::Achieved,
        CompletionState::Rejected => GoalStatus::Unmet,
        CompletionState::Escalate => GoalStatus::Blocked,
        // More work remains — keep the goal open.
        CompletionState::NeedsReview | CompletionState::PartiallyComplete => GoalStatus::Active,
    }
}

/// A terminal status closes the goal (persist `closed_at`, notify); Active/Paused
/// leave it open.
pub fn is_terminal(status: GoalStatus) -> bool {
    !matches!(status, GoalStatus::Active | GoalStatus::Paused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_is_the_only_achieved() {
        assert_eq!(
            goal_status_from_completion(CompletionState::Accepted),
            GoalStatus::Achieved
        );
        for s in [
            CompletionState::Rejected,
            CompletionState::Escalate,
            CompletionState::NeedsReview,
            CompletionState::PartiallyComplete,
        ] {
            assert_ne!(goal_status_from_completion(s), GoalStatus::Achieved);
        }
    }

    #[test]
    fn rejected_is_unmet_not_achieved() {
        // jury Test-C: a rejected verdict never reaches `achieved`.
        assert_eq!(
            goal_status_from_completion(CompletionState::Rejected),
            GoalStatus::Unmet
        );
    }

    #[test]
    fn partial_keeps_goal_active_and_non_terminal() {
        assert_eq!(
            goal_status_from_completion(CompletionState::PartiallyComplete),
            GoalStatus::Active
        );
        assert!(!is_terminal(GoalStatus::Active));
        assert!(!is_terminal(GoalStatus::Paused));
        assert!(is_terminal(GoalStatus::Achieved));
        assert!(is_terminal(GoalStatus::Blocked));
    }
}
