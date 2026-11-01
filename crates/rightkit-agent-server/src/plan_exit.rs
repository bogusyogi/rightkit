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

/// Pending plan-approval oneshots keyed by plan_id (resolved by the route).
pub type PendingPlans = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>;

pub struct DaemonPlanExit {
    pub session_id: String,
    pub conduit: Arc<Conduit>,
    pub pending: PendingPlans,
    pub permission_mode_tx: tokio::sync::watch::Sender<PermissionMode>,
}

impl rightkit_agent_tools::coding_tools::PlanExit for DaemonPlanExit {
    fn present(
        &self,
        plan: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send>> {
        let session_id = self.session_id.clone();
        let conduit = self.conduit.clone();
        let pending = self.pending.clone();
        let mode_tx = self.permission_mode_tx.clone();
        Box::pin(async move {
            let plan_id = format!(
                "plan-{}-{}",
                now_iso(),
                PLAN_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
            lock_recover(&pending, "pending").insert(plan_id.clone(), tx);

            let seq = conduit.next_seq();
            conduit.publish(ProtocolEvent::PlanPresented {
                session_id,
                seq,
                created_at: now_iso(),
                plan_id,
                plan,
            });

            let approved = rx
                .await
                .map_err(|_| "plan approval was cancelled".to_string())?;
            if approved {
                // Leave Plan mode so the agent can start making changes.
                let _ = mode_tx.send(PermissionMode::AcceptEdits);
            }
            Ok(approved)
        })
    }
}
