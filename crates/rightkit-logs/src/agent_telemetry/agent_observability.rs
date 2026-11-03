//! Background-agent observability (FM-086).
//!
//! A bounded, in-process record of background-agent (subagent/worker)
//! lifecycle: when one was spawned, how it ended, and how long it ran.
//! Mirrors the `evidence` module's sink pattern exactly (bounded push,
//! `record_*`/`*_observations`/`reset_*` triple) so both modules read the
//! same way. This type never manufactures a measurement -- a caller that
//! never records an observation simply has none; nothing here estimates or
//! infers agent activity that was not explicitly reported.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

pub const AGENT_OBSERVABILITY_SCHEMA: &str = "coderight.agent-observability.v1";
const MAX_ROWS: usize = 4096;

/// How a background agent's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutcome {
    Completed,
    Cancelled,
    Failed,
}

/// One background-agent run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentObservationV1 {
    pub schema_version: String,
    pub agent_id: String,
    pub session_id: String,
    pub spawned_at_unix_ms: u64,
    pub ended_at_unix_ms: u64,
    pub outcome: AgentOutcome,
    /// Present only when `outcome` is `Failed`.
    pub failure_reason: Option<String>,
}

impl AgentObservationV1 {
    /// Construct an observation ending in [`AgentOutcome::Completed`] or
    /// [`AgentOutcome::Cancelled`] -- outcomes that carry no failure reason.
    pub fn ended(
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        spawned_at_unix_ms: u64,
        ended_at_unix_ms: u64,
        outcome: AgentOutcome,
    ) -> Self {
        Self {
            schema_version: AGENT_OBSERVABILITY_SCHEMA.into(),
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            spawned_at_unix_ms,
            ended_at_unix_ms,
            outcome,
            failure_reason: None,
        }
    }

    /// Construct a [`AgentOutcome::Failed`] observation with its reason.
    pub fn failed(
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        spawned_at_unix_ms: u64,
        ended_at_unix_ms: u64,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: AGENT_OBSERVABILITY_SCHEMA.into(),
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            spawned_at_unix_ms,
            ended_at_unix_ms,
            outcome: AgentOutcome::Failed,
            failure_reason: Some(reason.into()),
        }
    }

    /// Run duration. `None` when `ended_at_unix_ms` precedes
    /// `spawned_at_unix_ms` -- a genuinely inconsistent input, not something
    /// this type silently clamps to zero.
    pub fn duration_ms(&self) -> Option<u64> {
        self.ended_at_unix_ms.checked_sub(self.spawned_at_unix_ms)
    }
}

#[derive(Debug, Default)]
struct AgentObservabilitySink {
    observations: Vec<AgentObservationV1>,
}

static SINK: Mutex<Option<AgentObservabilitySink>> = Mutex::new(None);

fn with_sink<R>(f: impl FnOnce(&mut AgentObservabilitySink) -> R) -> R {
    // A poisoned telemetry mutex must not turn a background-agent failure
    // into a daemon panic. The surviving value is still a conservative
    // observation boundary: continue with it and let callers see whatever
    // records remain available.
    let mut guard = SINK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(guard.get_or_insert_with(AgentObservabilitySink::default))
}

pub fn record_agent_observation(event: AgentObservationV1) {
    tracing::info!(
        operation = "agent_observability",
        schema_version = %event.schema_version,
        agent_id = %event.agent_id,
        session_id = %event.session_id,
        outcome = ?event.outcome,
        "background agent observation"
    );
    with_sink(|sink| {
        if sink.observations.len() >= MAX_ROWS {
            sink.observations.remove(0);
        }
        sink.observations.push(event);
    });
}

pub fn agent_observations() -> Vec<AgentObservationV1> {
    with_sink(|sink| sink.observations.clone())
}

/// Observations for one session, in recorded order.
pub fn agent_observations_for_session(session_id: &str) -> Vec<AgentObservationV1> {
    with_sink(|sink| {
        sink.observations
            .iter()
            .filter(|o| o.session_id == session_id)
            .cloned()
            .collect()
    })
}

pub fn reset_agent_observability() {
    with_sink(|sink| sink.observations.clear());
}
