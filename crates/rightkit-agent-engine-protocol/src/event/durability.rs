use super::ProtocolEvent;

/// Whether an event is canonical replay truth or an ephemeral live projection.
/// Durable facts belong in SQLite and must survive reconnect/replay. Live
/// projections may be dropped/reordered without changing settled session truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventDurability {
    DurableFact,
    LiveProjection,
}

impl ProtocolEvent {
    /// Conservative classification: only events that are unambiguously
    /// presentation/control projections are non-durable. New variants default
    /// to durable by virtue of the explicit catch-all arm.
    pub fn durability(&self) -> EventDurability {
        match self {
            Self::AssistantDelta { .. }
            | Self::ToolOutputDelta { .. }
            | Self::StreamResyncRequired { .. }
            | Self::PersistenceFailed { .. } => EventDurability::LiveProjection,
            _ => EventDurability::DurableFact,
        }
    }

    pub fn is_durable_fact(&self) -> bool {
        self.durability() == EventDurability::DurableFact
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TranscriptMessage;

    #[test]
    fn token_delta_and_resync_are_live_projections() {
        assert_eq!(
            ProtocolEvent::AssistantDelta {
                session_id: "s".into(),
                seq: 1,
                created_at: "t".into(),
                text: "x".into(),
                is_reasoning: false,
            }
            .durability(),
            EventDurability::LiveProjection
        );
        assert_eq!(
            ProtocolEvent::StreamResyncRequired {
                session_id: "s".into(),
                seq: 2,
                created_at: "t".into(),
                after_seq: 0,
                oldest_available_seq: 1,
                high_water_mark: 1,
            }
            .durability(),
            EventDurability::LiveProjection
        );
    }

    #[test]
    fn persistence_failed_is_live_only() {
        let event = ProtocolEvent::PersistenceFailed {
            session_id: "s".into(),
            seq: 0,
            created_at: "t".into(),
            reason: "disk full".into(),
        };
        assert_eq!(event.durability(), EventDurability::LiveProjection);
        assert!(!event.is_durable_fact());
    }

    #[test]
    fn settled_assistant_message_is_durable_truth() {
        let message = TranscriptMessage::Assistant {
            id: "m".into(),
            created_at: "t".into(),
            content: "done".into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            pinned: false,
        };
        assert!(ProtocolEvent::AssistantMessageCompleted {
            session_id: "s".into(),
            seq: 3,
            created_at: "t".into(),
            message,
        }
        .is_durable_fact());
    }
}
