//! Host-side H9/H10 observation shapes are `rightkit-context-protocol`'s (`host_observation`):
//! one copy of the v1 documents, so persisted rows keep parsing.

pub use rightkit_context_protocol::host_observation::*;

/// The provenance receipt of an H9/H10 record under the name this host uses.
pub use rightkit_context_protocol::host_observation::HostObservationProvenanceV1 as HostReceiptProvenanceV1;
