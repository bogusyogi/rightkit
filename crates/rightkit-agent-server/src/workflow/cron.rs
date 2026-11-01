//! WP-06 cron trigger cursor worker.
#![forbid(unsafe_code)]
use crate::{
    api::SharedStorage,
    api_workflows::routes::{occurrence_policy, trigger_run_admission},
};
use rightkit_agent_engine_storage::{
    JournalAppend, ProjectionUpdate, TriggerRunAdmission, WorkflowRepository,
    WorkflowTriggerEventRow, WorkflowTriggerWriter,
};
use rightkit_workflow::{
    clock::{Clock, SystemClock},
    domain::ContentHash,
    trigger::DstResolutionPolicy,
};

pub struct CronWorker {
    storage: SharedStorage,
    clock: SystemClock,
}
impl CronWorker {
    pub fn new(storage: SharedStorage) -> Self {
        Self {
            storage,
            clock: SystemClock::new(),
        }
    }
    pub async fn tick(&self) {
        let storage = self.storage.clone();
        let now = self.clock.now_unix_ms();
        let _ = tokio::task::spawn_blocking(move || drive(&storage, now)).await;
    }
}

fn drive(storage: &SharedStorage, now: i64) -> rightkit_agent_engine_storage::Result<()> {
    let storage = storage.blocking_lock();
    let repo = WorkflowRepository::new(storage.conn());
    for trigger in repo.list_due_cron_triggers(now, 64)? {
        let _ = admit(storage.conn(), trigger, now);
    }
    Ok(())
}

fn admit(
    conn: &rightkit_db::rusqlite::Connection,
    trigger: rightkit_agent_engine_storage::WorkflowTriggerRow,
    now: i64,
) -> rightkit_agent_engine_storage::Result<()> {
    let due = match trigger.next_fire_at_unix_ms {
        Some(x) => x,
        None => return Ok(()),
    };
    let cron = match trigger.cron_expression.as_deref() {
        Some(x) => x,
        None => return Ok(()),
    };
    let zone = match trigger.timezone.as_deref() {
        Some(x) => x,
        None => return Ok(()),
    };
    let repo = WorkflowRepository::new(conn);
    let version = match trigger
        .version_pin
        .as_deref()
        .and_then(|id| repo.find_version(id).ok().flatten())
    {
        Some(v) if v.workflow_id == trigger.workflow_id && v.publication_state == "published" => v,
        _ => return Ok(()),
    };
    let next = match crate::workflow::triggers::occurrence::resolve_cron_due(
        cron,
        zone,
        due,
        DstResolutionPolicy::SkipGap,
    ) {
        Ok(Some(x)) => x,
        _ => return Ok(()),
    };
    let clock = SystemClock::new();
    let at = crate::workflow::triggers::format_unix_ms_as_iso(due);
    let source = "coderight:cron".to_string();
    let external = format!("{}:{due}", trigger.id);
    let hash = ContentHash::sha256(external.as_bytes());
    let dedupe =
        crate::workflow::triggers::derive_dedupe_key(&source, &external, &hash, Some(&trigger.id));
    let event = WorkflowTriggerEventRow {
        id: crate::workflow::triggers::derive_event_id(&source, &external, due).to_string(),
        trigger_id: Some(trigger.id.clone()),
        source,
        event_type: "schedule.cron".into(),
        external_id: Some(external),
        dedupe_key: dedupe,
        content_hash: hash.to_string(),
        trust_json: r#"{"level":"trusted","origin":"workflow-template-derived"}"#.into(),
        data_json: serde_json::json!({"scheduledAt":at}).to_string(),
        raw_artifact_id: None,
        occurred_at: at.clone(),
        received_at: crate::workflow::triggers::format_unix_ms_as_iso(now),
    };
    let normalized = crate::workflow::triggers::NormalizedTriggerEvent {
        spec_version: crate::workflow::triggers::NormalizedTriggerEvent::SPEC_VERSION.into(),
        id: rightkit_workflow::domain::trigger_event_id(
            event.id.strip_prefix("wf_evt_").unwrap_or(&event.id),
        ),
        trigger_id: Some(rightkit_workflow::domain::trigger_id(trigger.id.clone())),
        external_id: event.external_id.clone(),
        source: event.source.clone(),
        type_: event.event_type.clone(),
        subject: None,
        time: event.occurred_at.clone(),
        received_at: event.received_at.clone(),
        data_content_type: "application/json".into(),
        data_schema: None,
        data: crate::workflow::triggers::NormalizedData::from_json(
            serde_json::json!({"scheduledAt":at}),
        )
        .map_err(|e| rightkit_agent_engine_storage::StorageError::Serde(e.to_string()))?,
        trust: crate::workflow::triggers::TrustInfo {
            level: crate::workflow::triggers::TrustLevel::Trusted,
            authenticated: true,
            origin: crate::workflow::triggers::TrustOrigin::WorkflowTemplateDerived,
            principal: "cron".into(),
            admission_policy: "cron".into(),
            signature_algorithm: None,
            signature_key_id: None,
            actor_association: None,
            workspace_trust: None,
            findings: vec![],
        },
        dedupe_key: event.dedupe_key.clone(),
        content_hash: hash,
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
                event_row: &event,
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
    .then(|| trigger_run_admission(&trigger, &version, &occurrence, &event, now));
    let writer = WorkflowTriggerWriter::new(conn);
    let outcome = if let Some(p) = parts.as_ref() {
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
        let run = TriggerRunAdmission {
            snapshot: &p.snapshot,
            reducer_command: &p.command,
            run: &p.run,
            command: &p.command_row,
            outcome_json: "{}",
            event: &journal,
            projection: &projection,
            root_queue: &p.root_queue,
        };
        writer.admit_scheduled_event(
            &event,
            &occurrence,
            Some(&run),
            due,
            next,
            &event.received_at,
        )
    } else {
        writer.admit_scheduled_event(&event, &occurrence, None, due, next, &event.received_at)
    }?;
    if !outcome.duplicate {
        let _ = crate::workflow::continuations::match_event_waits(
            conn,
            &normalized.type_,
            normalized.correlation_id.as_deref().unwrap_or(""),
            &normalized.data.as_value(),
            now,
        );
    }
    Ok(())
}
