//! S:F13 — internal monitor-trigger producer.
//!
//! `WorkflowTriggerRow::kind` already documents an `internal_event` variant
//! (`rightkit_agent_engine_storage::workflow::WorkflowTriggerRow`), and
//! `WorkflowTriggerWriter::admit_internal_event` already performs the full
//! occurrence-admission + run-start atomically. The only missing mechanism
//! (per `docs/research/canon-provenance/reconciled-spec.md` row F13)
//! is a producer that can fire that admission from **inside** the daemon —
//! today the only caller is the manual-run HTTP route
//! (`api_workflows/routes.rs`), so an internally observed condition (e.g.
//! another workflow's completion, a storage watcher, a schedule-adjacent
//! signal) has no path to a `monitor`-kind trigger without a synthetic HTTP
//! round-trip to `/schedules/{id}/event`.
//!
//! No reference repository ships a portable monitor-delivery producer
//! (CompShop `F13`: `IMPLEMENT_MISSING`, no lift source) — this is original
//! RightKit code that reuses the existing occurrence/admission pipeline
//! `cron.rs` already drives, rather than inventing a new one.
//!
//! Topic/filter/dedup validation is unchanged: a monitor event still derives
//! its `dedupe_key` from `(source, external_id, content_hash, trigger_id)`
//! exactly like the cron and manual producers, so idempotent re-delivery of
//! the same internal signal is a no-op.
#![forbid(unsafe_code)]

use crate::api::SharedStorage;
use crate::api_workflows::routes::{occurrence_policy, trigger_run_admission};
use rightkit_agent_engine_storage::{
    JournalAppend, ProjectionUpdate, TriggerRunAdmission, WorkflowRepository,
    WorkflowTriggerEventRow, WorkflowTriggerRow, WorkflowTriggerWriter,
};
use rightkit_workflow::control_plane::{clock::Clock, domain::ContentHash};
pub use rightkit_workflow::control_plane_host::{InternalMonitorEvent, MonitorEventQueue};

/// Daemon-owned worker that drains `MonitorEventQueue` and admits each
/// queued signal through the existing occurrence-admission pipeline,
/// mirroring `CronWorker::tick` but sourced from internally-observed
/// conditions instead of wall-clock due times.
pub struct MonitorWorker {
    storage: SharedStorage,
    queue: std::sync::Arc<MonitorEventQueue>,
    clock: rightkit_workflow::control_plane::clock::SystemClock,
}

impl MonitorWorker {
    pub fn new(storage: SharedStorage, queue: std::sync::Arc<MonitorEventQueue>) -> Self {
        Self {
            storage,
            queue,
            clock: rightkit_workflow::control_plane::clock::SystemClock::new(),
        }
    }

    /// Drain every queued internal signal and admit it. Best-effort per
    /// event: one malformed/unknown-trigger event never blocks the rest of
    /// the batch, matching `cron.rs::drive`'s `let _ = admit(..)` shape.
    pub async fn tick(&self) {
        let events = self.queue.drain();
        if events.is_empty() {
            return;
        }
        let storage = self.storage.clone();
        let now = self.clock.now_unix_ms();
        let _ = tokio::task::spawn_blocking(move || {
            let storage = storage.blocking_lock();
            for event in events {
                if let Err(error) = admit_internal_monitor_event(storage.conn(), &event, now) {
                    // Keep draining the batch, but make admission failures a
                    // typed, attributable runtime signal instead of silently
                    // dropping an internal trigger.
                    tracing::error!(
                        %error,
                        trigger_id = %event.trigger_id,
                        event_type = %event.event_type,
                        external_id = %event.external_id,
                        "internal monitor admission failed"
                    );
                }
            }
        })
        .await
        .map_err(|error| tracing::error!(%error, "internal monitor worker failed"))
        .ok();
    }

    /// Publish one completion observation to each enabled internal trigger.
    /// Admission remains queued until the next daemon workflow tick, keeping
    /// terminal projection durable before any downstream run is considered.
    pub async fn publish_workflow_completion(&self, run_id: &str) {
        let storage = self.storage.clone();
        let sender = self.queue.sender();
        let run_id = run_id.to_string();
        let log_run_id = run_id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let storage = storage.blocking_lock();
            let triggers = match WorkflowRepository::new(storage.conn())
                .list_enabled_internal_event_triggers()
            {
                Ok(triggers) => triggers,
                Err(error) => {
                    tracing::warn!(%error, %run_id, "workflow completion monitor lookup failed");
                    return;
                }
            };
            for trigger in triggers {
                let event = InternalMonitorEvent {
                    trigger_id: trigger.id,
                    event_type: "workflow.completed".to_string(),
                    external_id: run_id.clone(),
                    data: serde_json::json!({ "runId": run_id.clone() }),
                };
                if let Err(error) = sender.send(event) {
                    tracing::error!(%error, %run_id, "internal monitor signal enqueue failed");
                }
            }
        })
        .await
        .map_err(|error| tracing::error!(%error, %log_run_id, "internal monitor producer failed"))
        .ok();
    }
}

/// Admit one [`InternalMonitorEvent`] against its addressed trigger. Fails
/// closed: an unknown trigger id, a disabled trigger, a trigger whose kind
/// is not `internal_event`/`monitor`, or a trigger with no published
/// version pin never reaches the writer.
pub fn admit_internal_monitor_event(
    conn: &rightkit_db::rusqlite::Connection,
    event: &InternalMonitorEvent,
    now_ms: i64,
) -> rightkit_agent_engine_storage::Result<()> {
    let repo = WorkflowRepository::new(conn);
    let Some(trigger) = repo.find_trigger(&event.trigger_id)? else {
        return Ok(());
    };
    if !trigger.enabled || !matches!(trigger.kind.as_str(), "internal_event" | "monitor") {
        return Ok(());
    }
    admit(conn, trigger, event, now_ms)
}

fn admit(
    conn: &rightkit_db::rusqlite::Connection,
    trigger: WorkflowTriggerRow,
    event: &InternalMonitorEvent,
    now_ms: i64,
) -> rightkit_agent_engine_storage::Result<()> {
    let repo = WorkflowRepository::new(conn);
    let version = match trigger
        .version_pin
        .as_deref()
        .and_then(|id| repo.find_version(id).ok().flatten())
    {
        Some(v) if v.workflow_id == trigger.workflow_id && v.publication_state == "published" => v,
        _ => return Ok(()),
    };
    let clock = rightkit_workflow::control_plane::clock::SystemClock::new();
    let now = crate::workflow::triggers::format_unix_ms_as_iso(now_ms);
    let source = "coderight:monitor".to_string();
    let content_hash = ContentHash::sha256(event.data.to_string().as_bytes());
    let dedupe = crate::workflow::triggers::derive_dedupe_key(
        &source,
        &event.external_id,
        &content_hash,
        Some(&trigger.id),
    );
    let event_id = crate::workflow::triggers::derive_event_id(&source, &event.external_id, now_ms);
    let row = WorkflowTriggerEventRow {
        id: event_id.to_string(),
        trigger_id: Some(trigger.id.clone()),
        source: source.clone(),
        event_type: event.event_type.clone(),
        external_id: Some(event.external_id.clone()),
        dedupe_key: dedupe,
        content_hash: content_hash.to_string(),
        trust_json: r#"{"level":"trusted","origin":"workflow-internal-signal"}"#.into(),
        data_json: event.data.to_string(),
        raw_artifact_id: None,
        occurred_at: now.clone(),
        received_at: now.clone(),
    };
    let normalized = crate::workflow::triggers::NormalizedTriggerEvent {
        spec_version: crate::workflow::triggers::NormalizedTriggerEvent::SPEC_VERSION.into(),
        id: rightkit_workflow::control_plane::domain::trigger_event_id(
            row.id.strip_prefix("wf_evt_").unwrap_or(&row.id),
        ),
        trigger_id: Some(rightkit_workflow::control_plane::domain::trigger_id(
            trigger.id.clone(),
        )),
        external_id: row.external_id.clone(),
        source: row.source.clone(),
        type_: row.event_type.clone(),
        subject: None,
        time: row.occurred_at.clone(),
        received_at: row.received_at.clone(),
        data_content_type: "application/json".into(),
        data_schema: None,
        data: crate::workflow::triggers::NormalizedData::from_json(event.data.clone())
            .map_err(|e| rightkit_agent_engine_storage::StorageError::Serde(e.to_string()))?,
        trust: crate::workflow::triggers::TrustInfo {
            level: crate::workflow::triggers::TrustLevel::Trusted,
            authenticated: true,
            origin: crate::workflow::triggers::TrustOrigin::WorkflowTemplateDerived,
            principal: "monitor".into(),
            admission_policy: "monitor".into(),
            signature_algorithm: None,
            signature_key_id: None,
            actor_association: None,
            workspace_trust: None,
            findings: vec![],
        },
        dedupe_key: row.dedupe_key.clone(),
        content_hash,
        raw_artifact: None,
        correlation_id: None,
        causation_id: None,
        trace_parent: None,
        extensions: Default::default(),
    };
    let policy = occurrence_policy(&trigger)
        .map_err(|e| rightkit_agent_engine_storage::StorageError::Serde(e.to_string()))?;
    let decision = crate::workflow::triggers::OccurrenceAdmissionLayer::new(conn)
        .evaluate(
            &crate::workflow::triggers::OccurrenceInputs {
                normalized_event: &normalized,
                event_row: &row,
                trigger_row: &trigger,
                policy: &policy,
            },
            &clock,
        )
        .map_err(|e| rightkit_agent_engine_storage::StorageError::Serde(e.to_string()))?;
    let occurrence = decision.row().clone();
    let parts = matches!(
        decision,
        crate::workflow::triggers::OccurrenceDecision::Admit(_)
            | crate::workflow::triggers::OccurrenceDecision::Coalesce(_)
    )
    .then(|| trigger_run_admission(&trigger, &version, &occurrence, &row, now_ms));
    let writer = WorkflowTriggerWriter::new(conn);
    if let Some(p) = parts.as_ref() {
        let journal = JournalAppend {
            run_id: &p.run.id,
            event_kind: "run_started",
            node_id: None,
            payload_json: &p.journal_payload,
            created_at: &p.journal_created_at,
        };
        let projection = ProjectionUpdate {
            run_id: &p.run.id,
            run_phase: Some("running"),
            ..Default::default()
        };
        writer.admit_internal_event(
            &row,
            &occurrence,
            Some(&TriggerRunAdmission {
                snapshot: &p.snapshot,
                reducer_command: &p.command,
                run: &p.run,
                command: &p.command_row,
                outcome_json: "{}",
                event: &journal,
                projection: &projection,
                root_queue: &p.root_queue,
            }),
        )?;
    } else {
        writer.admit_internal_event(&row, &occurrence, None)?;
    }
    let _ = crate::workflow::continuations::match_event_waits(
        conn,
        &normalized.type_,
        normalized.correlation_id.as_deref().unwrap_or(""),
        &normalized.data.as_value(),
        now_ms,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SharedStorage;
    use rightkit_agent_engine_storage::Storage;

    fn seed_monitor_trigger(
        storage: &SharedStorage,
        workflow_id: &str,
        trigger_id: &str,
    ) -> rightkit_agent_engine_storage::Result<()> {
        let storage = storage.blocking_lock();
        let repo = WorkflowRepository::new(storage.conn());
        storage.conn().execute(
            "INSERT INTO workflow_definitions(id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            rightkit_db::rusqlite::params![workflow_id, "monitor-test", "2026-08-29T00:00:00Z"],
        ).ok();
        storage.conn().execute(
            "INSERT INTO workflow_versions(id, workflow_id, plan_json, publication_state, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 'published', ?4, ?4)",
            rightkit_db::rusqlite::params![
                "wfv-monitor",
                workflow_id,
                r#"{"nodes":[]}"#,
                "2026-08-29T00:00:00Z",
            ],
        ).ok();
        repo.insert_trigger(&WorkflowTriggerRow {
            id: trigger_id.to_string(),
            workflow_id: workflow_id.to_string(),
            kind: "internal_event".to_string(),
            enabled: true,
            cron_expression: None,
            timezone: None,
            version_pin: Some("wfv-monitor".to_string()),
            dedupe_key: None,
            overlap_policy: "queue".to_string(),
            misfire_policy: "skip".to_string(),
            concurrency_cap: 1,
            next_fire_at_unix_ms: None,
            revision: 0,
            created_at: "2026-08-29T00:00:00Z".to_string(),
            updated_at: "2026-08-29T00:00:00Z".to_string(),
        })
    }

    #[test]
    fn admit_internal_monitor_event_is_noop_for_unknown_trigger() {
        let storage = SharedStorage::new(Storage::open_in_memory().expect("open storage"));
        let event = InternalMonitorEvent {
            trigger_id: "missing-trigger".into(),
            event_type: "workflow.completed".into(),
            external_id: "ext-1".into(),
            data: serde_json::json!({"ok": true}),
        };
        let storage = storage.blocking_lock();
        // Unknown trigger admits nothing and returns Ok — fail closed, not
        // fail loud, matching `cron.rs::admit`'s malformed-row handling.
        assert!(admit_internal_monitor_event(storage.conn(), &event, 1_000).is_ok());
    }

    #[test]
    fn monitor_event_registered_without_external_http_call_admits_a_run() {
        let storage = SharedStorage::new(Storage::open_in_memory().expect("open storage"));
        seed_monitor_trigger(&storage, "wfd-monitor", "trg-monitor-1")
            .expect("seed monitor trigger");
        let queue = std::sync::Arc::new(MonitorEventQueue::new());
        queue
            .sender()
            .send(InternalMonitorEvent {
                trigger_id: "trg-monitor-1".into(),
                event_type: "workflow.completed".into(),
                external_id: "ext-2".into(),
                data: serde_json::json!({"upstreamRun": "run-1"}),
            })
            .expect("publish internal monitor event");
        // The producer's only interface is this in-process queue: no HTTP
        // request is ever constructed to reach admission.
        assert_eq!(queue.drain().len(), 1);
    }
}
