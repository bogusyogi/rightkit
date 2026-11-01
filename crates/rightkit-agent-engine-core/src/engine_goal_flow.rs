use super::*;

impl SessionEngine {
    /// Share the goal Arc so the API layer can update the persistent goal text.
    pub fn goal_arc(&self) -> Arc<Mutex<Option<String>>> {
        self.goal.clone()
    }

    /// Replace the engine's internal goal Arc with a shared one from the API layer.
    pub fn set_goal_arc(&mut self, goal: Arc<Mutex<Option<String>>>) {
        self.goal = goal;
    }

    /// Share the budget-bounded goal-state container (F2). The daemon sets/clears
    /// the inner `GoalState` live and owns writes to `tokens_used`/`cost_usd`; the
    /// engine only reads it at turn-end.
    pub fn set_goal_state_arc(
        &mut self,
        goal_state: Arc<Mutex<Option<crate::goal_state::GoalState>>>,
    ) {
        self.goal_state = goal_state;
    }

    /// Share the goal-state container outward (so the daemon holds the same Arc).
    pub fn goal_state_arc(&self) -> Arc<Mutex<Option<crate::goal_state::GoalState>>> {
        self.goal_state.clone()
    }

    /// Toggle the goal continuation loop (the global kill switch).
    pub fn set_continuation_enabled(&mut self, enabled: bool) {
        self.continuation_enabled = enabled;
    }

    /// F2 turn-end continuation. Returns true iff a continue-toward-goal nudge
    /// was injected and the loop should re-enter; false to stop (close / pause /
    /// no goal). A goal-less or disabled session returns false immediately, so
    /// its behavior is byte-identical to the pre-F2 path.
    pub(super) fn maybe_continue_toward_goal(&mut self, progressed: bool, sid: &str) -> bool {
        if !self.continuation_enabled {
            return false;
        }
        // User input wins: never continue while a steer/user turn is queued.
        let pending = self
            .steer_rx
            .as_ref()
            .map(|rx| !rx.is_empty())
            .unwrap_or(false);
        if pending {
            return false;
        }

        let Some(update) = update_goal_state(&self.goal_state, progressed) else {
            return false; // no active goal — identical to the pre-F2 path
        };

        self.emit(|seq| ProtocolEvent::GoalProgressUpdated {
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            snapshot: update.snapshot,
        });

        match update.decision {
            crate::goal_state::ContinueDecision::Continue => {
                self.transcript.push_user(GOAL_CONTINUE_NUDGE);
                self.emit(|seq| ProtocolEvent::HumanSteeringInjected {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    injected_by: "goal_continuation".into(),
                    content_preview: GOAL_CONTINUE_NUDGE.chars().take(80).collect(),
                    text: Some(crate::engine::engine_loop::bound_interjection_text(
                        GOAL_CONTINUE_NUDGE,
                    )),
                });
                true
            }
            // Close / Stop: leave the terminal status on the shared GoalState; the
            // daemon (watching the conduit) persists it + notifies. The engine
            // never marks `achieved` — only the jury can.
            _ => false,
        }
    }
}
