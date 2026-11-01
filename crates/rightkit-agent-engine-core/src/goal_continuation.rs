//! Turn-end goal-continuation state transitions.
//!
//! `SessionEngine` owns event emission and transcript mutation; this module owns
//! the shared `GoalState` update so the large agent loop does not also encode
//! continuation policy.

use crate::goal_state::{ContinueDecision, GoalState};
use std::sync::{Arc, Mutex};

/// Injected as a user turn when the goal continuation loop decides to keep going.
/// Phrased to avoid undo-churn (council P2): only act if the goal is truly unmet.
pub(crate) const GOAL_CONTINUE_NUDGE: &str =
    "Continue toward the goal. Act ONLY if the goal is genuinely unmet — do not \
     redo or undo work that is already complete. If the goal is fully achieved, \
     say so and stop.";

#[allow(dead_code)]
pub(crate) struct GoalContinuationUpdate {
    pub(crate) decision: ContinueDecision,
    pub(crate) goal_id: String,
    pub(crate) status: String,
    pub(crate) tokens_used: i64,
    pub(crate) budget_tokens: i64,
    pub(crate) continuation_count: i64,
    pub(crate) no_progress_count: i64,
    pub(crate) snapshot: rightkit_agent_engine_protocol::event::GoalSnapshotV1,
}

pub(crate) fn update_goal_state(
    goal_state: &Arc<Mutex<Option<GoalState>>>,
    progressed: bool,
) -> Option<GoalContinuationUpdate> {
    let mut guard = goal_state.lock().unwrap();
    let gs = guard.as_mut()?;
    if progressed {
        gs.no_progress_count = 0;
    } else {
        gs.no_progress_count += 1;
    }
    let decision = gs.may_continue();
    match &decision {
        ContinueDecision::Continue => gs.continuation_count += 1,
        ContinueDecision::Close(status) => gs.status = status.clone(),
        ContinueDecision::Stop(_) => {}
    }
    Some(GoalContinuationUpdate {
        decision,
        goal_id: gs.id.clone(),
        status: gs.status.clone(),
        tokens_used: gs.tokens_used,
        budget_tokens: gs.budget_tokens,
        continuation_count: gs.continuation_count,
        no_progress_count: gs.no_progress_count,
        snapshot: gs.snapshot(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal_state(
        continuation_count: i64,
        no_progress_count: i64,
    ) -> Arc<Mutex<Option<GoalState>>> {
        Arc::new(Mutex::new(Some(GoalState {
            id: "goal-1".into(),
            text: "ship".into(),
            status: "active".into(),
            budget_usd: 0.0,
            budget_tokens: 1_000_000,
            tokens_used: 0,
            cost_usd: 0.0,
            continuation_count,
            no_progress_count,
            gds_score: 0.0,
            verification_present: false,
            verification_approved: false,
            started_at: "t0".into(),
            closed_at: None,
            dismissed_at: None,
            decomposition_state: "missing".into(),
            plan_revision: 0,
            now: None,
            next: None,
            steps: Vec::new(),
            verification_state: "unverified".into(),
        })))
    }

    #[test]
    fn update_goal_state_counts_progress_and_continuation() {
        let state = goal_state(0, 2);

        let update = update_goal_state(&state, true).expect("active goal should update");

        assert_eq!(update.decision, ContinueDecision::Continue);
        assert_eq!(update.continuation_count, 1);
        assert_eq!(update.no_progress_count, 0);
    }

    #[test]
    fn update_goal_state_closes_blocked_after_no_progress_limit() {
        let state = goal_state(0, 2);

        let update = update_goal_state(&state, false).expect("active goal should update");

        assert_eq!(update.decision, ContinueDecision::Close("blocked".into()));
        assert_eq!(update.status, "blocked");
        assert_eq!(
            state.lock().unwrap().as_ref().expect("goal remains").status,
            "blocked"
        );
    }

    #[test]
    fn update_goal_state_returns_none_without_active_goal() {
        let state = Arc::new(Mutex::new(None));

        assert!(update_goal_state(&state, true).is_none());
    }
}
