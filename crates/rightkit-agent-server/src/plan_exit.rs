//! Daemon bridge for the `exit_plan_mode` tool: emits `PlanPresented`, blocks on
//! a per-session oneshot until the operator decides, and on approval flips the
//! session out of Plan mode (into AcceptEdits) via the live permission-mode
//! channel.

use crate::api::lock_recover;
use crate::api::now_iso;
use rightkit_agent_engine_core::Conduit;
use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_agent_policy::PermissionMode;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static PLAN_SEQ: AtomicU64 = AtomicU64::new(0);

/// What the operator decided about a presented plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOutcome {
    /// Leave Plan mode and keep the conversation.
    Approve,
    /// Leave Plan mode and continue from the plan alone: the earlier conversation is dropped from the
    /// model's context (A209-2).
    ApproveClearContext,
    /// Stay in Plan mode.
    Reject,
}

impl PlanOutcome {
    /// The outcome a `resolve-plan` body asks for: `approved` plus the optional `clearContext`.
    pub fn from_request(approved: bool, clear_context: bool) -> Self {
        match (approved, clear_context) {
            (false, _) => Self::Reject,
            (true, true) => Self::ApproveClearContext,
            (true, false) => Self::Approve,
        }
    }

    pub fn is_approved(self) -> bool {
        !matches!(self, Self::Reject)
    }
}

/// Pending plan-approval oneshots keyed by plan_id (resolved by the route).
pub type PendingPlans = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<PlanOutcome>>>>;

pub struct DaemonPlanExit {
    pub session_id: String,
    pub conduit: Arc<Conduit>,
    pub pending: PendingPlans,
    pub permission_mode_tx: tokio::sync::watch::Sender<PermissionMode>,
    /// The plan text of an "approve and clear context" decision, until the engine takes it.
    pub context_reset: Arc<Mutex<Option<String>>>,
}

impl rightkit_agent_coding_tools::PlanExit for DaemonPlanExit {
    fn present(
        &self,
        plan: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send>> {
        let session_id = self.session_id.clone();
        let conduit = self.conduit.clone();
        let pending = self.pending.clone();
        let mode_tx = self.permission_mode_tx.clone();
        let context_reset = self.context_reset.clone();
        Box::pin(async move {
            let plan_id = format!(
                "plan-{}-{}",
                now_iso(),
                PLAN_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let (tx, rx) = tokio::sync::oneshot::channel::<PlanOutcome>();
            lock_recover(&pending, "pending").insert(plan_id.clone(), tx);

            let seq = conduit.next_seq();
            conduit.publish(ProtocolEvent::PlanPresented {
                session_id,
                seq,
                created_at: now_iso(),
                plan_id,
                plan: plan.clone(),
            });

            let outcome = rx
                .await
                .map_err(|_| "plan approval was cancelled".to_string())?;
            if outcome.is_approved() {
                // Leave Plan mode so the agent can start making changes.
                if mode_tx.send(PermissionMode::AcceptEdits).is_err() {
                    tracing::debug!("plan approved after the session's mode channel closed");
                }
            }
            if outcome == PlanOutcome::ApproveClearContext {
                *lock_recover(&context_reset, "plan.context_reset") = Some(plan);
            }
            Ok(outcome.is_approved())
        })
    }

    fn take_context_reset(&self) -> Option<String> {
        lock_recover(&self.context_reset, "plan.context_reset").take()
    }

    fn context_reset_pending(&self) -> bool {
        lock_recover(&self.context_reset, "plan.context_reset").is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::PlanOutcome;

    #[test]
    fn a_resolve_plan_body_maps_to_one_outcome() {
        assert_eq!(PlanOutcome::from_request(false, false), PlanOutcome::Reject);
        assert_eq!(PlanOutcome::from_request(false, true), PlanOutcome::Reject);
        assert_eq!(PlanOutcome::from_request(true, false), PlanOutcome::Approve);
        assert_eq!(
            PlanOutcome::from_request(true, true),
            PlanOutcome::ApproveClearContext
        );
        assert!(PlanOutcome::ApproveClearContext.is_approved());
        assert!(!PlanOutcome::Reject.is_approved());
    }
}
