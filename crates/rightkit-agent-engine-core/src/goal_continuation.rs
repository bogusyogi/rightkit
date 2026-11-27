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

pub(crate) struct GoalContinuationUpdate {
    pub(crate) decision: ContinueDecision,
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
        snapshot: gs.snapshot(),
    })
}
