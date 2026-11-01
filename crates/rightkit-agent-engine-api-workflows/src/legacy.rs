//! Cycle-free compatibility types for legacy authenticated workflow delivery.

use serde_json::Value;

/// Verified legacy webhook material handed off after HMAC, replay, body-size,
/// workspace, and trust admission. This intentionally contains no secret.
#[derive(Debug, Clone)]
pub struct LegacyTriggerDelivery {
    pub trigger_id: String,
    pub provider: String,
    pub delivery_id: String,
    pub event_type: Option<String>,
    pub payload: Value,
    /// Bounded raw JSON is supplied solely for same-transaction redacted
    /// artifact persistence; normalized event data remains separate.
    pub raw_payload: Value,
    pub workspace_root: String,
    pub principal: String,
    pub trust_level: String,
    pub content_hash: String,
    pub signature_hash: String,
    pub received_at: String,
}

/// Explicit compatibility result. Callers must never treat a mapped workflow
/// trigger as a reason to fall through into legacy session execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyWorkflowAdmission {
    NotMapped,
    Shadow { reason: String },
    CutOver,
    CoordinatorUnavailable { reason: String },
}
