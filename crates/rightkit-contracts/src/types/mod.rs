//! Typed views of the six contracts. Each struct deserialises a document that
//! has already passed schema and semantic validation; `deny_unknown_fields`
//! keeps the typed view as strict as the schema.

use serde::{Deserialize, Serialize};

pub mod content_brief;
pub mod daemon_api;
pub mod delivery_envelope;
pub mod demo_plan;
pub mod record_event;
pub mod record_session;

/// `channel-memory` v1 as the contracts crate sees it: the schema-validated document, untyped.
/// The typed model and the semantic rules (proof, conflict and supersession rules) live in
/// `rightkit-channel-memory`; this view exists so the document can be validated and negotiated
/// here without a dependency on that crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelMemory(pub serde_json::Value);

pub use content_brief::ContentBrief;
pub use daemon_api::DaemonApi;
pub use delivery_envelope::DeliveryEnvelope;
pub use demo_plan::DemoPlan;
pub use record_event::RecordEvent;
pub use record_session::RecordSession;

/// A rectangle in points, as used by rightkit-qa-driver and the recorder.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A blake3 content address, written `blake3:<64 lowercase hex>` on the wire.
pub type Blake3 = String;
