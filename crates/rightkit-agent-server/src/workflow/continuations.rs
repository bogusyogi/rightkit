//! WP-07 durable continuation worker for waits & local draft effects.
#![forbid(unsafe_code)]

use crate::api::SharedStorage;
use rightkit_agent_engine_storage::{
    JournalAppend, ProjectionUpdate, WorkflowAttentionRow, WorkflowCommandRow, WorkflowEffectClaim,
    WorkflowEffectOutcome, WorkflowJournalWriter, WorkflowRepository,
};
use rightkit_workflow::{
    clock::{Clock, SystemClock},
    domain::Revision,
    reducer::{CommandKind, EffectObservationKind, NodeCommand, NodeLifecycleState, NodePhase},
};

pub struct ContinuationWorker {
    storage: SharedStorage,
    clock: SystemClock,
    holder: String,
}

impl ContinuationWorker {
    pub fn new(storage: SharedStorage) -> Self {
        Self {
            storage,
            clock: SystemClock::new(),
            holder: format!("continuation-{}", std::process::id()),
        }
    }

    pub async fn tick(&self) {
        let storage = self.storage.clone();
        let now_ms = self.clock.now_unix_ms();
        let holder = self.holder.clone();
        let _ = tokio::task::spawn_blocking(move || drive(&storage, now_ms, &holder)).await;
    }
}

fn drive(storage: &SharedStorage, now_ms: i64, holder: &str) -> rightkit_agent_engine_storage::Result<()> {
    let storage = storage.blocking_lock();
    let repo = WorkflowRepository::new(storage.conn());
    for wait in repo.list_due_waits(now_ms, 64)? {
        resolve_wait(storage.conn(), &wait, now_ms)?;
    }
    for effect in repo.list_recoverable_effects(now_ms, 64)? {
        drive_effect(&storage, &effect, now_ms, holder)?;
    }
    for outbox in repo.list_due_outbox(&iso(now_ms), 64)? {
        drive_outbox(&storage, &outbox, now_ms, holder)?;
    }
    Ok(())
}

fn snapshot(
    repo: &WorkflowRepository<'_>,
    run_id: &str,
    node_id: &str,
) -> rightkit_agent_engine_storage::Result<NodeLifecycleState> {
    let node = repo.find_node_state(run_id, node_id)?.ok_or_else(|| {
        rightkit_agent_engine_storage::StorageError::NotFound(format!("workflow node {node_id}"))
    })?;
    let phase = match node.phase.as_str() {
        "ready" => NodePhase::Ready,
        "claimed" => NodePhase::Claimed,
        "running" => NodePhase::Running,
        "waiting" => NodePhase::Waiting,
        "completed" | "succeeded" => NodePhase::Succeeded,
        "skipped" => NodePhase::Skipped,
        "failed" => NodePhase::Failed,
        _ => NodePhase::Cancelled,
    };
    let generation = repo
        .latest_attempt_generation(run_id, node_id)?
        .ok_or_else(|| {
            rightkit_agent_engine_storage::StorageError::NotFound(format!(
                "workflow node attempt generation {run_id}:{node_id}"
            ))
        })?;
    Ok(NodeLifecycleState {
        node_id: node.node_id,
        revision: Revision(node.last_reducer_revision as u64),
        generation: generation as u64,
        phase,
    })
}

fn iso(now_ms: i64) -> String {
    crate::workflow::triggers::format_unix_ms_as_iso(now_ms)
}

/// Match a committed normalized event against exact open event waits. Callers
/// invoke this only after their event admission transaction returns.
pub(crate) fn match_event_waits(
    conn: &rightkit_db::rusqlite::Connection,
    topic: &str,
    correlation: &str,
    data: &serde_json::Value,
    now_ms: i64,
) -> rightkit_agent_engine_storage::Result<()> {
    let repo = WorkflowRepository::new(conn);
    for wait in repo.matching_event_waits(topic, correlation, data, 64)? {
        let _ = resolve_wait(conn, &wait, now_ms);
    }
    Ok(())
}

fn resolve_wait(
    conn: &rightkit_db::rusqlite::Connection,
    wait: &rightkit_agent_engine_storage::WorkflowWaitRow,
    now_ms: i64,
) -> rightkit_agent_engine_storage::Result<()> {
    let repo = WorkflowRepository::new(conn);
    let state = snapshot(&repo, &wait.run_id, &wait.node_id)?;
    let timed_out = wait
        .deadline_unix_ms
        .is_some_and(|deadline| deadline <= now_ms);
    let resolved = if timed_out { "timed_out" } else { "matched" };
    let phase = if timed_out { "failed" } else { "succeeded" };
    let command_kind = if timed_out {
        CommandKind::NodeFail
    } else {
        CommandKind::NodeComplete
    };
    let now = iso(now_ms);
    let command_id = format!("wfc-wait-{}-{}", wait.id, now_ms);
    let payload = serde_json::json!({"waitId":wait.id,"state":resolved}).to_string();
    let command = WorkflowCommandRow {
        run_id: wait.run_id.clone(),
        command_id: command_id.clone(),
        expected_revision: state.revision.0 as i64,
        command_kind: command_kind.as_str().into(),
        payload_json: payload.clone(),
        created_at: now.clone(),
    };
    let node_command = NodeCommand {
        command_id,
        kind: command_kind,
        node_id: wait.node_id.clone(),
        expected_revision: state.revision,
        generation: state.generation,
        approval_decision: None,
        effect_observation: None,
    };
    let event = JournalAppend {
        run_id: &wait.run_id,
        event_kind: if timed_out {
            "wait_timed_out"
        } else {
            "wait_matched"
        },
        node_id: Some(&wait.node_id),
        payload_json: &payload,
        created_at: &now,
    };
    let projection = ProjectionUpdate {
        run_id: &wait.run_id,
        node: Some((&wait.node_id, phase)),
        node_attempt: None,
        ..Default::default()
    };
    let attention = timed_out.then(|| WorkflowAttentionRow {
        id: format!("wfa-wait-{}", wait.id),
        run_id: Some(wait.run_id.clone()),
        node_id: Some(wait.node_id.clone()),
        effect_id: None,
        approval_id: None,
        kind: "wait_timeout".into(),
        state: "open".into(),
        severity: "warning".into(),
        title: "Workflow wait timed out".into(),
        summary: wait.id.clone(),
        deep_link_json: "{}".into(),
        dedupe_key: format!("wait:{}", wait.id),
        created_at: now.clone(),
        updated_at: now.clone(),
    });
    WorkflowJournalWriter::new(conn).apply_wait_resolution(
        &state,
        &node_command,
        &command,
        "{}",
        &event,
        &projection,
        &wait.id,
        "open",
        resolved,
        attention.as_ref(),
    )?;
    Ok(())
}

fn drive_effect(
    storage: &rightkit_agent_engine_storage::Storage,
    effect: &rightkit_agent_engine_storage::WorkflowEffectRow,
    now_ms: i64,
    holder: &str,
) -> rightkit_agent_engine_storage::Result<()> {
    if !matches!(
        effect.adapter.as_str(),
        "DraftDocument" | "DraftReview" | "DraftComment"
    ) {
        return Ok(());
    }
    let repo = WorkflowRepository::new(storage.conn());
    let state = snapshot(&repo, &effect.run_id, &effect.node_id)?;
    let fence = now_ms.max(1);
    let now = iso(now_ms);
    let claim = WorkflowEffectClaim {
        id: &effect.id,
        expected_state: &effect.state,
        holder,
        fence,
        idempotency_key: &effect.idempotency_key,
        now_unix_ms: now_ms,
        expires_at_unix_ms: now_ms.saturating_add(60_000),
        updated_at: &now,
    };
    if !repo.claim_effect(&claim)? {
        return Ok(());
    }
    let observation = if effect.state == "planned" {
        EffectObservationKind::Confirmed
    } else {
        EffectObservationKind::Reconciling
    };
    let outcome_state = if effect.state == "planned" {
        "confirmed"
    } else {
        "reconciling"
    };
    let projection_phase = if effect.state == "planned" {
        "completed"
    } else {
        "waiting"
    };
    let command_id = format!("wfc-effect-{}-{}", effect.id, fence);
    let payload =
        serde_json::json!({"effectId":effect.id,"state":outcome_state,"receipt":"local-draft"})
            .to_string();
    let command = WorkflowCommandRow {
        run_id: effect.run_id.clone(),
        command_id: command_id.clone(),
        expected_revision: state.revision.0 as i64,
        command_kind: "effect_observe".into(),
        payload_json: payload.clone(),
        created_at: now.clone(),
    };
    let node_command = NodeCommand {
        command_id,
        kind: CommandKind::EffectObserve,
        node_id: effect.node_id.clone(),
        expected_revision: state.revision,
        generation: state.generation,
        approval_decision: None,
        effect_observation: Some(observation),
    };
    let event = JournalAppend {
        run_id: &effect.run_id,
        event_kind: if outcome_state == "confirmed" {
            "effect_confirmed"
        } else {
            "effect_reconciling"
        },
        node_id: Some(&effect.node_id),
        payload_json: &payload,
        created_at: &now,
    };
    let projection = ProjectionUpdate {
        run_id: &effect.run_id,
        node: Some((&effect.node_id, projection_phase)),
        node_attempt: None,
        ..Default::default()
    };
    let receipt = serde_json::json!({"adapter":"draft","idempotencyKey":effect.idempotency_key,"state":"staged"}).to_string();
    let outcome = WorkflowEffectOutcome {
        id: &effect.id,
        idempotency_key: &effect.idempotency_key,
        holder,
        fence,
        state: outcome_state,
        receipt_json: (outcome_state == "confirmed").then_some(receipt.as_str()),
        outcome_json: Some(&payload),
        reconciliation_json: (outcome_state == "reconciling").then_some(payload.as_str()),
        failure_json: None,
        updated_at: &now,
    };
    WorkflowJournalWriter::new(storage.conn()).apply_effect_observation(
        &state,
        &node_command,
        &command,
        "{}",
        &event,
        &projection,
        &outcome,
        None,
        None,
    )?;
    Ok(())
}

fn drive_outbox(
    storage: &rightkit_agent_engine_storage::Storage,
    outbox: &rightkit_agent_engine_storage::WorkflowOutboxRow,
    now_ms: i64,
    holder: &str,
) -> rightkit_agent_engine_storage::Result<()> {
    let repo = WorkflowRepository::new(storage.conn());
    let now = iso(now_ms);
    let fence = now_ms.max(1);
    if !repo.claim_outbox(
        &outbox.id,
        holder,
        fence,
        &now,
        &iso(now_ms.saturating_add(60_000)),
    )? {
        return Ok(());
    }
    if outbox.kind == "approval"
        && outbox.topic == "workflow.approval.requested"
        && serde_json::from_str::<serde_json::Value>(&outbox.payload_json).is_ok()
    {
        tracing::info!(outbox_id = %outbox.id, run_id = %outbox.run_id, kind = %outbox.kind, topic = %outbox.topic, "workflow local outbox published");
        let _ = repo.ack_outbox(&outbox.id, holder, fence, &now)?;
        return Ok(());
    }
    let error = serde_json::json!({"class":"unsupported_or_malformed_local_outbox","outboxId":outbox.id,"kind":outbox.kind,"topic":outbox.topic}).to_string();
    let attention = WorkflowAttentionRow {
        id: format!("wfa-outbox-{}", outbox.id),
        run_id: Some(outbox.run_id.clone()),
        node_id: None,
        effect_id: None,
        approval_id: None,
        kind: "dead_letter".into(),
        state: "open".into(),
        severity: "error".into(),
        title: "Workflow outbox needs review".into(),
        summary: format!("{}:{}", outbox.kind, outbox.topic),
        deep_link_json: serde_json::json!({"runId":outbox.run_id,"outboxId":outbox.id}).to_string(),
        dedupe_key: format!("outbox:{}", outbox.id),
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    let _ = repo.dead_letter_outbox(&outbox.id, holder, fence, &error, &now, &attention)?;
    Ok(())
}
