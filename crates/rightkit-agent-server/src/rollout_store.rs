//! Live registry of in-progress model rollouts. A promotion starts a canary
//! rollout here; each subsequent run for that role feeds a success sample that
//! advances the rollout toward Full or trips an auto-rollback. In-memory: a
//! rollout is short-lived, and if the daemon restarts the canary model simply
//! stays in place (already written to config).

use crate::api::lock_recover;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rightkit_agent_orchestration::coding::{Rollout, RolloutConfig, RolloutEvent, RolloutStage};

#[derive(Clone, Default)]
pub struct RolloutStore {
    active: Arc<Mutex<HashMap<String, Rollout>>>,
}

impl RolloutStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a canary rollout for `role` from `from_model` to `to_model`.
    pub fn start(&self, role: &str, from_model: &str, to_model: &str, baseline_rate: f64) {
        lock_recover(&self.active, "self.active").insert(
            role.to_string(),
            Rollout::start(
                role,
                from_model,
                to_model,
                baseline_rate,
                RolloutConfig::default(),
            ),
        );
    }

    /// Feed a live result. Returns the event plus a snapshot, and removes the
    /// rollout once it reaches a terminal stage (Full or RolledBack).
    pub fn record(&self, role: &str, success: bool) -> Option<(RolloutEvent, Rollout)> {
        let mut g = lock_recover(&self.active, "self.active");
        let r = g.get_mut(role)?;
        let ev = r.record(success);
        let snap = r.clone();
        let terminal = matches!(ev, RolloutEvent::RolledBack)
            || matches!(ev, RolloutEvent::Advanced(RolloutStage::Full));
        if terminal {
            g.remove(role);
        }
        Some((ev, snap))
    }

    /// Active rollouts as JSON (for `GET /runtime/rollouts`).
    pub fn json(&self) -> Vec<serde_json::Value> {
        self.active
            .lock()
            .unwrap()
            .values()
            .map(|r| {
                serde_json::json!({
                    "role": r.role,
                    "fromModel": r.from_model,
                    "toModel": r.to_model,
                    "stage": r.stage.as_str(),
                    "baselineRate": r.baseline_rate,
                })
            })
            .collect()
    }
}
