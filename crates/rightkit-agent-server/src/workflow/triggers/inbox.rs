//! WP-06 — Trigger-event inbox.
//!
//! The inbox is the daemon-side facade that turns an authenticated
//! [`NormalizedTriggerEvent`] into a durable row in `workflow_trigger_events`.
//! HMAC, replay, body-size, workspace, and trust admission happen upstream
//! in the `api_triggers` delivery route; by the time a value reaches
//! [`TriggerInbox::admit_normalized`] the body has been normalised, the
//! content hash has been computed, the dedupe key has been assigned, and a
//! [`TrustInfo`] has been decided.
//!
//! ## Responsibilities
//!
//! 1. Validate the *post-admission* shape — `specVersion`, `dataContentType`,
//!    bounded `extensions` map, length-capped `subject` / `source` /
//!    `dedupe_key` / `external_id`.
//! 2. Stamp the `time` and `received_at` fields using the supplied clock so
//!    re-derived events during recovery get the same wall-clock timestamp.
//! 3. Persist the row through [`WorkflowTriggerWriter::insert_trigger_event_idempotent`]
//!    so redeliveries collapse to the originally stored id rather than
//!    surfacing a constraint error.
//! 4. Emit an [`InboxAuditRow`] for every admission attempt (admitted,
//!    duplicate, rejected) so the audit trail is durable alongside the
//!    event row.
//!
//! ## What this module does **not** do
//!
//! - It does **not** pick a workflow version. That is the
//!   [`super::occurrence`] layer's job, which runs after the event is
//!   durably admitted.
//! - It does **not** enforce overlap or misfire policy. The inbox is
//!   idempotency-only: a redelivery is "the same event, please don't run
//!   it twice", not "skip the second run".
//! - It does **not** run a row-level mutex. SQLite serialises the writes
//!   through the daemon's `Arc<Mutex<Storage>>` (see
//!   [`super::supervisor`]); the inbox trusts that the caller holds the
//!   lock for the duration of `admit_normalized`.

#![forbid(unsafe_code)]

use rightkit_db::rusqlite::Connection;
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;
use tracing::debug;

use rightkit_agent_engine_storage::{
    StorageError, TriggerEventInsert, WorkflowTriggerEventRow, WorkflowTriggerWriter,
};
use rightkit_workflow::control_plane::clock::{Clock, UnixMs};
use rightkit_workflow::control_plane::domain::{
    trigger_event_id, ContentHash, TriggerEventId, TriggerId,
};

use super::dedupe;
use super::event::{NormalizedTriggerEvent, PersistedEventRow, RawArtifact, TrustInfo};

/// Outcome of an inbox admission attempt. Callers use [`InboxAdmission::Duplicate`]
/// to short-circuit further processing when a redelivery collapses against
/// the originally stored event.
#[derive(Debug, Clone, PartialEq)]
pub enum InboxAdmission {
    /// New row written. `event_id` is the stable id assigned by
    /// [`dedupe::derive_event_id`].
    Admitted {
        event_id: TriggerEventId,
        row: WorkflowTriggerEventRow,
    },
    /// The `(source, dedupe_key)` pair was already present. The stored row
    /// is returned verbatim so the caller can merge in any new metadata
    /// (e.g. a refreshed trust envelope) without re-reading the database.
    Duplicate {
        event_id: TriggerEventId,
        row: WorkflowTriggerEventRow,
    },
}

/// Failure shape for [`TriggerInbox::admit_normalized`]. The error kinds are
/// intentionally explicit so the API layer can map them onto the right HTTP
/// status without re-parsing a stringified error.
#[derive(Debug, Error)]
pub enum InboxError {
    /// The event's `specVersion` is not `"coderight.trigger/v1"`.
    #[error("inbox: refusing unsupported specVersion {0}")]
    UnsupportedSpecVersion(String),
    /// The event's `dataContentType` is not `application/json`.
    #[error("inbox: refusing unsupported dataContentType {0}")]
    UnsupportedDataContentType(String),
    /// The event's `dedupeKey` is empty or exceeds the schema's 1024-byte
    /// bound after normalisation.
    #[error("inbox: dedupe_key must be 1..={max} bytes (got {got})", max = MAX_DEDUPE_KEY_BYTES)]
    DedupeKeyOutOfRange { got: usize },
    /// The `extensions` map exceeds the schema's 32-entry cap.
    #[error("inbox: extensions map must hold at most {max} entries (got {got})", max = MAX_EXTENSIONS_ENTRIES)]
    ExtensionsTooLarge { got: usize },
    /// The `extensions` entry value is not a scalar (object / array).
    #[error("inbox: extensions[{key}] must be a scalar value (got non-scalar)")]
    ExtensionNotScalar { key: String },
    /// The `subject` field exceeds the schema's 2048-char cap.
    #[error("inbox: subject exceeds {max} bytes (got {got})", max = MAX_SUBJECT_BYTES)]
    SubjectTooLarge { got: usize },
    /// The `source` field is empty or exceeds the schema's 512-byte cap.
    #[error("inbox: source must be 1..={max} bytes (got {got})", max = MAX_SOURCE_BYTES)]
    SourceOutOfRange { got: usize },
    /// The `external_id` field exceeds the schema's 512-byte cap.
    #[error("inbox: external_id exceeds {max} bytes (got {got})", max = MAX_EXTERNAL_ID_BYTES)]
    ExternalIdTooLarge { got: usize },
    /// The `type` field is empty or exceeds the schema's 256-byte cap.
    #[error("inbox: type must be 1..={max} bytes (got {got})", max = MAX_TYPE_BYTES)]
    TypeOutOfRange { got: usize },
    /// The `data` slot could not be re-serialised to canonical JSON.
    #[error("inbox: failed to canonicalize data: {0}")]
    Canonicalize(String),
    /// The trust envelope's `principal` field is empty — every admitted
    /// event must carry a non-empty principal for downstream audit.
    #[error("inbox: trust.principal must be non-empty")]
    EmptyTrustPrincipal,
    /// The trust envelope's `admissionPolicy` field is empty.
    #[error("inbox: trust.admissionPolicy must be non-empty")]
    EmptyAdmissionPolicy,
    /// The trigger id (when present) does not use the namespaced
    /// `wft_` prefix required by the workflow crate.
    #[error("inbox: trigger_id must use the wf_evt namespaced prefix (got {0})")]
    InvalidTriggerId(String),
    /// Storage rejected the write. The caller should propagate this — the
    /// inbox never masks storage errors.
    #[error("inbox: storage error: {0}")]
    Storage(#[from] StorageError),
}

impl InboxError {
    /// True when this error should surface to the API layer as a `400
    /// Bad Request` (the event is malformed for our schema).
    pub fn is_validation(&self) -> bool {
        !matches!(self, Self::Storage(_))
    }
}

// Schema bounds (mirror `trigger-event-v1.schema.json`).
pub const MAX_DEDUPE_KEY_BYTES: usize = 1024;
pub const MAX_SUBJECT_BYTES: usize = 2048;
pub const MAX_SOURCE_BYTES: usize = 512;
pub const MAX_EXTERNAL_ID_BYTES: usize = 512;
pub const MAX_TYPE_BYTES: usize = 256;
pub const MAX_EXTENSIONS_ENTRIES: usize = 32;

/// Daemon-side facade over [`WorkflowTriggerWriter`]. Construction is
/// cheap: the inbox holds only a `&Connection`, so a single
/// `Arc<Mutex<Storage>>` can produce many inboxes through the daemon's
/// lifetime.
pub struct TriggerInbox<'a> {
    conn: &'a Connection,
}

impl<'a> TriggerInbox<'a> {
    /// Wrap a SQLite connection. The caller must hold the daemon's
    /// `Arc<Mutex<Storage>>` for the duration of every method call on the
    /// returned inbox.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Validate, stamp, and persist a normalized trigger event. The clock
    /// supplies the wall-clock values that get baked into `time` and
    /// `received_at`; pass a `TestClock` in tests so timestamps are
    /// deterministic.
    ///
    /// On success the stored row is returned with the assigned event id;
    /// on a redelivery the *original* row is returned so the caller can
    /// merge any new trust findings into the stored envelope without an
    /// extra round trip.
    pub fn admit_normalized(
        &self,
        event: &mut NormalizedTriggerEvent,
        clock: &dyn Clock,
    ) -> Result<InboxAdmission, InboxError> {
        // 1. Schema validation ------------------------------------------------
        validate_spec_version(&event.spec_version)?;
        validate_data_content_type(&event.data_content_type)?;
        validate_source(&event.source)?;
        validate_type(&event.type_)?;
        validate_subject(event.subject.as_deref())?;
        validate_external_id(event.external_id.as_deref())?;
        validate_dedupe_key(&event.dedupe_key)?;
        validate_extensions(&event.extensions)?;
        validate_trust(&event.trust)?;
        if let Some(trigger_id) = &event.trigger_id {
            validate_trigger_id_prefix(trigger_id)?;
        }

        // 2. Stamp times -------------------------------------------------------
        let now_unix_ms = clock.now_unix_ms();
        // Honour an explicitly-provided `time` (the API may have set it
        // from an upstream timestamp header); default to "now" when blank.
        let time_unix_ms = parse_iso_or_now(&event.time, now_unix_ms);
        event.stamp_times(time_unix_ms, now_unix_ms);

        // 3. Derive ids / hashes / dedupe key --------------------------------
        let external_id = event.external_id.clone().unwrap_or_default();
        let computed_event_id = dedupe::derive_event_id(&event.source, &external_id, time_unix_ms);
        // The schema mandates that `id` is stable. If the caller pre-assigned
        // one, we *honour* it (so the upstream contract is preserved) but we
        // *verify* it matches our derivation — otherwise we surface a
        // validation error so the caller knows their id is inconsistent.
        if event.id.to_string() != computed_event_id.to_string() {
            return Err(InboxError::UnsupportedSpecVersion(format!(
                "event id {} does not match derived id {} for source={} external_id={} time_unix_ms={}",
                event.id,
                computed_event_id,
                event.source,
                external_id,
                time_unix_ms,
            )));
        }

        let canonical = dedupe::build_normalized_canonical_json(&event.data)
            .map_err(|err| InboxError::Canonicalize(err.to_string()))?;
        let computed_content_hash = dedupe::compute_content_hash(&canonical);
        // Verify the caller-supplied content hash matches. This is the
        // invariant that lets the recovery worker re-derive the event from
        // the raw body and trust that the hash matches what was originally
        // admitted.
        if event.content_hash.as_str() != computed_content_hash.as_str() {
            return Err(InboxError::UnsupportedSpecVersion(format!(
                "content_hash {} does not match derived {} for source={}",
                event.content_hash, computed_content_hash, event.source,
            )));
        }

        // 4. Persist ----------------------------------------------------------
        let trust_json = serde_json::to_string(&event.trust)
            .map_err(|err| InboxError::Canonicalize(format!("trust serialization: {err}")))?;
        let data_json = serde_json::to_string(&event.data.as_value())
            .map_err(|err| InboxError::Canonicalize(format!("data serialization: {err}")))?;

        let raw_artifact_id = event.raw_artifact.as_ref().map(|a| a.artifact_id.clone());

        let row = WorkflowTriggerEventRow {
            id: computed_event_id.to_string(),
            trigger_id: event.trigger_id.as_ref().map(|t| t.to_string()),
            source: event.source.clone(),
            event_type: event.type_.clone(),
            external_id: event.external_id.clone(),
            dedupe_key: event.dedupe_key.clone(),
            content_hash: event.content_hash.as_str().to_string(),
            trust_json,
            data_json,
            raw_artifact_id,
            occurred_at: event.time.clone(),
            received_at: event.received_at.clone(),
        };

        let outcome =
            WorkflowTriggerWriter::new(self.conn).insert_trigger_event_idempotent(&row)?;
        debug!(
            event_id = %computed_event_id,
            source = %event.source,
            outcome = ?outcome_label(&outcome),
            "inbox: trigger event admitted",
        );
        Ok(match outcome {
            TriggerEventInsert::Inserted(stored) => InboxAdmission::Admitted {
                event_id: trigger_event_id(stored.id.clone()),
                row: stored,
            },
            TriggerEventInsert::Duplicate(existing) => InboxAdmission::Duplicate {
                event_id: trigger_event_id(existing.id.clone()),
                row: existing,
            },
        })
    }

    /// Build a [`PersistedEventRow`] from a [`NormalizedTriggerEvent`] for
    /// the caller's convenience. The inbox itself never writes this row;
    /// it is the shape the storage layer persists, sliced into scalar
    /// columns plus a JSON envelope.
    pub fn build_persisted_row(
        event: &NormalizedTriggerEvent,
        event_id: TriggerEventId,
    ) -> Result<PersistedEventRow, InboxError> {
        let time_unix_ms = parse_iso_or_now(&event.time, 0);
        let received_at_unix_ms = parse_iso_or_now(&event.received_at, 0);
        let envelope_json = serde_json::to_string(event)
            .map_err(|err| InboxError::Canonicalize(format!("envelope serialization: {err}")))?;
        Ok(PersistedEventRow {
            event_id,
            trigger_id: event.trigger_id.clone(),
            source: event.source.clone(),
            type_: event.type_.clone(),
            time_unix_ms,
            received_at_unix_ms,
            dedupe_key: event.dedupe_key.clone(),
            content_hash: event.content_hash.clone(),
            trust: event.trust.clone(),
            envelope_json,
        })
    }
}

/// Build an [`InboxAuditRow`] for one admission attempt. The inbox never
/// persists audit rows itself (the API layer owns the audit file), but the
/// shape is exported so the caller's audit emission is consistent.
pub fn build_audit_row(
    created_at_unix_ms: i64,
    trigger_id: Option<&TriggerId>,
    source: &str,
    outcome: &str,
    reason: &str,
    event_id: Option<&TriggerEventId>,
    delivery_id: Option<&str>,
) -> super::event::InboxAuditRow {
    super::event::InboxAuditRow {
        created_at_unix_ms,
        trigger_id: trigger_id.cloned(),
        source: source.to_string(),
        outcome: outcome.to_string(),
        reason: reason.to_string(),
        event_id: event_id.cloned(),
        delivery_id: delivery_id.map(str::to_string),
    }
}

fn outcome_label(outcome: &TriggerEventInsert) -> &'static str {
    match outcome {
        TriggerEventInsert::Inserted(_) => "admitted",
        TriggerEventInsert::Duplicate(_) => "duplicate",
    }
}

// ─── Validators ──────────────────────────────────────────────────────────────

fn validate_spec_version(spec_version: &str) -> Result<(), InboxError> {
    if spec_version == NormalizedTriggerEvent::SPEC_VERSION {
        Ok(())
    } else {
        Err(InboxError::UnsupportedSpecVersion(spec_version.to_string()))
    }
}

fn validate_data_content_type(value: &str) -> Result<(), InboxError> {
    if value.eq_ignore_ascii_case("application/json") {
        Ok(())
    } else {
        Err(InboxError::UnsupportedDataContentType(value.to_string()))
    }
}

fn validate_source(source: &str) -> Result<(), InboxError> {
    let len = source.len();
    if (1..=MAX_SOURCE_BYTES).contains(&len) {
        Ok(())
    } else {
        Err(InboxError::SourceOutOfRange { got: len })
    }
}

fn validate_type(type_: &str) -> Result<(), InboxError> {
    let len = type_.len();
    if (1..=MAX_TYPE_BYTES).contains(&len) {
        Ok(())
    } else {
        Err(InboxError::TypeOutOfRange { got: len })
    }
}

fn validate_subject(subject: Option<&str>) -> Result<(), InboxError> {
    if let Some(value) = subject {
        if value.len() > MAX_SUBJECT_BYTES {
            return Err(InboxError::SubjectTooLarge { got: value.len() });
        }
    }
    Ok(())
}

fn validate_external_id(external_id: Option<&str>) -> Result<(), InboxError> {
    if let Some(value) = external_id {
        if value.len() > MAX_EXTERNAL_ID_BYTES {
            return Err(InboxError::ExternalIdTooLarge { got: value.len() });
        }
    }
    Ok(())
}

fn validate_dedupe_key(key: &str) -> Result<(), InboxError> {
    let len = key.len();
    if (1..=MAX_DEDUPE_KEY_BYTES).contains(&len) {
        Ok(())
    } else {
        Err(InboxError::DedupeKeyOutOfRange { got: len })
    }
}

fn validate_extensions(extensions: &BTreeMap<String, Value>) -> Result<(), InboxError> {
    if extensions.len() > MAX_EXTENSIONS_ENTRIES {
        return Err(InboxError::ExtensionsTooLarge {
            got: extensions.len(),
        });
    }
    for (key, value) in extensions {
        if !is_scalar(value) {
            return Err(InboxError::ExtensionNotScalar { key: key.clone() });
        }
    }
    Ok(())
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn validate_trust(trust: &TrustInfo) -> Result<(), InboxError> {
    if trust.principal.trim().is_empty() {
        return Err(InboxError::EmptyTrustPrincipal);
    }
    if trust.admission_policy.trim().is_empty() {
        return Err(InboxError::EmptyAdmissionPolicy);
    }
    Ok(())
}

fn validate_trigger_id_prefix(trigger_id: &TriggerId) -> Result<(), InboxError> {
    let rendered = trigger_id.to_string();
    if rendered.starts_with("wft_") {
        Ok(())
    } else {
        Err(InboxError::InvalidTriggerId(rendered))
    }
}

/// Parse an ISO timestamp or fall back to `default_ms` when the value is
/// empty / malformed. Returning a default rather than failing closed is
/// deliberate: the API layer has already validated the inbound
/// `X-GitHub-Delivery` etc., and a blank timestamp means "use now".
fn parse_iso_or_now(value: &str, default_ms: UnixMs) -> UnixMs {
    super::event::parse_iso_as_unix_ms(value).unwrap_or(default_ms)
}

// Re-export the helper so external callers don't need to know about the
// `event` module's internal layout.
#[allow(unused_imports)]
pub use super::event::{format_unix_ms_as_iso, parse_iso_as_unix_ms};

// ─── Re-exports for tests ────────────────────────────────────────────────────
//
// The dedupe module is `pub` from `super::dedupe`; we re-export the
// content hash helper so test fixtures don't need a separate import.
#[allow(unused_imports)]
pub use super::dedupe::{
    build_normalized_canonical_json, compute_content_hash, derive_dedupe_key, derive_event_id,
    MAX_COMPONENT_BYTES as DEDUPE_MAX_COMPONENT_BYTES,
    MAX_DEDUPE_KEY_BYTES as DEDUPE_MAX_KEY_BYTES,
};

// `ContentHash` is re-exported so test code can construct one without
// reaching into the workflow crate.
#[allow(unused_imports)]
pub use rightkit_workflow::control_plane::domain::ContentHash as ReexportedContentHash;
#[allow(unused_imports)]
type _RawArtifactMarker = RawArtifact;
