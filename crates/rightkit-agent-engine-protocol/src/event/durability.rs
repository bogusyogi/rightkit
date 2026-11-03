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
