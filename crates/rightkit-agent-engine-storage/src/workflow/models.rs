//! Row types, request/update structs, enums, and shared payload helpers
//! for the workflow storage domain. Pure data — no SQL, no rusqlite
//! Connection/Transaction usage lives in journal.rs, triggers.rs, and
//! runs.rs, which all depend on these types.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// --------------------------------------------------------------------------
// Row types — these mirror the schema column-for-column. The JSON payload
// fields store user-defined data verbatim, and are deserialized lazily by
// the callers that know their inner type.
// --------------------------------------------------------------------------

/// One row of `workflow_definitions`. Source kind is "manual" or "imported".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDefinitionRow {
    pub id: String,
    pub name: String,
    #[serde(default = "default_source_kind")]
    pub source_kind: String,
    pub created_at: String,
    pub updated_at: String,
}

fn default_source_kind() -> String {
    "manual".to_string()
}

/// One row of `workflow_versions`. A workflow's version history is the
/// audit trail of every definition refinement; `content_hash` is the
/// canonical SHA-256 the compiler emitted (see `workflow::compiler`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowVersionRow {
    pub id: String,
    pub workflow_id: String,
    pub version_number: i64,
    pub content_hash: String,
    pub definition_json: String,
    pub compiled_plan_json: String,
    /// One of: `draft` | `published` | `retired`. Stored as TEXT to keep
    /// forward compatibility (new states don't require a schema migration).
    pub publication_state: String,
    pub created_at: String,
    pub published_at: Option<String>,
    #[serde(default = "default_created_by")]
    pub created_by: String,
    #[serde(default)]
    pub notes: String,
}

/// Durable, disabled record for every submitted workflow package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowImportQuarantineRow {
    pub id: String,
    pub raw_artifact_ref: String,
    pub content_hash: String,
    pub signature_status: String,
    pub manifest_json: String,
    pub state: String,
    pub reviewer: Option<String>,
    pub reason: String,
    pub created_at: String,
    pub reviewed_at: Option<String>,
}

/// Human review receipt required before an optimization can admit future work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPromotionReceiptRow {
    pub proposal_id: String,
    pub approver: String,
    pub benchmark_id: String,
    pub activation_id: String,
    pub rollback_id: String,
    pub monitoring_id: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Monitoring evidence; a failing row switches its promotion to rollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowMonitoringReceiptRow {
    pub id: String,
    pub proposal_id: String,
    pub monitoring_id: String,
    pub passed: bool,
    pub reason: String,
    pub created_at: String,
}

fn default_created_by() -> String {
    "system".to_string()
}

/// One row of `workflow_runs`. This is the durable root of a single run;
/// every journal event for that run keys off `id`. `lifecycle_phase` and
/// `lifecycle_revision` are sourced from the journal projection; everything
/// else here is set once and never rewritten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRunRow {
    pub id: String,
    pub workflow_id: String,
    pub workflow_version_id: String,
    pub parent_run_id: Option<String>,
    pub lifecycle_phase: String,
    pub lifecycle_revision: i64,
    pub initiated_by_kind: String,
    pub initiated_by_ref: Option<String>,
    pub trigger_payload_json: Option<String>,
    pub input_json: String,
    pub started_at: String,
    pub terminal_at: Option<String>,
    pub terminal_failure_class: Option<String>,
    pub runs_root_durable: bool,
}

/// One row of `workflow_journal`. The journal is append-only — never
/// UPDATE, never DELETE (apart from retention pruning later, which itself
/// emits a journal event). `event_kind` is one of the `JournalEventKind`
/// constants used by the reducer (see `workflow::reducer`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowJournalRow {
    pub run_id: String,
    pub seq: i64,
    pub revision: i64,
    pub event_kind: String,
    pub node_id: Option<String>,
    pub payload_json: String,
    pub created_at: String,
}

/// One row of `workflow_node_states`. The projection is keyed by
/// `(run_id, node_id)` and the writer derives everything in it from the
/// reducer output; no caller should mutate a `phase` outside the journal
/// writer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowNodeStateRow {
    pub run_id: String,
    pub node_id: String,
    pub phase: String,
    pub attempt: i64,
    pub lease_holder: Option<String>,
    pub lease_expires_at: Option<String>,
    pub last_reducer_revision: i64,
    pub last_event_seq: i64,
    pub last_failure_class: Option<String>,
    pub last_updated_at: String,
}

/// One row of `workflow_run_lease`. The lease is set when a supervisor
/// claims a run (single writer), updated on every keep-alive, and cleared
/// at terminal phase. WP-03 owns the supervisor side; WP-02 only owns the
/// schema + writer for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRunLeaseRow {
    pub run_id: String,
    pub lease_holder: String,
    pub lease_expires_at: String,
    pub lease_fence: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowQueueRow {
    pub run_id: String,
    pub node_id: String,
    pub available_at: String,
    pub priority: i64,
    pub claimed_by: Option<String>,
    pub claim_fence: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowCommandRow {
    pub run_id: String,
    pub command_id: String,
    pub expected_revision: i64,
    pub command_kind: String,
    pub payload_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAttemptRow {
    pub id: String,
    pub run_id: String,
    pub node_id: String,
    pub attempt: i64,
    pub lease_fence: i64,
    pub state: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// A fenced write to a leased attempt. `generation` is never inferred from
/// wall-clock state: callers must carry forward the generation they claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowAttemptUpdate<'a> {
    pub id: &'a str,
    pub generation: i64,
    pub holder: &'a str,
    pub at: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEvidenceRow {
    pub id: String,
    pub run_id: String,
    pub node_id: Option<String>,
    pub kind: String,
    pub content_hash: String,
    pub artifact_ref: Option<String>,
    pub created_at: String,
}

/// A factual assertion emitted by a node. Lease records deliberately live in
/// `workflow_attempt_claims`; this table is reviewable product evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowClaimRow {
    pub id: String,
    pub run_id: String,
    pub node_id: String,
    pub claim_key: String,
    pub statement: String,
    pub state: String,
    pub statement_hash: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowClaimEvidenceRow {
    pub id: String,
    pub claim_id: String,
    pub artifact_ref: Option<String>,
    pub evidence_type: String,
    pub source_json: String,
    pub verdict: String,
    pub evidence_hash: String,
    pub reviewer_attempt_id: Option<String>,
    pub created_at: String,
}

// --------------------------------------------------------------------------
// WP-06 / WP-07 row types — triggers, events, occurrences, waits,
// approvals, effects, deliveries, outbox, attention. Each row mirrors the
// schema column-for-column so the typed mapping is mechanical; JSON payload
// fields are stored verbatim and parsed lazily by the caller that knows
// the inner type.
// --------------------------------------------------------------------------

/// One row of `workflow_triggers`. Owned by the trigger registry / supervisor
/// (WP-06); the storage layer treats `next_fire_at_unix_ms` as an opaque
/// cursor so timezone / DST handling stays in the domain layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowTriggerRow {
    pub id: String,
    pub workflow_id: String,
    /// One of: `manual`, `one_time`, `cron`, `webhook`, `github`,
    /// `internal_event`. Stored as text so the closed enum stays
    /// forward-compatible.
    pub kind: String,
    pub enabled: bool,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub version_pin: Option<String>,
    pub dedupe_key: Option<String>,
    pub overlap_policy: String,
    pub misfire_policy: String,
    pub concurrency_cap: i64,
    pub next_fire_at_unix_ms: Option<i64>,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One row of `workflow_trigger_events`. Inbound events from webhooks,
/// GitHub, cron firings, internal publications — each row's `(source,
/// dedupe_key)` pair is globally unique so an upstream redelivery is a
/// no-op rather than a duplicate admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowTriggerEventRow {
    pub id: String,
    pub trigger_id: Option<String>,
    pub source: String,
    pub event_type: String,
    pub external_id: Option<String>,
    pub dedupe_key: String,
    pub content_hash: String,
    pub trust_json: String,
    pub data_json: String,
    pub raw_artifact_id: Option<String>,
    pub occurred_at: String,
    pub received_at: String,
}

/// One row of `workflow_occurrences`. The supervisor admits one row per
/// unique `(trigger_id, dedupe_key)` so retry / overlap / misfire decisions
/// are durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowOccurrenceRow {
    pub id: String,
    pub trigger_id: String,
    pub trigger_event_id: Option<String>,
    pub nominal_at_unix_ms: i64,
    /// One of: `admitted`, `skipped`, `coalesced`, `queued`, `replaced`,
    /// `rejected`.
    pub decision: String,
    pub decision_reason: String,
    pub selected_version_id: Option<String>,
    pub concurrency_key: Option<String>,
    pub dedupe_key: String,
    pub created_at: String,
}

/// One row of `workflow_waits`. Durable timer / event waits parked on a
/// node; the supervisor wakes them when `wake_at_unix_ms` elapses or when
/// an inbound event matches topic, correlation, & compiled filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowWaitRow {
    pub id: String,
    pub run_id: String,
    pub node_id: String,
    /// `timer` or `event`.
    pub kind: String,
    /// `open`, `matched`, `timed_out`, `cancelled`.
    pub state: String,
    pub correlation_key: String,
    pub topic: Option<String>,
    pub filter_expression: Option<String>,
    pub filter_hash: Option<String>,
    pub wake_at_unix_ms: Option<i64>,
    pub deadline_unix_ms: Option<i64>,
    pub matched_count: i64,
    pub max_matches: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One row of `workflow_approvals`. The `(version_id, inputs_hash,
/// outputs_hash, effect_request_hash)` tuple is the binding — replay rejects
/// a decided approval whose hashes drift from the compiled intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowApprovalRow {
    pub id: String,
    pub run_id: String,
    pub node_id: String,
    pub version_id: String,
    pub inputs_hash: String,
    pub outputs_hash: String,
    pub artifact_refs_json: String,
    pub capabilities_json: String,
    pub effect_request_hash: String,
    pub expires_at_unix_ms: i64,
    pub role: String,
    pub one_shot: bool,
    /// `requested`, `approved`, `rejected`, `expired`, `invalidated`,
    /// `cancelled`.
    pub state: String,
    pub decision_id: Option<String>,
    pub decision_actor: Option<String>,
    pub decision_reason: Option<String>,
    pub decision_signature: Option<String>,
    pub decided_at_unix_ms: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct WorkflowApprovalDecision<'a> {
    pub approval_id: &'a str,
    pub decision_id: &'a str,
    pub actor: &'a str,
    pub role: &'a str,
    pub state: &'a str,
    pub now_unix_ms: i64,
    pub version_id: &'a str,
    pub inputs_hash: &'a str,
    pub outputs_hash: &'a str,
    pub artifact_refs_json: &'a str,
    pub capabilities_json: &'a str,
    pub effect_request_hash: &'a str,
}

/// One row of `workflow_effects`. `(adapter, idempotency_key)` is unique so
/// re-dispatch is a no-op rather than a duplicate external action. The
/// `workflow_effect_terminal_guard` trigger blocks state writes once a row
/// reaches `confirmed` or `cancelled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEffectRow {
    pub id: String,
    pub run_id: String,
    pub node_id: String,
    pub adapter: String,
    pub operation: String,
    pub destination_hash: String,
    pub payload_hash: String,
    pub idempotency_key: String,
    /// `planned`, `awaiting_approval`, `dispatching`, `confirmed`,
    /// `failed`, `outcome_unknown`, `reconciling`, `cancelled`.
    pub state: String,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub receipt_json: Option<String>,
    pub outcome_json: Option<String>,
    pub reconciliation_json: Option<String>,
    pub failure_json: Option<String>,
    pub requested_at_unix_ms: i64,
    pub ttl_unix_ms: i64,
    pub created_at: String,
    pub updated_at: String,
}

pub struct WorkflowEffectClaim<'a> {
    pub id: &'a str,
    pub expected_state: &'a str,
    pub holder: &'a str,
    pub fence: i64,
    pub idempotency_key: &'a str,
    pub now_unix_ms: i64,
    pub expires_at_unix_ms: i64,
    pub updated_at: &'a str,
}

pub struct WorkflowEffectOutcome<'a> {
    pub id: &'a str,
    pub idempotency_key: &'a str,
    pub holder: &'a str,
    pub fence: i64,
    pub state: &'a str,
    pub receipt_json: Option<&'a str>,
    pub outcome_json: Option<&'a str>,
    pub reconciliation_json: Option<&'a str>,
    pub failure_json: Option<&'a str>,
    pub updated_at: &'a str,
}

/// One row of `workflow_deliveries`. Carrier of "this run produced this
/// deliverable" — distinct from `workflow_outbox` (delivery requests) so a
/// delivered artifact can outlive the outbox row that triggered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDeliveryRow {
    pub id: String,
    pub run_id: String,
    pub node_id: Option<String>,
    pub effect_id: Option<String>,
    pub delivery_type: String,
    pub destination_json: String,
    pub payload_hash: String,
    /// `pending`, `delivered`, `failed`, `cancelled`.
    pub state: String,
    pub attempt_count: i64,
    pub receipt_json: Option<String>,
    pub last_error_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One row of `workflow_outbox`. Transactional outbox: appended in the same
/// transaction as the lifecycle transition that produced it so a crash
/// between transition and dispatch is impossible. `dedupe_key` is unique so
/// a re-driver cannot accidentally double-deliver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowOutboxRow {
    pub id: String,
    pub run_id: String,
    /// Closed set — see `workflow::outbox::OutboxKind`. Stored as text.
    pub kind: String,
    pub topic: String,
    pub dedupe_key: String,
    pub payload_json: String,
    /// `pending`, `sending`, `sent`, `failed` (terminal dead letter).
    pub state: String,
    pub available_at: String,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub last_error_json: Option<String>,
    pub next_attempt_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One row of `workflow_attention`, queried by the review inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAttentionRow {
    pub id: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub effect_id: Option<String>,
    pub approval_id: Option<String>,
    /// Closed set — see `workflow::outbox::OutboxKind` for the matching set
    /// plus `deliverable`. Stored as text.
    pub kind: String,
    /// `open`, `read`, `resolved`, `dismissed`.
    pub state: String,
    /// `info`, `warning`, `error`, `critical`.
    pub severity: String,
    pub title: String,
    pub summary: String,
    pub deep_link_json: String,
    pub dedupe_key: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Outcome of an idempotent trigger-event insert. Callers use the
/// `Duplicate` variant to short-circuit redelivery handling without
/// surfacing a storage error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerEventInsert {
    /// New row written; `event` is the inserted row.
    Inserted(WorkflowTriggerEventRow),
    /// A row with the same `(source, dedupe_key)` already existed; the
    /// returned row is the existing one so callers can merge metadata.
    Duplicate(WorkflowTriggerEventRow),
}

#[derive(Debug, Clone)]
pub struct HmacAdmittedTriggerEnvelope {
    pub event: WorkflowTriggerEventRow,
    pub occurrence: WorkflowOccurrenceRow,
    pub raw_artifact: WorkflowTriggerRawArtifactWrite,
    pub hmac_key_id: String,
    pub signature_hash: String,
}

/// Untrusted bytes handed to a content-addressed artifact store after HMAC
/// admission. The bytes never enter workflow SQLite tables.
#[derive(Debug, Clone)]
pub struct WorkflowTriggerRawArtifactWrite {
    pub id: String,
    pub content_hash: String,
    pub source: String,
    pub provenance_json: String,
    pub trust_json: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub created_at: String,
}

/// Durable, non-secret pointer returned only after an artifact store has
/// persisted the content-addressed body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowTriggerRawArtifactRow {
    pub id: String,
    pub content_hash: String,
    pub source: String,
    pub provenance_json: String,
    pub trust_json: String,
    pub storage_uri: String,
    pub metadata_json: String,
    pub byte_length: i64,
    pub created_at: String,
}

pub struct WorkflowTriggerArtifactPut {
    pub row: WorkflowTriggerRawArtifactRow,
    /// True only when this call created new durable content. Existing
    /// content-addressed bytes must never be deleted as rollback cleanup.
    pub created: bool,
}

/// Content-addressed blob writer used before SQLite admission. Success means
/// `storage_uri` resolves to durable bytes matching `content_hash`.
pub trait WorkflowTriggerArtifactStore {
    fn put_content_addressed(
        &self,
        artifact: &WorkflowTriggerRawArtifactWrite,
    ) -> Result<WorkflowTriggerArtifactPut>;

    fn remove_content_addressed(&self, artifact: &WorkflowTriggerRawArtifactRow) -> Result<()>;
}

pub struct TriggerRunAdmission<'a> {
    pub snapshot: &'a rightkit_workflow::control_plane::domain::LifecycleState,
    pub reducer_command: &'a rightkit_workflow::control_plane::reducer::Command,
    pub run: &'a WorkflowRunRow,
    pub command: &'a WorkflowCommandRow,
    pub outcome_json: &'a str,
    pub event: &'a JournalAppend<'a>,
    pub projection: &'a ProjectionUpdate<'a>,
    pub root_queue: &'a WorkflowQueueRow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerAdmissionOutcome {
    pub trigger_event_id: String,
    pub occurrence_id: String,
    pub run_id: Option<String>,
    pub duplicate: bool,
    pub trigger_advanced: bool,
}

/// Outcome of an occurrence admission. Callers translate the `Decision::*`
/// variants into journal projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OccurrenceAdmission {
    Admitted(WorkflowOccurrenceRow),
    Coalesced(WorkflowOccurrenceRow),
    Skipped(WorkflowOccurrenceRow),
    Queued(WorkflowOccurrenceRow),
    Replaced(WorkflowOccurrenceRow),
    Rejected(WorkflowOccurrenceRow),
}

// --------------------------------------------------------------------------
// Definition writer
// --------------------------------------------------------------------------

/// Upserts the definition and version rows for a publish.
///
/// `version_number` is allocated by the caller as `MAX(version_number)+1`
/// per `workflow_id` to keep the writer free of arbitrary "next number"
/// queries — the daemon (WP-09) does that lookup, then passes the result
/// here. Doing it inside the writer would require another SELECT inside
/// the same transaction, which is fine, but the per-PR discipline is to
/// keep the writer's transaction the smallest unit possible.
pub enum WorkflowIntegrityDiagnostic {
    MissingRun {
        run_id: String,
    },
    SequenceGap {
        run_id: String,
        expected: i64,
        observed: i64,
    },
    ProjectionRevisionMismatch {
        run_id: String,
        projection: i64,
        journal: i64,
    },
}

// --------------------------------------------------------------------------
// Journal + projection writer (the only allowed appender)
// --------------------------------------------------------------------------

/// The projection-update bundle a writer applies alongside a journal
/// event. Every field is optional — if `None`, that row is left alone (the
/// caller decided the event doesn't move it). If multiple optional fields
/// are provided for the same row, the writer picks the most-derived one
/// (e.g. `node_phase` > `node_lease_clear`).
#[derive(Debug, Default, Clone)]

pub struct ProjectionUpdate<'a> {
    pub run_phase: Option<&'a str>,
    pub run_lifecycle_revision: Option<i64>,
    pub run_terminal_at: Option<&'a str>,
    pub run_terminal_failure_class: Option<&'a str>,
    pub node: Option<(&'a str, &'a str)>,
    pub node_attempt: Option<i64>,
    pub node_lease: Option<LeaseUpdate<'a>>,
    pub node_last_failure_class: Option<&'a str>,
    pub run_lease: Option<&'a WorkflowRunLeaseRow>,
    /// Fenced lease release performed in same transaction as terminal event.
    pub release_run_lease: Option<LeaseRelease<'a>>,
    pub run_id: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct LeaseRelease<'a> {
    pub run_id: &'a str,
    pub holder: &'a str,
    pub fence: i64,
}

/// Lease state transition for a node: set/refresh/clear. `cleared == true`
/// forces the writer to UPDATE the node row with NULLs regardless of
/// current value.
#[derive(Debug, Clone)]
pub struct LeaseUpdate<'a> {
    pub holder: &'a str,
    pub expires_at: &'a str,
    pub cleared: bool,
}

/// Append a single journal event and apply its projection update in one
/// SQLite transaction. Returns the assigned `(seq, revision)` so callers
/// can correlate subsequent events without re-querying.
/// What the writer is asked to append. The shape mirrors a row of
/// `workflow_journal` minus the auto-assigned `seq` and the derived
/// `revision`.
#[derive(Debug, Clone)]
pub struct JournalAppend<'a> {
    pub run_id: &'a str,
    pub event_kind: &'a str,
    pub node_id: Option<&'a str>,
    pub payload_json: &'a str,
    pub created_at: &'a str,
}

/// Returned by `WorkflowJournalWriter::append_with_projection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalAppendOutcome {
    pub seq: i64,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowCommandApplyOutcome {
    Applied(JournalAppendOutcome),
    Duplicate { outcome_json: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowCommandBoundary {
    Command,
    JournalProjection,
    Queue,
    Attention,
    Outbox,
}

/// Convenience: parse a JSON payload that the journal writer stored.
/// Returns `None` if the payload is `null`; errors bubble up unchanged.
pub fn parse_payload_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<Option<T>> {
    let v: Value = serde_json::from_str(raw)?;
    if v.is_null() {
        return Ok(None);
    }
    let typed: T = serde_json::from_value(v)?;
    Ok(Some(typed))
}
