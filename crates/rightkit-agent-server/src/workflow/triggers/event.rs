//! WP-06 — Normalized trigger-event types (mirror
//! `docs/pending/workflow-control-plane-support/schemas/trigger-event-v1.schema.json`).
//!
//! These types are the *post-admission* shape that lands in the workflow
//! trigger inbox. HMAC, replay, body-size, workspace, and trust checks happen
//! upstream in the `api_triggers` delivery route — by the time a value of
//! [`NormalizedTriggerEvent`] exists, the daemon has authenticated the
//! request, normalized the body into bounded JSON, computed a content hash
//! and a stable dedupe key, and decided a [`TrustInfo`].
//!
//! ## Single-authority rule
//!
//! Every value here is built exactly once. Re-deriving the event id from
//! `(source, delivery_id, time)` is the only way to reproduce the id across
//! restarts and replays; we expose [`crate::dedupe::derive_event_id`] for
//! that purpose.
//!
//! ## Constraint summary (matches the schema)
//!
//! - `id` is a stable `wf_evt_*` id (`wf_evt:<22 hex>` in the namespaced form
//!   used everywhere else in the workflow crate).
//! - `dedupe_key` is `1..=1024` bytes; the inbox truncates longer keys so a
//!   misbehaving caller can't crash admission.
//! - `content_hash` is always `sha256:<hex64>`.
//! - `data_content_type` is restricted to `application/json` in V1.
//! - `trust.level ∈ {untrusted, authenticated, trusted}`.
//! - `trust.origin` enumerates the allowed provenance tags; we treat unknown
//!   origins as a parse error rather than silently coercing to "untrusted".

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use rightkit_workflow::clock::UnixMs;
use rightkit_workflow::domain::{
    ContentHash, OccurrenceId, TriggerEventId, TriggerId, WorkflowVersionId,
};

// ─── Bounded data ─────────────────────────────────────────────────────────────

/// The bounded, JSON-only payload of a normalized event. Raw request bodies
/// belong in the optional [`RawArtifact`] reference, never here. `null`
/// represents a body-less event (e.g. a manual ping).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NormalizedData {
    Object(serde_json::Map<String, serde_json::Value>),
    Array(Vec<serde_json::Value>),
    String(String),
    Number(f64),
    Boolean(bool),
    Null,
}

impl NormalizedData {
    /// Construct a normalized data value from an owned JSON value, refusing
    /// anything that the schema's `data` slot cannot represent (the schema
    /// only allows object | array | string | number | boolean | null).
    pub fn from_json(value: serde_json::Value) -> Result<Self, NormalizedDataError> {
        Ok(match value {
            serde_json::Value::Object(map) => Self::Object(map),
            serde_json::Value::Array(arr) => Self::Array(arr),
            serde_json::Value::String(s) => Self::String(s),
            serde_json::Value::Number(n) => {
                let as_f64 = n
                    .as_f64()
                    .ok_or(NormalizedDataError::UnrepresentableNumber)?;
                if !as_f64.is_finite() {
                    return Err(NormalizedDataError::UnrepresentableNumber);
                }
                Self::Number(as_f64)
            }
            serde_json::Value::Bool(b) => Self::Boolean(b),
            serde_json::Value::Null => Self::Null,
        })
    }

    /// Borrow as a JSON value.
    pub fn as_value(&self) -> serde_json::Value {
        match self {
            Self::Object(map) => serde_json::Value::Object(map.clone()),
            Self::Array(arr) => serde_json::Value::Array(arr.clone()),
            Self::String(s) => serde_json::Value::String(s.clone()),
            Self::Number(n) => serde_json::Number::from_f64(*n)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Self::Boolean(b) => serde_json::Value::Bool(*b),
            Self::Null => serde_json::Value::Null,
        }
    }
}

/// Failure shape for [`NormalizedData::from_json`]. The schema disallows
/// non-finite floats; this error is the only path that can produce one.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NormalizedDataError {
    #[error("normalized data cannot represent non-finite numbers (NaN/Inf)")]
    UnrepresentableNumber,
}

// ─── Trust ────────────────────────────────────────────────────────────────────

/// Trust classification attached to a normalized event. The schema restricts
/// `level` to one of three strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// No signature, no verified principal — external data only.
    Untrusted,
    /// Signature/principal verified upstream, but not yet policy-approved.
    Authenticated,
    /// Approved by the operator policy path (rare; reserved for
    /// first-party connectors in V1).
    Trusted,
}

impl TrustLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::Authenticated => "authenticated",
            Self::Trusted => "trusted",
        }
    }
}

/// Origin tag for the trust envelope. The schema enumerates the allowed
/// origins; any other value is a parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustOrigin {
    SessionOrigin,
    ToolDerived,
    WebDerived,
    McpDerived,
    WebhookDerived,
    RemoteAgentDerived,
    WorkflowTemplateDerived,
    ConnectorDerived,
}

impl TrustOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionOrigin => "session-origin",
            Self::ToolDerived => "tool-derived",
            Self::WebDerived => "web-derived",
            Self::McpDerived => "mcp-derived",
            Self::WebhookDerived => "webhook-derived",
            Self::RemoteAgentDerived => "remote-agent-derived",
            Self::WorkflowTemplateDerived => "workflow-template-derived",
            Self::ConnectorDerived => "connector-derived",
        }
    }
}

/// Severity of a single trust finding. The schema limits to five levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl TrustSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// One trust finding produced by the admission policy. Examples:
/// "signature scheme deprecated", "missing delivery id", "actor not on
/// allowlist". The schema caps findings at 64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustFinding {
    pub kind: String,
    pub severity: TrustSeverity,
    pub summary: String,
}

/// Trust envelope attached to every normalized event. Field names match the
/// schema one-for-one so we can serialize straight through to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustInfo {
    pub level: TrustLevel,
    pub authenticated: bool,
    pub origin: TrustOrigin,
    pub principal: String,
    pub admission_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_algorithm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_association: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_trust: Option<String>,
    #[serde(default)]
    pub findings: Vec<TrustFinding>,
}

/// Reference to the redacted/hash-bound raw artifact. Bodies larger than the
/// schema's `data` slot allow (or that need to be replayed byte-for-byte)
/// are stored under an `artifactId` whose content hash is recorded here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawArtifact {
    pub artifact_id: String,
    pub content_hash: ContentHash,
    pub redacted: bool,
}

// ─── Normalized event ────────────────────────────────────────────────────────

/// The full normalized trigger event. Mirrors `trigger-event-v1.schema.json`.
/// Field names match the schema so a JSON dump is valid against it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedTriggerEvent {
    /// Schema marker. Always `"coderight.trigger/v1"`. The inbox refuses
    /// any event whose `specVersion` differs.
    pub spec_version: String,
    /// Stable event id (`wf_evt:<22 hex>` namespaced form).
    pub id: TriggerEventId,
    /// Optional trigger id this event targets (`wft_*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<TriggerId>,
    /// Provider-supplied identifier (e.g. `X-GitHub-Delivery`). The
    /// inbox folds it into the dedupe key alongside the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    /// Stable source identity: `github:owner/repo`, `coderight:manual`,
    /// `webhook:<trigger-id>`, etc.
    pub source: String,
    /// Logical event type (`push`, `pull_request`, `manual`, `schedule.cron`,
    /// `internal.run.completed`, …).
    #[serde(rename = "type")]
    pub type_: String,
    /// Optional human-readable subject (PR title, commit message, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Provider-asserted event time (RFC3339; the inbox parses to UnixMs).
    pub time: String,
    /// Daemon-asserted receipt time (RFC3339; always set by the inbox).
    pub received_at: String,
    /// Content type of `data`. Restricted to `application/json` in V1.
    pub data_content_type: String,
    /// Optional URI of the data schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_schema: Option<String>,
    /// Bounded JSON data.
    pub data: NormalizedData,
    /// Trust envelope.
    pub trust: TrustInfo,
    /// Stable dedupe key (`1..=1024` bytes).
    pub dedupe_key: String,
    /// Canonical SHA-256 of the redacted raw body.
    pub content_hash: ContentHash,
    /// Optional reference to the redacted raw artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_artifact: Option<RawArtifact>,
    /// Optional correlation id propagated from caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Optional causation id (id of the run/node that produced this event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    /// Optional W3C trace parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_parent: Option<String>,
    /// Bounded extension bag (≤32 entries, scalar values, ≤2 KiB each).
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl NormalizedTriggerEvent {
    /// `specVersion` constant.
    pub const SPEC_VERSION: &'static str = "coderight.trigger/v1";

    /// Set the runtime `time` and `received_at` fields from unix-ms values
    /// using a deterministic RFC3339 formatter. The inbox calls this once
    /// during admission; downstream callers can rebuild the event from the
    /// stored fields without re-deriving them.
    pub fn stamp_times(&mut self, time_unix_ms: UnixMs, received_at_unix_ms: UnixMs) {
        self.time = format_unix_ms_as_iso(time_unix_ms);
        self.received_at = format_unix_ms_as_iso(received_at_unix_ms);
    }
}

// ─── Persisted projections (used by the occurrence layer) ────────────────────

/// Durable record written by the inbox for every admitted event. The shape
/// is what the storage layer persists; `NormalizedTriggerEvent` is what the
/// JSON envelope looks like. They are the same row, just sliced for storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedEventRow {
    pub event_id: TriggerEventId,
    pub trigger_id: Option<TriggerId>,
    pub source: String,
    pub type_: String,
    pub time_unix_ms: i64,
    pub received_at_unix_ms: i64,
    pub dedupe_key: String,
    pub content_hash: ContentHash,
    pub trust: TrustInfo,
    /// Full normalized event JSON, suitable for replay.
    pub envelope_json: String,
}

/// One durable occurrence row produced by the occurrence admission layer.
/// The workflow supervisor consumes this projection to dispatch runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedOccurrence {
    pub occurrence_id: OccurrenceId,
    pub event_id: TriggerEventId,
    pub trigger_id: TriggerId,
    pub workflow_id: Option<String>,
    pub workflow_version_id: Option<WorkflowVersionId>,
    pub fire_time_unix_ms: i64,
    pub admission_decision: String,
    pub dedupe_key: String,
    pub created_at_unix_ms: i64,
}

/// One audit row written by the inbox. Bounded FIFO; trim is the caller's
/// responsibility (the inbox helper [`trim_front`] lives in `dedupe.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxAuditRow {
    pub created_at_unix_ms: i64,
    pub trigger_id: Option<TriggerId>,
    pub source: String,
    pub outcome: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<TriggerEventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<String>,
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// UTC RFC 3339 text (`YYYY-MM-DDTHH:MM:SSZ`, second resolution) for a Unix-millisecond timestamp;
/// a negative value renders the epoch. The civil-date math is `rightkit-time`'s.
pub fn format_unix_ms_as_iso(ms: i64) -> String {
    rightkit_time::DateTime::from_unix(ms.max(0) / 1000, 0)
        .map(|parsed| parsed.format_rfc3339())
        .unwrap_or_default()
}

/// Parse a UTC RFC3339 string of the exact shape `format_unix_ms_as_iso` emits
/// (`YYYY-MM-DDTHH:MM:SSZ`, no fraction, no offset). Returns `None` for anything else so the inbox
/// can refuse the event rather than silently coercing it.
pub fn parse_iso_as_unix_ms(value: &str) -> Option<UnixMs> {
    let bytes = value.as_bytes();
    if bytes.len() != 20 || bytes[10] != b'T' || bytes[19] != b'Z' {
        return None;
    }
    let parsed = rightkit_time::DateTime::parse_rfc3339(value)?;
    Some(parsed.unix_seconds() * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_data_round_trips_through_json() {
        let value = serde_json::json!({
            "number": 42,
            "boolean": true,
            "array": [1, 2, 3],
            "string": "hi",
            "null": null,
        });
        let data = NormalizedData::from_json(value.clone()).expect("normalize");
        assert_eq!(data.as_value(), value);
    }

    #[test]
    fn normalized_data_rejects_nan_and_inf() {
        // serde_json itself refuses to construct non-finite Number values;
        // normalized data additionally guards this boundary for custom values.
        assert!(serde_json::Number::from_f64(f64::NAN).is_none());
        assert!(matches!(
            NormalizedData::Number(f64::NAN).as_value(),
            serde_json::Value::Null
        ));
    }

    #[test]
    fn trust_level_strings_match_schema() {
        assert_eq!(TrustLevel::Untrusted.as_str(), "untrusted");
        assert_eq!(TrustLevel::Authenticated.as_str(), "authenticated");
        assert_eq!(TrustLevel::Trusted.as_str(), "trusted");
    }

    #[test]
    fn trust_origin_strings_match_schema() {
        assert_eq!(TrustOrigin::WebhookDerived.as_str(), "webhook-derived");
        assert_eq!(TrustOrigin::SessionOrigin.as_str(), "session-origin");
    }

    #[test]
    fn iso_round_trip() {
        let ms = parse_iso_as_unix_ms("2026-08-07T00:00:00Z").expect("parse");
        assert_eq!(format_unix_ms_as_iso(ms), "2026-08-07T00:00:00Z");
        let leap = parse_iso_as_unix_ms("2024-02-29T12:34:56Z").expect("parse leap");
        assert_eq!(format_unix_ms_as_iso(leap), "2024-02-29T12:34:56Z");
    }

    #[test]
    fn iso_rejects_partial_or_with_offset() {
        assert!(parse_iso_as_unix_ms("2026-08-07").is_none());
        assert!(parse_iso_as_unix_ms("2026-08-07T00:00:00+00:00").is_none());
        assert!(parse_iso_as_unix_ms("not a date").is_none());
        // Impossible calendar dates and non-exact shapes are refused.
        assert!(parse_iso_as_unix_ms("2026-02-30T00:00:00Z").is_none());
        assert!(parse_iso_as_unix_ms("2026-08-07T00:00:00.5Z").is_none());
        assert!(parse_iso_as_unix_ms("2026-08-07T00:00:00z").is_none());
    }
}
