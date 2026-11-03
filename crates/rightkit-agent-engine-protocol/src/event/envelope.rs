use super::ProtocolEvent;
use crate::TranscriptMessage;
use serde::{Deserialize, Serialize};

pub const EVENT_ENVELOPE_SCHEMA_V1: u16 = 1;

/// Optional causal/cross-runtime correlation carried beside a canonical event.
/// These fields identify relationships only; session/sequence/kind remain owned
/// by `ProtocolEvent` and SQLite rather than being duplicated in the envelope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCorrelationV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causal_parent_event_id: Option<String>,
}

impl EventCorrelationV1 {
    /// Derive the normalized correlation dimensions already carried by the
    /// canonical event. Routing identity (`session_id`, `seq`, kind) remains on
    /// `ProtocolEvent`; this only exposes cross-event relationships without
    /// duplicating the event payload.
    pub fn from_event(event: &ProtocolEvent) -> Self {
        let mut correlation = Self::default();
        match event {
            ProtocolEvent::SessionTurnFinished { turn_id, .. } => {
                correlation.turn_id = Some(turn_id.clone());
            }
            ProtocolEvent::ToolCallStarted { call, .. }
            | ProtocolEvent::ToolApprovalRequested { call, .. } => {
                correlation.tool_call_id = Some(call.id.clone());
            }
            ProtocolEvent::ToolApprovalResolved { tool_call_id, .. }
            | ProtocolEvent::ToolApprovalReviewed { tool_call_id, .. } => {
                correlation.tool_call_id = Some(tool_call_id.clone());
            }
            ProtocolEvent::CredentialRequested { tool_call_id, .. } => {
                correlation.tool_call_id = Some(tool_call_id.clone());
            }
            ProtocolEvent::CredentialResolved { tool_call_id, .. } => {
                correlation.tool_call_id = Some(tool_call_id.clone());
            }
            ProtocolEvent::ToolResultAdded {
                result: TranscriptMessage::Tool { tool_call_id, .. },
                ..
            } => {
                correlation.tool_call_id = Some(tool_call_id.clone());
            }
            ProtocolEvent::TaskStatusUpdated { task_id, .. }
            | ProtocolEvent::TaskArtifactUpdated { task_id, .. }
            | ProtocolEvent::ArtifactLineageUpdated { task_id, .. } => {
                correlation.task_id = Some(task_id.clone());
            }
            ProtocolEvent::SessionContextFolded { subtask_id, .. } => {
                correlation.task_id = Some(subtask_id.clone());
            }
            ProtocolEvent::RunGovernanceRejected {
                tool_call_id,
                budget,
                ownership,
                origin,
                ..
            } => {
                correlation.tool_call_id = Some(tool_call_id.clone());
                correlation.run_id = budget
                    .as_ref()
                    .map(|value| value.run_id.clone())
                    .or_else(|| origin.as_ref().map(|value| value.run_id.clone()))
                    .or_else(|| ownership.as_ref().map(|value| value.target_run_id.clone()));
            }
            _ => {}
        }
        correlation
    }
}

/// Stable metadata wrapper for one durable the host app event.
///
/// `event_id` is deterministically derived from the already-authoritative
/// `(session_id, seq)` pair, so replay/reopen produces the same identity. The
/// event payload remains unchanged and therefore backward-compatible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelopeV1 {
    pub schema_version: u16,
    pub event_id: String,
    #[serde(default)]
    pub correlation: EventCorrelationV1,
    pub event: ProtocolEvent,
}

impl EventEnvelopeV1 {
    /// Build the canonical normalized representation for a durable fact using
    /// correlation already present on the event. Because `event_id` derives
    /// from `(session_id, seq)`, replay/reopen reconstructs the same identity
    /// without a duplicate database column that could drift.
    pub fn canonical(event: ProtocolEvent) -> Result<Self, String> {
        let correlation = EventCorrelationV1::from_event(&event);
        Self::for_durable_event(event, correlation)
    }

    pub fn for_durable_event(
        event: ProtocolEvent,
        correlation: EventCorrelationV1,
    ) -> Result<Self, String> {
        if !event.is_durable_fact() {
            return Err("live projection events do not receive durable event envelopes".into());
        }
        let meta = event.meta();
        if meta.session_id.trim().is_empty() {
            return Err("durable event envelope requires session_id".into());
        }
        if meta.seq < 1 {
            return Err("durable event envelope requires committed sequence >= 1".into());
        }
        Ok(Self {
            schema_version: EVENT_ENVELOPE_SCHEMA_V1,
            event_id: stable_event_id(&meta.session_id, meta.seq),
            correlation,
            event,
        })
    }
}

pub fn stable_event_id(session_id: &str, seq: i64) -> String {
    format!("evt-v1:{session_id}:{seq}")
}
