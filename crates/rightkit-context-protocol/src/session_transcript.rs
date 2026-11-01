//! Additive canonical session-transcript append contract (CodeRight → Membrane).
//!
//! CodeRight owns the durable session event log; this module is the wire shape
//! the runtime exporter uses to append committed `ProtocolEvent` projections
//! into Membrane's append-only absorbed-event stream so the existing Adapt
//! normalization/mining pipeline consumes them without a second transport or
//! textual transcript scanning. The request envelope itself lives on
//! `AdaptObservationRequestV1` (`operation: "session_transcript"` /
//! `"session_transcript_forget"`), so these types carry the operation payload
//! only — there is exactly one wire type for this purpose.
//!
//! Ordering discipline: `seq` is the absorbed-stream position assigned by the
//! producer (1-based, contiguous, `expected_stream_seq + 1 + i` within one
//! append); `source_seq` is the event's own CodeRight durable `events.seq`,
//! which is strictly increasing but may skip positions occupied by live
//! projections that are never exported. `event_id` must equal
//! `evt-v1:{session_id}:{source_seq}` — CodeRight's canonical stable event
//! identity — so exact replay is byte-identical and any rewrite surfaces as a
//! conflict rather than a silent fork.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host_observation::HostObservationValidationError;

pub const SESSION_TRANSCRIPT_SCHEMA_VERSION: u32 = 1;
/// The only normalized row contract admitted through the transcript append at
/// this schema version.
pub const SESSION_TRANSCRIPT_EVENT_TYPE: &str = "coderight_event_v1";
/// The only `host` value admitted on the normalized row at this version.
pub const SESSION_TRANSCRIPT_HOST: &str = "coderight";
/// Producer- and worker-side bound on one append batch.
pub const SESSION_TRANSCRIPT_MAX_EVENTS_PER_APPEND: usize = 256;

/// One committed source event projected into the normalized transcript row
/// the worker stores verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTranscriptEventV1 {
    /// 1-based position in the worker-side absorbed stream for this session.
    pub seq: u64,
    /// Position in the producer's durable source log (`events.seq`).
    /// Strictly increasing per stream; gaps reflect non-exported live
    /// projections, never lost events.
    pub source_seq: u64,
    /// Stable identity derived from `(session_id, source_seq)`:
    /// `evt-v1:{session_id}:{source_seq}`.
    pub event_id: String,
    /// Normalized row contract carried by `payload` (`coderight_event_v1`).
    pub event_type: String,
    /// The normalized row object stored verbatim as the stream payload.
    pub payload: Value,
    pub occurred_at_unix_ms: u64,
}

impl SessionTranscriptEventV1 {
    pub fn validate(&self) -> Result<(), HostObservationValidationError> {
        if self.seq == 0 || self.source_seq == 0 {
            return Err(HostObservationValidationError::InvalidField {
                field: "seq/sourceSeq".into(),
                reason: "sequence positions are 1-based".into(),
            });
        }
        if self.event_id.trim().is_empty() || self.event_id.len() > 512 {
            return Err(HostObservationValidationError::RequiredField {
                field: "eventId".into(),
            });
        }
        if self.event_type.trim().is_empty() {
            return Err(HostObservationValidationError::RequiredField {
                field: "eventType".into(),
            });
        }
        if !self.payload.is_object() {
            return Err(HostObservationValidationError::InvalidField {
                field: "payload".into(),
                reason: "normalized row must be a JSON object".into(),
            });
        }
        Ok(())
    }
}

/// CAS append of one bounded batch into the session's absorbed stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTranscriptAppendV1 {
    pub schema_version: u32,
    /// CodeRight session id; also the absorbed-stream identity.
    pub session_id: String,
    /// Repository scope produced by `canonical_repository_id`.
    pub scope_id: String,
    /// The producer's last committed stream seq — the position this batch
    /// extends. The worker compares it to its own cursor before admitting.
    pub expected_stream_seq: u64,
    pub events: Vec<SessionTranscriptEventV1>,
}

impl SessionTranscriptAppendV1 {
    pub fn validate(&self) -> Result<(), HostObservationValidationError> {
        if self.schema_version != SESSION_TRANSCRIPT_SCHEMA_VERSION {
            return Err(HostObservationValidationError::UnsupportedSchemaVersion {
                shape: "SessionTranscriptAppendV1",
                found: self.schema_version,
                expected: SESSION_TRANSCRIPT_SCHEMA_VERSION,
            });
        }
        for (field, value) in [("sessionId", &self.session_id), ("scopeId", &self.scope_id)] {
            if value.trim().is_empty() || value.len() > 512 {
                return Err(HostObservationValidationError::RequiredField {
                    field: field.into(),
                });
            }
        }
        if self.events.is_empty() {
            return Err(HostObservationValidationError::RequiredField {
                field: "events".into(),
            });
        }
        if self.events.len() > SESSION_TRANSCRIPT_MAX_EVENTS_PER_APPEND {
            return Err(HostObservationValidationError::InvalidField {
                field: "events".into(),
                reason: format!(
                    "at most {SESSION_TRANSCRIPT_MAX_EVENTS_PER_APPEND} events per append"
                ),
            });
        }
        for (index, event) in self.events.iter().enumerate() {
            event.validate()?;
            let expected_seq = self
                .expected_stream_seq
                .checked_add(index as u64)
                .and_then(|v| v.checked_add(1))
                .ok_or_else(|| HostObservationValidationError::InvalidField {
                    field: "expectedStreamSeq".into(),
                    reason: "sequence overflow".into(),
                })?;
            if event.seq != expected_seq {
                return Err(HostObservationValidationError::InvalidField {
                    field: format!("events[{index}].seq"),
                    reason: format!("expected {expected_seq}, found {}", event.seq),
                });
            }
            if index > 0 && event.source_seq <= self.events[index - 1].source_seq {
                return Err(HostObservationValidationError::InvalidField {
                    field: format!("events[{index}].sourceSeq"),
                    reason: "source sequence must strictly increase within a batch".into(),
                });
            }
        }
        Ok(())
    }
}

/// Committed-tip state of the absorbed stream — returned in the append
/// receipt and echoed in `transcript_cursor_mismatch` error bodies so the
/// producer can resynchronize without guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTranscriptCursorTipV1 {
    pub stream_id: String,
    pub last_seq: u64,
    /// `sourceSeq` of the tip event; `0` when the stream is empty.
    pub last_source_seq: u64,
    /// `sha256:` digest over `[last_seq, last_event_id, last_content_hash]`
    /// — a tip-bound attestation of the committed prefix.
    pub prefix_digest: String,
}

/// Admission receipt for one committed append batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTranscriptAppendReceiptV1 {
    pub schema_version: u32,
    pub stream_id: String,
    /// Rows newly inserted by this call (`0` on an exact replay).
    pub inserted: u64,
    /// Rows that matched already-committed identical content (idempotent
    /// replay tolerated, never double-appended).
    pub already_present: u64,
    pub last_seq: u64,
    pub last_source_seq: u64,
    pub prefix_digest: String,
}

/// Detail payload of a `transcript_event_conflict` (HTTP 409) body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTranscriptConflictV1 {
    pub stream_id: String,
    pub event_id: String,
}
