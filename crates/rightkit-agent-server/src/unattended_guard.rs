//! Unattended turns (scheduler contract R41, headless R8): a scheduled or webhook turn has no
//! human attached, so the two interactions an approval card does not cover are answered at once
//! instead of waiting for a reply that cannot come.
//!
//! * an `ask_user` question is declined (`{"declined": true, "reason": "unattended"}`) and the
//!   turn goes on;
//! * a plan card is denied and the turn is interrupted: the run ends as `interaction_required`.
//!
//! An approval request needs nothing here: the session runs under `auto-safe` and the engine
//! denies a gated call at once as `unattended`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::api::{lock_recover, DaemonState};
use rightkit_agent_engine_core::conduit::ConduitReceive;
use rightkit_agent_engine_protocol::ProtocolEvent;

/// Watch one unattended session until its first turn finishes. Call it after the session exists
/// and before the prompt is submitted. The returned flag turns true when a plan card ended the
/// run, so the caller can record `interaction_required` for it.
pub(crate) fn watch_unattended_session(state: &DaemonState, session_id: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let Some(live) = state.get_session(session_id) else {
        return flag;
    };
    let mut subscription = live.conduit.subscribe();
    let state = state.clone();
    let session_id = session_id.to_string();
    let seen_plan = flag.clone();
    tokio::spawn(async move {
        loop {
            match subscription.recv().await {
                ConduitReceive::Event(ProtocolEvent::UserQuestionAsked { question_id, .. }) => {
                    if let Some(live) = state.get_session(&session_id) {
                        let sender = lock_recover(&live.pending_questions, "pending_questions")
                            .remove(&question_id);
                        if let Some(sender) = sender {
                            let _ = sender.send(
                                serde_json::json!({ "declined": true, "reason": "unattended" }),
                            );
                        }
                    }
                }
                ConduitReceive::Event(ProtocolEvent::PlanPresented { plan_id, .. }) => {
                    seen_plan.store(true, Ordering::SeqCst);
                    if let Some(live) = state.get_session(&session_id) {
                        let sender =
                            lock_recover(&live.pending_plans, "pending_plans").remove(&plan_id);
                        if let Some(sender) = sender {
                            let _ = sender.send(crate::plan_exit::PlanOutcome::Reject);
                        }
                        if let Some(interrupt) = &live.interrupt_tx {
                            let _ = interrupt.send(true);
                        }
                    }
                }
                ConduitReceive::Event(ProtocolEvent::SessionTurnFinished {
                    session_id: finished,
                    ..
                }) if finished == session_id => return,
                ConduitReceive::Event(_) | ConduitReceive::Lagged { .. } => continue,
                ConduitReceive::Closed => return,
            }
        }
    });
    flag
}
