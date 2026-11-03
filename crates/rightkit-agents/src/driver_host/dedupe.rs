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
