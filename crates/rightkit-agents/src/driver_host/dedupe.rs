use crate::driver_host::NormalizedEvent;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupAdmission {
    New,
    Duplicate,
}

/// Exact-once admission boundary. Durable implementations should enforce a unique event-key
/// constraint in the same transaction that advances the persisted driver cursor.
pub trait EventDeduplicator {
    type Error;

    fn admit(&mut self, event: &NormalizedEvent) -> Result<DedupAdmission, Self::Error>;
}

#[derive(Debug, Default)]
pub struct InMemoryEventDeduplicator {
    admitted: HashSet<String>,
}

impl InMemoryEventDeduplicator {
    pub fn len(&self) -> usize {
        self.admitted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.admitted.is_empty()
    }
}

impl EventDeduplicator for InMemoryEventDeduplicator {
    type Error = std::convert::Infallible;

    fn admit(&mut self, event: &NormalizedEvent) -> Result<DedupAdmission, Self::Error> {
        Ok(if self.admitted.insert(event.event_key.clone()) {
            DedupAdmission::New
        } else {
            DedupAdmission::Duplicate
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::{DriverKind, NormalizedEventKind};

    fn event(key: &str, offset: u64) -> NormalizedEvent {
        NormalizedEvent {
            event_key: key.into(),
            driver: DriverKind::Codex,
            source_offset: offset,
            vendor_method: "item/agentMessage/delta".into(),
            kind: NormalizedEventKind::AssistantDelta { text: key.into() },
        }
    }

    #[test]
    fn duplicate_and_reordered_events_are_admitted_exactly_once() {
        let mut dedupe = InMemoryEventDeduplicator::default();
        let second = event("codex:2:delta", 2);
        let first = event("codex:1:delta", 1);

        assert_eq!(dedupe.admit(&second).unwrap(), DedupAdmission::New);
        assert_eq!(dedupe.admit(&first).unwrap(), DedupAdmission::New);
        assert_eq!(dedupe.admit(&second).unwrap(), DedupAdmission::Duplicate);
        assert_eq!(dedupe.admit(&first).unwrap(), DedupAdmission::Duplicate);
        assert_eq!(dedupe.len(), 2);
    }
}
