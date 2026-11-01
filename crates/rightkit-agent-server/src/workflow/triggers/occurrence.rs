//! WP-06 — Occurrence admission policy.
//!
//! Once the [`super::inbox::TriggerInbox`] has durably admitted a
//! [`NormalizedTriggerEvent`], the *next* question is "should this fire
//! run a workflow?". The occurrence admission layer answers that with the
//! typed [`OccurrenceDecision`] enum, which combines:
//!
//! - the trigger's [`OccurrencePolicy`] (dedupe / overlap / misfire /
//!   version pin),
//! - the daemon's wall-clock view (for misfire detection against the
//!   trigger's `next_fire_at_unix_ms`),
//! - the trigger row's `concurrency_cap` (for overlap admission), and
//! - any prior occurrences on the same `(trigger_id, dedupe_key)` tuple
//!   so we never admit a duplicate.
//!
//! ## Decision surface
//!
//! | Decision    | When                                                     |
//! |-------------|----------------------------------------------------------|
//! | `Admit`     | First-time fire, no overlap, no dedupe collision          |
//! | `Coalesce`  | Misfire window collided — fold into one occurrence        |
//! | `Skip`      | Dedupe collision or misfire window expired               |
//! | `Queue`     | Overlap policy is `Queue` and concurrency cap is reached  |
//! | `Replace`   | Overlap policy is `Replace` (cancel previous, admit new)  |
//! | `Reject`    | Overlap policy is `Forbid` and concurrency cap is reached |
//!
//! ## Determinism
//!
//! The decision is a pure function of the inputs (event, trigger row,
//! policy, prior occurrences, current wall-clock). The same inputs always
//! yield the same [`OccurrenceDecision`], which is what lets the
//! recovery worker re-run admission after a restart and observe the
//! same outcome as the live path.

#![forbid(unsafe_code)]

use rightkit_db::rusqlite::Connection;
use thiserror::Error;

use rightkit_agent_engine_storage::{
    OccurrenceAdmission, StorageError, WorkflowOccurrenceRow, WorkflowRepository,
    WorkflowTriggerEventRow, WorkflowTriggerRow, WorkflowTriggerWriter,
};
use rightkit_workflow::clock::{Clock, UnixMs};
use rightkit_workflow::domain::{occurrence_id, workflow_version_id, TriggerId, WorkflowVersionId};
use rightkit_workflow::prelude::{DedupeStrategy, MisfirePolicy, OccurrencePolicy, OverlapPolicy};

use super::event::NormalizedTriggerEvent;

/// Resolve a cron due instant through the daemon's IANA resolver. The
/// resolver owns zone normalization & offset lookup; this bridge turns its
/// durable epoch result into the workflow domain's explicit DST policy gate.
pub(crate) fn resolve_cron_due(
    cron: &str,
    timezone: &str,
    after_unix_ms: i64,
    policy: rightkit_workflow::trigger::DstResolutionPolicy,
) -> Result<Option<i64>, OccurrenceError> {
    let timezone = crate::scheduler::time::normalize_time_zone(timezone).map_err(|error| {
        OccurrenceError::InvalidDecision {
            decision: "timezone",
            message: error,
        }
    })?;
    let after_minute = after_unix_ms.div_euclid(60_000);
    let due = crate::scheduler::time::next_due_epoch_minute_in_time_zone(
        cron,
        &timezone,
        after_minute,
        527_040,
    )
    .map_err(|error| OccurrenceError::InvalidDecision {
        decision: "cron",
        message: error,
    })?;
    let Some(epoch_minute) = due else {
        return Ok(None);
    };
    let local_minute = crate::scheduler::time::local_epoch_minute(&timezone, epoch_minute)
        .map_err(|error| OccurrenceError::InvalidDecision {
            decision: "timezone",
            message: error,
        })?;
    let resolution =
        match crate::scheduler::time::resolve_local_epoch_minute(&timezone, local_minute).map_err(
            |error| OccurrenceError::InvalidDecision {
                decision: "timezone",
                message: error,
            },
        )? {
            crate::scheduler::time::LocalMinuteResolution::Gap => {
                rightkit_workflow::trigger::LocalTimeResolution::Gap
            }
            crate::scheduler::time::LocalMinuteResolution::Unique(minute) => {
                rightkit_workflow::trigger::LocalTimeResolution::Unique {
                    unix_ms: minute.saturating_mul(60_000),
                }
            }
            crate::scheduler::time::LocalMinuteResolution::Overlap { earlier, later } => {
                rightkit_workflow::trigger::LocalTimeResolution::Overlap {
                    earlier_unix_ms: earlier.saturating_mul(60_000),
                    later_unix_ms: later.saturating_mul(60_000),
                }
            }
        };
    let normalized = rightkit_workflow::trigger::NormalizedOccurrence {
        timezone,
        local_key: format!("{cron}:{local_minute}"),
        resolution,
    };
    rightkit_workflow::trigger::resolve_normalized_occurrence(&normalized, policy).map_err(
        |error| OccurrenceError::InvalidDecision {
            decision: "dst",
            message: error.to_string(),
        },
    )
}

/// Typed outcome of one occurrence admission. The `Admit` / `Coalesce` /
/// `Queue` / `Replace` variants carry the row that was persisted to
/// `workflow_occurrences`; the `Skip` / `Reject` variants carry a
/// synthesised row with `decision = skipped` / `rejected` so the audit
/// trail is durable.
#[derive(Debug, Clone, PartialEq)]
pub enum OccurrenceDecision {
    /// First fire — record an `admitted` occurrence and emit a run.
    Admit(WorkflowOccurrenceRow),
    /// Misfire window collapsed multiple fires into one — record a
    /// `coalesced` occurrence and emit a single run.
    Coalesce(WorkflowOccurrenceRow),
    /// Duplicate occurrence or expired misfire — record a `skipped`
    /// occurrence and emit no run.
    Skip(WorkflowOccurrenceRow),
    /// Overlap policy is `Queue` and the trigger is at concurrency —
    /// record a `queued` occurrence; the supervisor will drain the queue
    /// when the current run finishes.
    Queue(WorkflowOccurrenceRow),
    /// Overlap policy is `Replace` — record a `replaced` occurrence; the
    /// supervisor will cancel the prior run.
    Replace(WorkflowOccurrenceRow),
    /// Overlap policy is `Forbid` and concurrency is full — record a
    /// `rejected` occurrence and emit no run. Surfaced to the operator as
    /// an `attention` projection.
    Reject(WorkflowOccurrenceRow),
}

impl OccurrenceDecision {
    /// Borrow the persisted occurrence row.
    pub fn row(&self) -> &WorkflowOccurrenceRow {
        match self {
            Self::Admit(row)
            | Self::Coalesce(row)
            | Self::Skip(row)
            | Self::Queue(row)
            | Self::Replace(row)
            | Self::Reject(row) => row,
        }
    }

    /// Short string label used in audit messages and logs.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Admit(_) => "admit",
            Self::Coalesce(_) => "coalesce",
            Self::Skip(_) => "skip",
            Self::Queue(_) => "queue",
            Self::Replace(_) => "replace",
            Self::Reject(_) => "reject",
        }
    }
}

/// Inputs the admission policy needs to make a decision. The
/// `normalized_event` is consumed for its `time`, `trigger_id`, and
/// `dedupe_key`; the `trigger_row` is consumed for the kind, overlap
/// policy, concurrency cap, and version pin.
#[derive(Debug, Clone)]
pub struct OccurrenceInputs<'a> {
    /// The normalized event the inbox admitted.
    pub normalized_event: &'a NormalizedTriggerEvent,
    /// The persisted event row produced by the inbox. Used to bind the
    /// `trigger_event_id` column on the occurrence row.
    pub event_row: &'a WorkflowTriggerEventRow,
    /// The trigger definition row from `workflow_triggers`. Must match the
    /// event's `trigger_id`.
    pub trigger_row: &'a WorkflowTriggerRow,
    /// The occurrence policy attached to the trigger (dedupe / overlap /
    /// misfire / version pin / max concurrency).
    pub policy: &'a OccurrencePolicy,
}

/// Failure shape for [`OccurrenceAdmissionLayer::admit`]. All variants are
/// surfaced to the API layer as `500 Internal Server Error` because they
/// indicate a contract violation, not a malformed event.
#[derive(Debug, Error)]
pub enum OccurrenceError {
    /// Storage rejected the admission.
    #[error("occurrence: storage error: {0}")]
    Storage(#[from] StorageError),
    /// The supplied `trigger_id` on the event does not match the
    /// `trigger_row.id`. This should be impossible because the inbox
    /// rejects unprefixed trigger ids, but we belt-and-brace it here.
    #[error("occurrence: event trigger_id {event:?} does not match trigger_row.id {row}")]
    TriggerMismatch {
        event: Option<TriggerId>,
        row: String,
    },
    /// The chosen decision string was rejected by the storage layer
    /// (a future enum extension). Surfaces as a hard failure.
    #[error("occurrence: storage rejected decision {decision:?}: {message}")]
    InvalidDecision {
        decision: &'static str,
        message: String,
    },
}

/// Daemon-side facade over [`WorkflowTriggerWriter::admit_occurrence`] and
/// [`WorkflowRepository`] (for the dedupe probe and concurrency check).
pub struct OccurrenceAdmissionLayer<'a> {
    conn: &'a Connection,
}

impl<'a> OccurrenceAdmissionLayer<'a> {
    /// Wrap a SQLite connection. The caller must hold the daemon's
    /// `Arc<Mutex<Storage>>` for the duration of every method call.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Compute the typed decision *without* persisting. Useful for tests
    /// and for the recovery worker, which prefers to skip persistence when
    /// it is about to replay an identical admission.
    pub fn evaluate(
        &self,
        inputs: &OccurrenceInputs<'_>,
        clock: &dyn Clock,
    ) -> Result<OccurrenceDecision, OccurrenceError> {
        // 1. Validate event / trigger alignment -------------------------------
        let event_trigger_id = inputs
            .normalized_event
            .trigger_id
            .as_ref()
            .map(|t| t.to_string());
        if event_trigger_id.as_deref() != Some(inputs.trigger_row.id.as_str()) {
            return Err(OccurrenceError::TriggerMismatch {
                event: inputs.normalized_event.trigger_id.clone(),
                row: inputs.trigger_row.id.clone(),
            });
        }

        // 2. Compute the canonical `nominal_at` ------------------------------
        let nominal_at_unix_ms =
            crate::workflow::triggers::event::parse_iso_as_unix_ms(&inputs.normalized_event.time)
                .unwrap_or_else(|| clock.now_unix_ms());

        // 3. Dedupe probe ----------------------------------------------------
        let repo = WorkflowRepository::new(self.conn);
        if let Some(prior) = repo.find_occurrence_by_dedupe(
            &inputs.trigger_row.id,
            &inputs.normalized_event.dedupe_key,
        )? {
            return Ok(OccurrenceDecision::Skip(synthesised_skip(
                &inputs.normalized_event.dedupe_key,
                inputs.trigger_row,
                &prior,
                inputs.event_row.id.clone(),
                nominal_at_unix_ms,
                clock,
            )));
        }

        // 4. Misfire window probe --------------------------------------------
        if let Some(decision) = evaluate_misfire(self.conn, inputs, nominal_at_unix_ms, clock)? {
            return Ok(decision);
        }

        // 5. Overlap / concurrency probe -------------------------------------
        if let Some(decision) = evaluate_overlap(self.conn, inputs, nominal_at_unix_ms, clock)? {
            return Ok(decision);
        }

        // 6. Default: admit -------------------------------------------------
        let row = build_admitted_row(inputs, "admitted", "first_fire", nominal_at_unix_ms, clock)?;
        Ok(OccurrenceDecision::Admit(row))
    }

    /// Evaluate and persist. The typed outcome is returned alongside the
    /// persistence result so callers can distinguish "first admit" from
    /// "raced with another admission" without re-reading the database.
    pub fn admit(
        &self,
        inputs: &OccurrenceInputs<'_>,
        clock: &dyn Clock,
    ) -> Result<OccurrenceDecision, OccurrenceError> {
        let decision = self.evaluate(inputs, clock)?;
        if matches!(&decision, OccurrenceDecision::Skip(row) if row.decision_reason.starts_with("dedupe_collision:"))
        {
            return Ok(decision);
        }
        // Synthesised Skip / Reject / Coalesce / Queue / Replace rows are
        // already shaped correctly for the `admit_occurrence` writer.
        let row = decision.row().clone();
        let outcome = WorkflowTriggerWriter::new(self.conn).admit_occurrence(&row)?;
        // Map the storage outcome back to the typed decision so the caller
        // sees a single uniform enum.
        Ok(match outcome {
            OccurrenceAdmission::Admitted(_) => OccurrenceDecision::Admit(row),
            OccurrenceAdmission::Coalesced(_) => OccurrenceDecision::Coalesce(row),
            OccurrenceAdmission::Skipped(_) => OccurrenceDecision::Skip(row),
            OccurrenceAdmission::Queued(_) => OccurrenceDecision::Queue(row),
            OccurrenceAdmission::Replaced(_) => OccurrenceDecision::Replace(row),
            OccurrenceAdmission::Rejected(_) => OccurrenceDecision::Reject(row),
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Evaluate the misfire policy against the trigger's `next_fire_at_unix_ms`
/// and the prior occurrence ledger. Returns `Some(decision)` when the
/// policy short-circuits admission; returns `None` to fall through to the
/// overlap check.
fn evaluate_misfire(
    conn: &Connection,
    inputs: &OccurrenceInputs<'_>,
    nominal_at_unix_ms: UnixMs,
    clock: &dyn Clock,
) -> Result<Option<OccurrenceDecision>, OccurrenceError> {
    let Some(next_fire_at) = inputs.trigger_row.next_fire_at_unix_ms else {
        return Ok(None);
    };
    // A misfire is "the trigger fired later than scheduled and the
    // daemon is catching up". If the inbound event time is at or after
    // `next_fire_at` AND we are *behind* `next_fire_at` by more than the
    // catch-up window, we have a misfire.
    if nominal_at_unix_ms + MISFIRE_GRACE_MS < next_fire_at {
        // Inbound event is *earlier* than the next scheduled fire — not a
        // misfire.
        return Ok(None);
    }
    if clock.now_unix_ms() <= next_fire_at {
        // Daemon is current — not a misfire.
        return Ok(None);
    }
    let reason = match inputs.policy.misfire_policy {
        MisfirePolicy::Skip => Some(build_skipped_or_rejected(
            conn,
            inputs,
            "skipped",
            "misfire_skip",
            nominal_at_unix_ms,
            clock,
        )?),
        MisfirePolicy::Coalesce => Some(build_admitted_decision(
            conn,
            inputs,
            "coalesced",
            "misfire_coalesce",
            nominal_at_unix_ms,
            clock,
        )?),
        MisfirePolicy::BoundedCatchUp => {
            // BoundedCatchUp still admits the first late fire; subsequent
            // late fires within the same window collapse to Coalesce. The
            // supervisor (WP-07) owns the catch-up count; we admit as
            // `coalesced` here so the visible decision is unambiguous.
            Some(build_admitted_decision(
                conn,
                inputs,
                "coalesced",
                "misfire_bounded_catchup",
                nominal_at_unix_ms,
                clock,
            )?)
        }
    };
    Ok(reason)
}

/// Evaluate the overlap policy against the trigger's `concurrency_cap`
/// and the count of in-flight runs for the trigger.
fn evaluate_overlap(
    conn: &Connection,
    inputs: &OccurrenceInputs<'_>,
    nominal_at_unix_ms: UnixMs,
    clock: &dyn Clock,
) -> Result<Option<OccurrenceDecision>, OccurrenceError> {
    // Concurrency cap is `>= 1` so a trigger with `concurrency_cap = 1`
    // never admits two simultaneous runs under overlap-policy `Allow`.
    let in_flight = count_in_flight(conn, &inputs.trigger_row.id)?;
    let cap = inputs.trigger_row.concurrency_cap.max(1);
    if in_flight < cap {
        return Ok(None);
    }
    // Concurrency cap reached. Dispatch on the overlap policy.
    let reason = match inputs.policy.overlap_policy {
        OverlapPolicy::Allow => return Ok(None), // cap not enforced as documented
        OverlapPolicy::Forbid => build_skipped_or_rejected(
            conn,
            inputs,
            "rejected",
            "overlap_forbid",
            nominal_at_unix_ms,
            clock,
        )?,
        OverlapPolicy::Queue => build_admitted_decision(
            conn,
            inputs,
            "queued",
            "overlap_queue",
            nominal_at_unix_ms,
            clock,
        )?,
    };
    Ok(Some(reason))
}

/// Count the number of in-flight runs admitted from `trigger_id`. We treat
/// runs whose `lifecycle_phase` is one of the *not-terminal* phases as
/// in-flight; terminal phases (`succeeded`, `failed`, `cancelled`) are
/// excluded. This mirrors the supervisor's eligibility filter.
fn count_in_flight(conn: &Connection, trigger_id: &str) -> Result<i64, OccurrenceError> {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM workflow_runs
              WHERE json_extract(input_json, '$.triggerId') = ?1
                AND lifecycle_phase NOT IN ('succeeded','failed','cancelled')",
            [trigger_id],
            |row| row.get(0),
        )
        .map_err(StorageError::from)?;
    Ok(n)
}

const MISFIRE_GRACE_MS: UnixMs = 60 * 1000; // one minute slack

/// Build a row representing a non-terminal decision (`skipped` or
/// `rejected`). We still persist the row so the audit trail is durable.
fn build_skipped_or_rejected(
    _conn: &Connection,
    inputs: &OccurrenceInputs<'_>,
    decision: &'static str,
    reason: &str,
    nominal_at_unix_ms: UnixMs,
    clock: &dyn Clock,
) -> Result<OccurrenceDecision, OccurrenceError> {
    if !matches!(decision, "skipped" | "rejected") {
        return Err(OccurrenceError::InvalidDecision {
            decision,
            message: "build_skipped_or_rejected only accepts skipped|rejected".to_string(),
        });
    }
    let row = build_row(inputs, decision, reason, nominal_at_unix_ms, clock)?;
    Ok(match decision {
        "skipped" => OccurrenceDecision::Skip(row),
        "rejected" => OccurrenceDecision::Reject(row),
        _ => unreachable!(),
    })
}

/// Build a row representing a terminal-but-admitted decision (`admitted`,
/// `coalesced`, `queued`, `replaced`).
fn build_admitted_decision(
    _conn: &Connection,
    inputs: &OccurrenceInputs<'_>,
    decision: &'static str,
    reason: &str,
    nominal_at_unix_ms: UnixMs,
    clock: &dyn Clock,
) -> Result<OccurrenceDecision, OccurrenceError> {
    if !matches!(decision, "admitted" | "coalesced" | "queued" | "replaced") {
        return Err(OccurrenceError::InvalidDecision {
            decision,
            message: "build_admitted_decision only accepts admitted|coalesced|queued|replaced"
                .to_string(),
        });
    }
    let row = build_row(inputs, decision, reason, nominal_at_unix_ms, clock)?;
    Ok(match decision {
        "admitted" => OccurrenceDecision::Admit(row),
        "coalesced" => OccurrenceDecision::Coalesce(row),
        "queued" => OccurrenceDecision::Queue(row),
        "replaced" => OccurrenceDecision::Replace(row),
        _ => unreachable!(),
    })
}

/// Build a row that mirrors the prior occurrence's persistence shape but
/// stamps the new decision reason.
fn synthesised_skip(
    dedupe_key: &str,
    trigger_row: &WorkflowTriggerRow,
    prior: &WorkflowOccurrenceRow,
    trigger_event_id: String,
    nominal_at_unix_ms: UnixMs,
    clock: &dyn Clock,
) -> WorkflowOccurrenceRow {
    let now_ms = clock.now_unix_ms();
    WorkflowOccurrenceRow {
        id: make_occurrence_id(dedupe_key, trigger_row.id.as_str(), "skip", now_ms),
        trigger_id: trigger_row.id.clone(),
        trigger_event_id: Some(trigger_event_id),
        nominal_at_unix_ms,
        decision: "skipped".to_string(),
        decision_reason: format!(
            "dedupe_collision:prior={}:{}",
            prior.id, prior.decision_reason
        ),
        selected_version_id: prior.selected_version_id.clone(),
        concurrency_key: prior.concurrency_key.clone(),
        dedupe_key: dedupe_key.to_string(),
        created_at: now_iso(now_ms),
    }
}

fn build_admitted_row(
    inputs: &OccurrenceInputs<'_>,
    decision: &'static str,
    reason: &str,
    nominal_at_unix_ms: UnixMs,
    clock: &dyn Clock,
) -> Result<WorkflowOccurrenceRow, OccurrenceError> {
    build_row(inputs, decision, reason, nominal_at_unix_ms, clock)
}

fn build_row(
    inputs: &OccurrenceInputs<'_>,
    decision: &str,
    reason: &str,
    nominal_at_unix_ms: UnixMs,
    clock: &dyn Clock,
) -> Result<WorkflowOccurrenceRow, OccurrenceError> {
    if !matches!(
        decision,
        "admitted" | "coalesced" | "skipped" | "queued" | "replaced" | "rejected"
    ) {
        return Err(OccurrenceError::InvalidDecision {
            decision: "<runtime>",
            message: format!("unknown decision {decision:?}"),
        });
    }
    let now_ms = clock.now_unix_ms();
    let selected_version_id = resolve_version_pin(inputs);
    let concurrency_key = make_concurrency_key(inputs);
    Ok(WorkflowOccurrenceRow {
        id: make_occurrence_id(
            &inputs.normalized_event.dedupe_key,
            &inputs.trigger_row.id,
            decision,
            now_ms,
        ),
        trigger_id: inputs.trigger_row.id.clone(),
        trigger_event_id: Some(inputs.event_row.id.clone()),
        nominal_at_unix_ms,
        decision: decision.to_string(),
        decision_reason: reason.to_string(),
        selected_version_id: selected_version_id.map(|v| v.to_string()),
        concurrency_key: Some(concurrency_key),
        dedupe_key: inputs.normalized_event.dedupe_key.clone(),
        created_at: now_iso(now_ms),
    })
}

fn resolve_version_pin(inputs: &OccurrenceInputs<'_>) -> Option<WorkflowVersionId> {
    inputs
        .policy
        .version_pin
        .as_ref()
        .map(|v| workflow_version_id(v.clone()))
}

fn make_concurrency_key(inputs: &OccurrenceInputs<'_>) -> String {
    format!("wf.conc.{}", inputs.trigger_row.id)
}

fn make_occurrence_id(
    dedupe_key: &str,
    trigger_id: &str,
    decision: &str,
    now_ms: UnixMs,
) -> String {
    let raw = format!("{trigger_id}|{dedupe_key}|{decision}|{now_ms}");
    occurrence_id(raw).to_string()
}

fn now_iso(ms: UnixMs) -> String {
    super::event::format_unix_ms_as_iso(ms)
}

// `TriggerEventId` is referenced for documentation; the lint keeps the
// import.
#[allow(dead_code)]
type _TriggerEventId = ();

// `DedupeStrategy` is referenced from the policy but we don't dispatch
// on it here — the supervisor (WP-07) owns the strategy switching. We
// accept the unused-import signal by re-exporting a marker function.
#[allow(dead_code)]
fn _dedupe_strategy_marker(strategy: DedupeStrategy) -> &'static str {
    match strategy {
        DedupeStrategy::None => "none",
        DedupeStrategy::KeyHash => "key_hash",
        DedupeStrategy::IdempotencyKey => "idempotency_key",
    }
}

// `TriggerId` is referenced from `trigger_id.to_string()` comparisons
// in the production code paths; the lint keeps the import.
#[allow(dead_code)]
fn _trigger_id_marker(id: TriggerId) -> String {
    id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::triggers::dedupe::{
        build_normalized_canonical_json, compute_content_hash, derive_dedupe_key, derive_event_id,
    };
    use crate::workflow::triggers::event::{
        NormalizedData, NormalizedTriggerEvent, TrustInfo, TrustLevel, TrustOrigin,
    };
    use rightkit_agent_engine_storage::Storage;
    use rightkit_workflow::clock::TestClock;
    use rightkit_workflow::domain::trigger_id;

    fn now_iso(ms: i64) -> String {
        crate::workflow::triggers::event::format_unix_ms_as_iso(ms)
    }

    fn fixture_event(now_ms: i64, source: &str, external_id: &str) -> NormalizedTriggerEvent {
        let data = NormalizedData::from_json(serde_json::json!({"k": "v"})).expect("data");
        let canonical = build_normalized_canonical_json(&data).expect("canonical");
        let content_hash = compute_content_hash(&canonical);
        let trigger = trigger_id("wft_occurrence");
        let dedupe = derive_dedupe_key(
            source,
            external_id,
            &content_hash,
            Some(trigger.to_string().as_str()),
        );
        let event_id = derive_event_id(source, external_id, now_ms);
        NormalizedTriggerEvent {
            spec_version: NormalizedTriggerEvent::SPEC_VERSION.to_string(),
            id: event_id,
            trigger_id: Some(trigger.clone()),
            external_id: Some(external_id.to_string()),
            source: source.to_string(),
            type_: "manual".to_string(),
            subject: None,
            time: now_iso(now_ms),
            received_at: now_iso(now_ms),
            data_content_type: "application/json".to_string(),
            data_schema: None,
            data,
            trust: TrustInfo {
                level: TrustLevel::Authenticated,
                authenticated: true,
                origin: TrustOrigin::SessionOrigin,
                principal: "tester".to_string(),
                admission_policy: "unit-test".to_string(),
                signature_algorithm: None,
                signature_key_id: None,
                actor_association: None,
                workspace_trust: None,
                findings: Vec::new(),
            },
            dedupe_key: dedupe,
            content_hash,
            raw_artifact: None,
            correlation_id: None,
            causation_id: None,
            trace_parent: None,
            extensions: std::collections::BTreeMap::new(),
        }
    }

    fn fixture_event_row(now_ms: i64, source: &str, external_id: &str) -> WorkflowTriggerEventRow {
        let event = fixture_event(now_ms, source, external_id);
        WorkflowTriggerEventRow {
            id: event.id.to_string(),
            trigger_id: event.trigger_id.as_ref().map(|t| t.to_string()),
            source: event.source.clone(),
            event_type: event.type_.clone(),
            external_id: event.external_id.clone(),
            dedupe_key: event.dedupe_key.clone(),
            content_hash: event.content_hash.as_str().to_string(),
            trust_json: serde_json::to_string(&event.trust).unwrap(),
            data_json: serde_json::to_string(&event.data.as_value()).unwrap(),
            raw_artifact_id: None,
            occurred_at: event.time.clone(),
            received_at: event.received_at.clone(),
        }
    }

    fn fixture_trigger_row(
        id: &str,
        overlap_policy: OverlapPolicy,
        misfire_policy: MisfirePolicy,
        next_fire_at_unix_ms: Option<i64>,
        concurrency_cap: i64,
    ) -> WorkflowTriggerRow {
        WorkflowTriggerRow {
            id: id.to_string(),
            workflow_id: "wfd_occurrence_test".to_string(),
            kind: "manual".to_string(),
            enabled: true,
            cron_expression: None,
            timezone: None,
            version_pin: Some("wfv_pin".to_string()),
            dedupe_key: None,
            overlap_policy: policy_to_string(overlap_policy),
            misfire_policy: policy_to_string_misfire(misfire_policy),
            concurrency_cap,
            next_fire_at_unix_ms,
            revision: 1,
            created_at: "2026-08-07T00:00:00Z".to_string(),
            updated_at: "2026-08-07T00:00:00Z".to_string(),
        }
    }

    fn policy_to_string(p: OverlapPolicy) -> String {
        match p {
            OverlapPolicy::Forbid => "forbid",
            OverlapPolicy::Allow => "allow",
            OverlapPolicy::Queue => "queue",
        }
        .to_string()
    }

    fn policy_to_string_misfire(p: MisfirePolicy) -> String {
        match p {
            MisfirePolicy::Skip => "skip",
            MisfirePolicy::Coalesce => "coalesce",
            MisfirePolicy::BoundedCatchUp => "bounded_catchup",
        }
        .to_string()
    }

    fn fixture_policy(
        overlap: OverlapPolicy,
        misfire: MisfirePolicy,
        dedupe: DedupeStrategy,
    ) -> OccurrencePolicy {
        OccurrencePolicy {
            version_pin: Some("wfv_pin".to_string()),
            dedupe_strategy: dedupe,
            misfire_policy: misfire,
            overlap_policy: overlap,
            max_concurrency: 1,
        }
    }

    #[test]
    fn first_admit_is_admitted() {
        let storage = Storage::open_in_memory().expect("memory");
        let now_ms = 1_700_000_000_000_i64;
        let event = fixture_event(now_ms, "github:o/r", "deliv-1");
        let event_row = fixture_event_row(now_ms, "github:o/r", "deliv-1");
        let trigger_row = fixture_trigger_row(
            "wft_occurrence",
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            None,
            1,
        );
        let policy = fixture_policy(
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            DedupeStrategy::KeyHash,
        );
        let inputs = OccurrenceInputs {
            normalized_event: &event,
            event_row: &event_row,
            trigger_row: &trigger_row,
            policy: &policy,
        };
        let layer = OccurrenceAdmissionLayer::new(storage.conn());
        let clock = TestClock::new(now_ms);
        let decision = layer.admit(&inputs, &clock).expect("admit");
        assert!(matches!(decision, OccurrenceDecision::Admit(_)));
        assert_eq!(decision.label(), "admit");
    }

    #[test]
    fn duplicate_admit_collapses_to_skip() {
        let storage = Storage::open_in_memory().expect("memory");
        let now_ms = 1_700_000_000_000_i64;
        let event = fixture_event(now_ms, "github:o/r", "deliv-1");
        let event_row = fixture_event_row(now_ms, "github:o/r", "deliv-1");
        let trigger_row = fixture_trigger_row(
            "wft_occurrence",
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            None,
            1,
        );
        let policy = fixture_policy(
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            DedupeStrategy::KeyHash,
        );
        let inputs = OccurrenceInputs {
            normalized_event: &event,
            event_row: &event_row,
            trigger_row: &trigger_row,
            policy: &policy,
        };
        let layer = OccurrenceAdmissionLayer::new(storage.conn());
        let clock = TestClock::new(now_ms);

        // First admit.
        let first = layer.admit(&inputs, &clock).expect("first");
        assert!(matches!(first, OccurrenceDecision::Admit(_)));

        // Same dedupe key — second admit must collapse to Skip.
        let second = layer.admit(&inputs, &clock).expect("second");
        assert!(matches!(second, OccurrenceDecision::Skip(_)));
        assert!(second.row().decision_reason.contains("dedupe_collision"));
    }

    #[test]
    fn overlap_forbid_rejects_when_concurrency_full() {
        let storage = Storage::open_in_memory().expect("memory");
        let now_ms = 1_700_000_000_000_i64;
        let trigger_row = fixture_trigger_row(
            "wft_occurrence",
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            None,
            1,
        );
        let policy = fixture_policy(
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            DedupeStrategy::KeyHash,
        );
        let layer = OccurrenceAdmissionLayer::new(storage.conn());
        let clock = TestClock::new(now_ms);

        // Plant a run for the trigger so the concurrency probe finds it.
        storage
            .conn()
            .execute(
                "INSERT INTO workflow_runs(
                id, workflow_id, workflow_version_id,
                lifecycle_phase, lifecycle_revision,
                initiated_by_kind, input_json, started_at, runs_root_durable
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rightkit_db::rusqlite::params![
                    "wfr_inflight",
                    "wfd_x",
                    "wfv_x",
                    "running",
                    1_i64,
                    "manual",
                    r#"{"triggerId":"wft_occurrence"}"#,
                    now_iso(now_ms),
                    1_i64,
                ],
            )
            .expect("seed run");

        let event = fixture_event(now_ms, "github:o/r", "deliv-2");
        let event_row = fixture_event_row(now_ms, "github:o/r", "deliv-2");
        let inputs = OccurrenceInputs {
            normalized_event: &event,
            event_row: &event_row,
            trigger_row: &trigger_row,
            policy: &policy,
        };
        let decision = layer.admit(&inputs, &clock).expect("admit");
        assert!(matches!(decision, OccurrenceDecision::Reject(_)));
        assert!(decision.row().decision_reason.contains("overlap_forbid"));
    }

    #[test]
    fn overlap_queue_queues_when_concurrency_full() {
        let storage = Storage::open_in_memory().expect("memory");
        let now_ms = 1_700_000_000_000_i64;
        let trigger_row = fixture_trigger_row(
            "wft_occurrence",
            OverlapPolicy::Queue,
            MisfirePolicy::Skip,
            None,
            1,
        );
        let policy = fixture_policy(
            OverlapPolicy::Queue,
            MisfirePolicy::Skip,
            DedupeStrategy::KeyHash,
        );
        let layer = OccurrenceAdmissionLayer::new(storage.conn());
        let clock = TestClock::new(now_ms);

        storage
            .conn()
            .execute(
                "INSERT INTO workflow_runs(
                id, workflow_id, workflow_version_id,
                lifecycle_phase, lifecycle_revision,
                initiated_by_kind, input_json, started_at, runs_root_durable
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rightkit_db::rusqlite::params![
                    "wfr_inflight",
                    "wfd_x",
                    "wfv_x",
                    "running",
                    1_i64,
                    "manual",
                    r#"{"triggerId":"wft_occurrence"}"#,
                    now_iso(now_ms),
                    1_i64,
                ],
            )
            .expect("seed run");

        let event = fixture_event(now_ms, "github:o/r", "deliv-3");
        let event_row = fixture_event_row(now_ms, "github:o/r", "deliv-3");
        let inputs = OccurrenceInputs {
            normalized_event: &event,
            event_row: &event_row,
            trigger_row: &trigger_row,
            policy: &policy,
        };
        let decision = layer.admit(&inputs, &clock).expect("admit");
        assert!(matches!(decision, OccurrenceDecision::Queue(_)));
        assert!(decision.row().decision_reason.contains("overlap_queue"));
    }

    #[test]
    fn misfire_skip_skips_when_daemon_behind() {
        let storage = Storage::open_in_memory().expect("memory");
        let scheduled_ms = 1_700_000_000_000_i64;
        let now_ms = scheduled_ms + 5 * 60 * 1000; // daemon is 5 minutes late
        let event = fixture_event(scheduled_ms, "github:o/r", "deliv-4");
        let event_row = fixture_event_row(scheduled_ms, "github:o/r", "deliv-4");
        let trigger_row = fixture_trigger_row(
            "wft_occurrence",
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            Some(scheduled_ms),
            1,
        );
        let policy = fixture_policy(
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            DedupeStrategy::KeyHash,
        );
        let inputs = OccurrenceInputs {
            normalized_event: &event,
            event_row: &event_row,
            trigger_row: &trigger_row,
            policy: &policy,
        };
        let layer = OccurrenceAdmissionLayer::new(storage.conn());
        let clock = TestClock::new(now_ms);
        let decision = layer.admit(&inputs, &clock).expect("admit");
        assert!(matches!(decision, OccurrenceDecision::Skip(_)));
        assert!(decision.row().decision_reason.contains("misfire_skip"));
    }

    #[test]
    fn misfire_coalesce_admits_as_coalesced() {
        let storage = Storage::open_in_memory().expect("memory");
        let scheduled_ms = 1_700_000_000_000_i64;
        let now_ms = scheduled_ms + 5 * 60 * 1000;
        let event = fixture_event(scheduled_ms, "github:o/r", "deliv-5");
        let event_row = fixture_event_row(scheduled_ms, "github:o/r", "deliv-5");
        let trigger_row = fixture_trigger_row(
            "wft_occurrence",
            OverlapPolicy::Forbid,
            MisfirePolicy::Coalesce,
            Some(scheduled_ms),
            1,
        );
        let policy = fixture_policy(
            OverlapPolicy::Forbid,
            MisfirePolicy::Coalesce,
            DedupeStrategy::KeyHash,
        );
        let inputs = OccurrenceInputs {
            normalized_event: &event,
            event_row: &event_row,
            trigger_row: &trigger_row,
            policy: &policy,
        };
        let layer = OccurrenceAdmissionLayer::new(storage.conn());
        let clock = TestClock::new(now_ms);
        let decision = layer.admit(&inputs, &clock).expect("admit");
        assert!(matches!(decision, OccurrenceDecision::Coalesce(_)));
        assert!(decision.row().decision_reason.contains("misfire"));
    }

    #[test]
    fn evaluate_is_pure_for_same_inputs() {
        let storage = Storage::open_in_memory().expect("memory");
        let now_ms = 1_700_000_000_000_i64;
        let event = fixture_event(now_ms, "github:o/r", "deliv-6");
        let event_row = fixture_event_row(now_ms, "github:o/r", "deliv-6");
        let trigger_row = fixture_trigger_row(
            "wft_occurrence",
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            None,
            1,
        );
        let policy = fixture_policy(
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            DedupeStrategy::KeyHash,
        );
        let inputs = OccurrenceInputs {
            normalized_event: &event,
            event_row: &event_row,
            trigger_row: &trigger_row,
            policy: &policy,
        };
        let layer = OccurrenceAdmissionLayer::new(storage.conn());
        let clock = TestClock::new(now_ms);

        let d1 = layer.evaluate(&inputs, &clock).expect("eval-1");
        let d2 = layer.evaluate(&inputs, &clock).expect("eval-2");
        assert_eq!(d1.row().decision, d2.row().decision);
        assert_eq!(d1.row().dedupe_key, d2.row().dedupe_key);
    }

    #[test]
    fn trigger_mismatch_is_a_hard_error() {
        let storage = Storage::open_in_memory().expect("memory");
        let now_ms = 1_700_000_000_000_i64;
        let mut event = fixture_event(now_ms, "github:o/r", "deliv-7");
        event.trigger_id = Some(trigger_id("wft_other"));
        let event_row = fixture_event_row(now_ms, "github:o/r", "deliv-7");
        let trigger_row = fixture_trigger_row(
            "wft_occurrence",
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            None,
            1,
        );
        let policy = fixture_policy(
            OverlapPolicy::Forbid,
            MisfirePolicy::Skip,
            DedupeStrategy::KeyHash,
        );
        let inputs = OccurrenceInputs {
            normalized_event: &event,
            event_row: &event_row,
            trigger_row: &trigger_row,
            policy: &policy,
        };
        let layer = OccurrenceAdmissionLayer::new(storage.conn());
        let clock = TestClock::new(now_ms);
        let err = layer.evaluate(&inputs, &clock).expect_err("must error");
        assert!(matches!(err, OccurrenceError::TriggerMismatch { .. }));
    }

    #[test]
    fn decision_label_matches_variant() {
        let row = WorkflowOccurrenceRow {
            id: "wfo_test".to_string(),
            trigger_id: "wft_x".to_string(),
            trigger_event_id: None,
            nominal_at_unix_ms: 1,
            decision: "admitted".to_string(),
            decision_reason: "r".to_string(),
            selected_version_id: None,
            concurrency_key: None,
            dedupe_key: "k".to_string(),
            created_at: "2026-08-07T00:00:00Z".to_string(),
        };
        assert_eq!(OccurrenceDecision::Admit(row.clone()).label(), "admit");
        assert_eq!(
            OccurrenceDecision::Coalesce(row.clone()).label(),
            "coalesce"
        );
        assert_eq!(OccurrenceDecision::Skip(row.clone()).label(), "skip");
        assert_eq!(OccurrenceDecision::Queue(row.clone()).label(), "queue");
        assert_eq!(OccurrenceDecision::Replace(row.clone()).label(), "replace");
        assert_eq!(OccurrenceDecision::Reject(row).label(), "reject");
    }
}
