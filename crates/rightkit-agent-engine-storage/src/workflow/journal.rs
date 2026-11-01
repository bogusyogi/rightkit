//! The journal writer: the only code allowed to append rows to
//! `workflow_journal` and apply the matching `workflow_runs` /
//! `workflow_node_states` / `workflow_run_lease` projection updates
//! inside the same SQLite transaction.

use super::models::*;
use crate::error::{Result, StorageError};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension, Transaction};

pub struct WorkflowJournalWriter<'a> {
    conn: &'a Connection,
}

impl<'a> WorkflowJournalWriter<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// The atomic append. The order inside the transaction is:
    /// 1. Insert `run_run` projection fields (UPSERT) if any are set.
    /// 2. Compute `seq = MAX(seq) + 1` and `revision = run.lifecycle_revision`
    ///    (or the caller's `run_lifecycle_revision` override).
    /// 3. Insert the `journal` row at that `(run_id, seq)`.
    /// 4. Apply node-state upsert(s) and lease rows if requested.
    /// 5. Commit.
    pub fn append_with_projection(
        &self,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
    ) -> Result<JournalAppendOutcome> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let outcome = Self::append_with_projection_in_tx(&tx, event, projection)?;
        tx.commit()?;
        Ok(outcome)
    }

    pub(crate) fn append_with_projection_in_tx(
        tx: &Transaction<'_>,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
    ) -> Result<JournalAppendOutcome> {
        // 1. Read current run row if it exists; auto-create on first event.
        let existing_run: Option<WorkflowRunRow> = tx
            .query_row(
                "SELECT id, workflow_id, workflow_version_id, parent_run_id,
                        lifecycle_phase, lifecycle_revision,
                        initiated_by_kind, initiated_by_ref,
                        trigger_payload_json, input_json,
                        started_at, terminal_at, terminal_failure_class,
                        runs_root_durable
                 FROM workflow_runs WHERE id = ?1",
                params![projection.run_id],
                |row| {
                    Ok(WorkflowRunRow {
                        id: row.get(0)?,
                        workflow_id: row.get(1)?,
                        workflow_version_id: row.get(2)?,
                        parent_run_id: row.get(3)?,
                        lifecycle_phase: row.get(4)?,
                        lifecycle_revision: row.get(5)?,
                        initiated_by_kind: row.get(6)?,
                        initiated_by_ref: row.get(7)?,
                        trigger_payload_json: row.get(8)?,
                        input_json: row.get(9)?,
                        started_at: row.get(10)?,
                        terminal_at: row.get(11)?,
                        terminal_failure_class: row.get(12)?,
                        runs_root_durable: row.get::<_, i64>(13)? != 0,
                    })
                },
            )
            .optional()?;

        // 2. Upsert run row when first event creates it.
        let next_revision: i64 = if let Some(existing) = &existing_run {
            let next = existing.lifecycle_revision + 1;
            apply_run_projection(tx, existing, projection)?;
            next
        } else {
            // First event for this run — caller must have provided a phase
            // and the run identity. We synthesize the persist on the spot.
            insert_run_on_first_event(tx, projection)?;
            1
        };

        // 3. Compute seq.
        let next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM workflow_journal WHERE run_id = ?1",
            params![projection.run_id],
            |row| row.get::<_, i64>(0),
        )?;

        // 4. Insert journal row.
        tx.execute(
            "INSERT INTO workflow_journal(
                run_id, seq, revision, event_kind, node_id,
                payload_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.run_id,
                next_seq,
                next_revision,
                event.event_kind,
                event.node_id,
                event.payload_json,
                event.created_at,
            ],
        )?;

        // 5. Apply node + lease projections.
        if let Some((node_id, phase)) = projection.node {
            upsert_node_state(
                tx,
                projection.run_id,
                node_id,
                phase,
                next_seq,
                next_revision,
                projection,
            )?;
        }
        if let Some(lease) = projection.run_lease {
            upsert_run_lease(tx, lease)?;
        }
        if let Some(release) = projection.release_run_lease {
            let changed = tx.execute(
                "DELETE FROM workflow_run_lease
                   WHERE run_id = ?1 AND lease_holder = ?2 AND lease_fence = ?3",
                params![release.run_id, release.holder, release.fence],
            )?;
            if changed == 0 {
                return Err(StorageError::NotFound(format!(
                    "stale workflow lease {}:{}",
                    release.run_id, release.fence
                )));
            }
        }

        Ok(JournalAppendOutcome {
            seq: next_seq,
            revision: next_revision,
        })
    }

    /// Idempotently commit one reduced command. A duplicate returns its
    /// original outcome; every authoritative write shares one transaction.
    pub(crate) fn apply_command(
        &self,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        queue: Option<&WorkflowQueueRow>,
        wait: Option<&WorkflowWaitRow>,
        approval: Option<&WorkflowApprovalRow>,
        effect: Option<&WorkflowEffectRow>,
        attention: Option<&WorkflowAttentionRow>,
        outbox: Option<&WorkflowOutboxRow>,
    ) -> Result<WorkflowCommandApplyOutcome> {
        self.apply_command_at_boundary(
            command,
            outcome_json,
            event,
            projection,
            queue,
            wait,
            approval,
            effect,
            attention,
            outbox,
            None,
        )
    }

    fn apply_command_at_boundary(
        &self,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        queue: Option<&WorkflowQueueRow>,
        wait: Option<&WorkflowWaitRow>,
        approval: Option<&WorkflowApprovalRow>,
        effect: Option<&WorkflowEffectRow>,
        attention: Option<&WorkflowAttentionRow>,
        outbox: Option<&WorkflowOutboxRow>,
        fail_at: Option<WorkflowCommandBoundary>,
    ) -> Result<WorkflowCommandApplyOutcome> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO workflow_commands(run_id,command_id,expected_revision,command_kind,payload_json,created_at,outcome_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![command.run_id,command.command_id,command.expected_revision,command.command_kind,command.payload_json,command.created_at,outcome_json],
        )?;
        if inserted == 0 {
            let prior = tx.query_row(
                "SELECT outcome_json FROM workflow_commands WHERE run_id=?1 AND command_id=?2",
                params![command.run_id, command.command_id],
                |row| row.get(0),
            )?;
            tx.commit()?;
            return Ok(WorkflowCommandApplyOutcome::Duplicate {
                outcome_json: prior,
            });
        }
        fail_if(fail_at, WorkflowCommandBoundary::Command)?;
        let applied = Self::append_with_projection_in_tx(&tx, event, projection)?;
        fail_if(fail_at, WorkflowCommandBoundary::JournalProjection)?;
        if let Some(row) = queue {
            tx.execute("INSERT INTO workflow_queue(run_id,node_id,available_at,priority) VALUES (?1,?2,?3,?4) ON CONFLICT(run_id,node_id) DO UPDATE SET available_at=excluded.available_at,priority=excluded.priority", params![row.run_id,row.node_id,row.available_at,row.priority])?;
        }
        if let Some(row) = wait {
            tx.execute("INSERT INTO workflow_waits(id,run_id,node_id,kind,state,correlation_key,topic,filter_expression,filter_hash,wake_at_unix_ms,deadline_unix_ms,matched_count,max_matches,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)", params![row.id,row.run_id,row.node_id,row.kind,row.state,row.correlation_key,row.topic,row.filter_expression,row.filter_hash,row.wake_at_unix_ms,row.deadline_unix_ms,row.matched_count,row.max_matches,row.created_at,row.updated_at])?;
        }
        if let Some(row) = approval {
            tx.execute("INSERT INTO workflow_approvals(id,run_id,node_id,version_id,inputs_hash,outputs_hash,artifact_refs_json,capabilities_json,effect_request_hash,expires_at_unix_ms,role,one_shot,state,decision_id,decision_actor,decision_reason,decision_signature,decided_at_unix_ms,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)", params![row.id,row.run_id,row.node_id,row.version_id,row.inputs_hash,row.outputs_hash,row.artifact_refs_json,row.capabilities_json,row.effect_request_hash,row.expires_at_unix_ms,row.role,row.one_shot as i64,row.state,row.decision_id,row.decision_actor,row.decision_reason,row.decision_signature,row.decided_at_unix_ms,row.created_at,row.updated_at])?;
        }
        if let Some(row) = effect {
            tx.execute("INSERT INTO workflow_effects(id,run_id,node_id,adapter,operation,destination_hash,payload_hash,idempotency_key,state,attempt_count,max_attempts,receipt_json,outcome_json,reconciliation_json,failure_json,requested_at_unix_ms,ttl_unix_ms,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)", params![row.id,row.run_id,row.node_id,row.adapter,row.operation,row.destination_hash,row.payload_hash,row.idempotency_key,row.state,row.attempt_count,row.max_attempts,row.receipt_json,row.outcome_json,row.reconciliation_json,row.failure_json,row.requested_at_unix_ms,row.ttl_unix_ms,row.created_at,row.updated_at])?;
        }
        fail_if(fail_at, WorkflowCommandBoundary::Queue)?;
        if let Some(row) = attention {
            tx.execute("INSERT INTO workflow_attention(id,run_id,node_id,effect_id,approval_id,kind,state,severity,title,summary,deep_link_json,dedupe_key,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![row.id,row.run_id,row.node_id,row.effect_id,row.approval_id,row.kind,row.state,row.severity,row.title,row.summary,row.deep_link_json,row.dedupe_key,row.created_at,row.updated_at])?;
        }
        fail_if(fail_at, WorkflowCommandBoundary::Attention)?;
        if let Some(row) = outbox {
            tx.execute("INSERT INTO workflow_outbox(id,run_id,kind,topic,dedupe_key,payload_json,state,available_at,attempt_count,max_attempts,last_error_json,next_attempt_at,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![row.id,row.run_id,row.kind,row.topic,row.dedupe_key,row.payload_json,row.state,row.available_at,row.attempt_count,row.max_attempts,row.last_error_json,row.next_attempt_at,row.created_at,row.updated_at])?;
        }
        fail_if(fail_at, WorkflowCommandBoundary::Outbox)?;
        tx.commit()?;
        Ok(WorkflowCommandApplyOutcome::Applied(applied))
    }

    /// Typed lifecycle entrypoint. Reducer rejection precedes every durable
    /// write, keeping stale/illegal/terminal commands out of journal state.
    fn apply_reduced_command(
        &self,
        reduced: &rightkit_workflow::control_plane::reducer::ReducerOutcome,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        queue: Option<&WorkflowQueueRow>,
        wait: Option<&WorkflowWaitRow>,
        approval: Option<&WorkflowApprovalRow>,
        effect: Option<&WorkflowEffectRow>,
        attention: Option<&WorkflowAttentionRow>,
        outbox: Option<&WorkflowOutboxRow>,
    ) -> Result<WorkflowCommandApplyOutcome> {
        if let rightkit_workflow::control_plane::reducer::ReducerOutcome::Rejected {
            reason, ..
        } = reduced
        {
            return Err(StorageError::Serde(format!(
                "workflow reducer rejected command: {reason}"
            )));
        }
        self.apply_command(
            command,
            outcome_json,
            event,
            projection,
            queue,
            wait,
            approval,
            effect,
            attention,
            outbox,
        )
    }

    /// Public lifecycle entrypoint. Storage reduces the exact command against
    /// the supplied snapshot; callers cannot smuggle an unrelated `Applied`
    /// token across runs or revisions.
    pub fn apply_command_from_snapshot(
        &self,
        snapshot: &rightkit_workflow::control_plane::domain::LifecycleState,
        reducer_command: &rightkit_workflow::control_plane::reducer::Command,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        queue: Option<&WorkflowQueueRow>,
        wait: Option<&WorkflowWaitRow>,
        approval: Option<&WorkflowApprovalRow>,
        effect: Option<&WorkflowEffectRow>,
        attention: Option<&WorkflowAttentionRow>,
        outbox: Option<&WorkflowOutboxRow>,
    ) -> Result<WorkflowCommandApplyOutcome> {
        if reducer_command.command_id != command.command_id
            || reducer_command.kind != command.command_kind
            || reducer_command.expected_revision.0 != command.expected_revision as u64
            || reducer_command.payload_json != command.payload_json
            || reducer_command.run_id.stable().as_str() != command.run_id
            || event.run_id != command.run_id
            || projection.run_id != command.run_id
        {
            return Err(StorageError::Serde(
                "workflow command/reducer snapshot mismatch".into(),
            ));
        }
        let reduced = rightkit_workflow::control_plane::reducer::reduce(snapshot, reducer_command);
        self.apply_reduced_command(
            &reduced,
            command,
            outcome_json,
            event,
            projection,
            queue,
            wait,
            approval,
            effect,
            attention,
            outbox,
        )
    }

    /// Node lifecycle counterpart to [`Self::apply_command_from_snapshot`].
    /// It binds exact node id, expected node revision and lease generation
    /// before one atomic command/journal/projection/mutation transaction.
    pub fn apply_node_command_from_snapshot(
        &self,
        snapshot: &rightkit_workflow::control_plane::reducer::NodeLifecycleState,
        node_command: &rightkit_workflow::control_plane::reducer::NodeCommand,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        queue: Option<&WorkflowQueueRow>,
        wait: Option<&WorkflowWaitRow>,
        approval: Option<&WorkflowApprovalRow>,
        effect: Option<&WorkflowEffectRow>,
        attention: Option<&WorkflowAttentionRow>,
        outbox: Option<&WorkflowOutboxRow>,
    ) -> Result<WorkflowCommandApplyOutcome> {
        let Some((projection_node, _)) = projection.node else {
            return Err(StorageError::Serde(
                "node command requires node projection".into(),
            ));
        };
        if node_command.node_id != snapshot.node_id
            || node_command.node_id != projection_node
            || event.node_id != Some(projection_node)
            || node_command.command_id != command.command_id
            || command.run_id != projection.run_id
            || command.expected_revision < 0
            || node_command.expected_revision.0 != command.expected_revision as u64
            || node_command.kind.as_str() != command.command_kind
        {
            return Err(StorageError::Serde(
                "workflow node command/snapshot mismatch".into(),
            ));
        }
        if latest_attempt_generation_conn(self.conn, &command.run_id, &node_command.node_id)?
            != Some(node_command.generation as i64)
        {
            return Err(StorageError::Serde(
                "workflow node command stale attempt generation".into(),
            ));
        }
        let reduced =
            rightkit_workflow::control_plane::reducer::reduce_node(snapshot, node_command)
                .map_err(|reason| {
                    StorageError::Serde(format!("workflow node reducer rejected command: {reason}"))
                })?;
        if projection.node.map(|(_, phase)| phase) != Some(node_phase_storage(reduced.phase)) {
            return Err(StorageError::Serde(
                "workflow node projection disagrees with reducer".into(),
            ));
        }
        self.apply_command(
            command,
            outcome_json,
            event,
            projection,
            queue,
            wait,
            approval,
            effect,
            attention,
            outbox,
        )
    }

    pub fn apply_wait_resolution(
        &self,
        snapshot: &rightkit_workflow::control_plane::reducer::NodeLifecycleState,
        node_command: &rightkit_workflow::control_plane::reducer::NodeCommand,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        wait_id: &str,
        expected_state: &str,
        resolved_state: &str,
        attention: Option<&WorkflowAttentionRow>,
    ) -> Result<WorkflowCommandApplyOutcome> {
        let valid_transition = matches!(
            (resolved_state, node_command.kind),
            (
                "matched",
                rightkit_workflow::control_plane::reducer::CommandKind::NodeComplete
            ) | (
                "timed_out" | "cancelled",
                rightkit_workflow::control_plane::reducer::CommandKind::NodeFail
            )
        );
        if !valid_transition {
            return Err(StorageError::Serde(
                "invalid wait resolution command".into(),
            ));
        }
        let Some((projection_node, projection_phase)) = projection.node else {
            return Err(StorageError::Serde(
                "wait resolution requires node projection".into(),
            ));
        };
        let reduced =
            rightkit_workflow::control_plane::reducer::reduce_node(snapshot, node_command)
                .map_err(|reason| {
                    StorageError::Serde(format!("wait reducer rejected command: {reason}"))
                })?;
        if node_command.command_id != command.command_id
            || node_command.node_id != projection_node
            || event.node_id != Some(projection_node)
            || command.run_id != projection.run_id
            || node_command.expected_revision.0 != command.expected_revision as u64
            || node_command.kind.as_str() != command.command_kind
            || projection_phase != node_phase_storage(reduced.phase)
        {
            return Err(StorageError::Serde("wait command binding mismatch".into()));
        }
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        if latest_attempt_generation_conn(&tx, &command.run_id, &node_command.node_id)?
            != Some(node_command.generation as i64)
        {
            return Err(StorageError::Serde(
                "wait resolution stale attempt generation".into(),
            ));
        }
        if tx.execute("INSERT OR IGNORE INTO workflow_commands(run_id,command_id,expected_revision,command_kind,payload_json,created_at,outcome_json) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![command.run_id,command.command_id,command.expected_revision,command.command_kind,command.payload_json,command.created_at,outcome_json])? == 0 {
            let prior = tx.query_row("SELECT outcome_json FROM workflow_commands WHERE run_id=?1 AND command_id=?2", params![command.run_id,command.command_id], |row| row.get(0))?;
            tx.commit()?;
            return Ok(WorkflowCommandApplyOutcome::Duplicate { outcome_json: prior });
        }
        let applied = Self::append_with_projection_in_tx(&tx, event, projection)?;
        if tx.execute("UPDATE workflow_waits SET state=?1,matched_count=matched_count+CASE WHEN ?1='matched' THEN 1 ELSE 0 END,updated_at=?2 WHERE id=?3 AND run_id=?4 AND node_id=?5 AND state=?6", params![resolved_state,command.created_at,wait_id,command.run_id,node_command.node_id,expected_state])? != 1 {
            return Err(StorageError::Serde("wait resolution lost state race".into()));
        }
        if let Some(row) = attention {
            tx.execute("INSERT INTO workflow_attention(id,run_id,node_id,effect_id,approval_id,kind,state,severity,title,summary,deep_link_json,dedupe_key,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![row.id,row.run_id,row.node_id,row.effect_id,row.approval_id,row.kind,row.state,row.severity,row.title,row.summary,row.deep_link_json,row.dedupe_key,row.created_at,row.updated_at])?;
        }
        tx.commit()?;
        Ok(WorkflowCommandApplyOutcome::Applied(applied))
    }

    pub fn apply_effect_observation(
        &self,
        snapshot: &rightkit_workflow::control_plane::reducer::NodeLifecycleState,
        node_command: &rightkit_workflow::control_plane::reducer::NodeCommand,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        effect: &WorkflowEffectOutcome<'_>,
        attention: Option<&WorkflowAttentionRow>,
        outbox: Option<&WorkflowOutboxRow>,
    ) -> Result<WorkflowCommandApplyOutcome> {
        if node_command.kind
            != rightkit_workflow::control_plane::reducer::CommandKind::EffectObserve
            || !matches!(
                effect.state,
                "confirmed" | "failed" | "outcome_unknown" | "reconciling" | "cancelled"
            )
            || node_command.effect_observation != Some(match effect.state {
                "confirmed" => {
                    rightkit_workflow::control_plane::reducer::EffectObservationKind::Confirmed
                }
                "failed" => {
                    rightkit_workflow::control_plane::reducer::EffectObservationKind::Failed
                }
                "outcome_unknown" => {
                    rightkit_workflow::control_plane::reducer::EffectObservationKind::OutcomeUnknown
                }
                "reconciling" => {
                    rightkit_workflow::control_plane::reducer::EffectObservationKind::Reconciling
                }
                "cancelled" => {
                    rightkit_workflow::control_plane::reducer::EffectObservationKind::Cancelled
                }
                _ => unreachable!(),
            })
        {
            return Err(StorageError::Serde(
                "invalid effect observation command".into(),
            ));
        }
        let Some((projection_node, projection_phase)) = projection.node else {
            return Err(StorageError::Serde(
                "effect observation requires node projection".into(),
            ));
        };
        let reduced =
            rightkit_workflow::control_plane::reducer::reduce_node(snapshot, node_command)
                .map_err(|reason| {
                    StorageError::Serde(format!("effect reducer rejected command: {reason}"))
                })?;
        if node_command.command_id != command.command_id
            || node_command.node_id != projection_node
            || event.node_id != Some(projection_node)
            || command.run_id != projection.run_id
            || node_command.expected_revision.0 != command.expected_revision as u64
            || node_command.kind.as_str() != command.command_kind
            || projection_phase != node_phase_storage(reduced.phase)
        {
            return Err(StorageError::Serde(
                "effect observation binding mismatch".into(),
            ));
        }
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        if latest_attempt_generation_conn(&tx, &command.run_id, &node_command.node_id)?
            != Some(node_command.generation as i64)
        {
            return Err(StorageError::Serde(
                "effect observation stale attempt generation".into(),
            ));
        }
        if tx.execute("INSERT OR IGNORE INTO workflow_commands(run_id,command_id,expected_revision,command_kind,payload_json,created_at,outcome_json) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![command.run_id,command.command_id,command.expected_revision,command.command_kind,command.payload_json,command.created_at,outcome_json])? == 0 {
            let prior = tx.query_row("SELECT outcome_json FROM workflow_commands WHERE run_id=?1 AND command_id=?2", params![command.run_id,command.command_id], |row| row.get(0))?;
            tx.commit()?;
            return Ok(WorkflowCommandApplyOutcome::Duplicate { outcome_json: prior });
        }
        let applied = Self::append_with_projection_in_tx(&tx, event, projection)?;
        if tx.execute("UPDATE workflow_effects SET state=?1,receipt_json=?2,outcome_json=?3,reconciliation_json=?4,failure_json=?5,updated_at=?6,claim_expires_at_unix_ms=NULL WHERE id=?7 AND run_id=?8 AND node_id=?9 AND idempotency_key=?10 AND claim_holder=?11 AND claim_fence=?12 AND state IN ('dispatching','reconciling','outcome_unknown')", params![effect.state,effect.receipt_json,effect.outcome_json,effect.reconciliation_json,effect.failure_json,effect.updated_at,effect.id,command.run_id,node_command.node_id,effect.idempotency_key,effect.holder,effect.fence])? != 1 {
            return Err(StorageError::Serde("effect observation lost fence".into()));
        }
        if let Some(row) = attention {
            tx.execute("INSERT INTO workflow_attention(id,run_id,node_id,effect_id,approval_id,kind,state,severity,title,summary,deep_link_json,dedupe_key,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![row.id,row.run_id,row.node_id,row.effect_id,row.approval_id,row.kind,row.state,row.severity,row.title,row.summary,row.deep_link_json,row.dedupe_key,row.created_at,row.updated_at])?;
        }
        if let Some(row) = outbox {
            tx.execute("INSERT INTO workflow_outbox(id,run_id,kind,topic,dedupe_key,payload_json,state,available_at,attempt_count,max_attempts,last_error_json,next_attempt_at,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![row.id,row.run_id,row.kind,row.topic,row.dedupe_key,row.payload_json,row.state,row.available_at,row.attempt_count,row.max_attempts,row.last_error_json,row.next_attempt_at,row.created_at,row.updated_at])?;
        }
        tx.commit()?;
        Ok(WorkflowCommandApplyOutcome::Applied(applied))
    }

    /// Atomically admits a previously unseen run. Full immutable run metadata,
    /// command idempotency receipt, reducer journal/projection and initial
    /// ready root are committed together; a duplicate command returns its
    /// original receipt without creating another run.
    pub fn admit_run_from_snapshot(
        &self,
        snapshot: &rightkit_workflow::control_plane::domain::LifecycleState,
        reducer_command: &rightkit_workflow::control_plane::reducer::Command,
        run: &WorkflowRunRow,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        root_queue: &WorkflowQueueRow,
    ) -> Result<WorkflowCommandApplyOutcome> {
        if !matches!(reducer_command.kind.as_str(), "admit" | "start")
            || reducer_command.command_id != command.command_id
            || reducer_command.kind != command.command_kind
            || reducer_command.expected_revision.0 != command.expected_revision as u64
            || reducer_command.payload_json != command.payload_json
            || reducer_command.run_id.stable().as_str() != run.id
            || command.run_id != run.id
            || event.run_id != run.id
            || projection.run_id != run.id
            || root_queue.run_id != run.id
        {
            return Err(StorageError::Serde(
                "workflow admission command/run mismatch".into(),
            ));
        }
        let reduced = rightkit_workflow::control_plane::reducer::reduce(snapshot, reducer_command);
        if let rightkit_workflow::control_plane::reducer::ReducerOutcome::Rejected {
            reason, ..
        } = reduced
        {
            return Err(StorageError::Serde(format!(
                "workflow reducer rejected admission: {reason}"
            )));
        }
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        if let Some(prior) = tx
            .query_row(
                "SELECT outcome_json FROM workflow_commands WHERE run_id=?1 AND command_id=?2",
                params![command.run_id, command.command_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            tx.commit()?;
            return Ok(WorkflowCommandApplyOutcome::Duplicate {
                outcome_json: prior,
            });
        }
        tx.execute(
            "INSERT INTO workflow_runs(id,workflow_id,workflow_version_id,parent_run_id,lifecycle_phase,lifecycle_revision,initiated_by_kind,initiated_by_ref,trigger_payload_json,input_json,started_at,terminal_at,terminal_failure_class,runs_root_durable) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![run.id,run.workflow_id,run.workflow_version_id,run.parent_run_id,run.lifecycle_phase,run.lifecycle_revision,run.initiated_by_kind,run.initiated_by_ref,run.trigger_payload_json,run.input_json,run.started_at,run.terminal_at,run.terminal_failure_class,run.runs_root_durable as i64],
        )?;
        tx.execute(
            "INSERT INTO workflow_commands(run_id,command_id,expected_revision,command_kind,payload_json,created_at,outcome_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![command.run_id,command.command_id,command.expected_revision,command.command_kind,command.payload_json,command.created_at,outcome_json],
        )?;
        let applied = Self::append_with_projection_in_tx(&tx, event, projection)?;
        tx.execute(
            "INSERT INTO workflow_queue(run_id,node_id,available_at,priority) VALUES (?1,?2,?3,?4)",
            params![
                root_queue.run_id,
                root_queue.node_id,
                root_queue.available_at,
                root_queue.priority
            ],
        )?;
        tx.commit()?;
        Ok(WorkflowCommandApplyOutcome::Applied(applied))
    }

    pub fn apply_approval_decision(
        &self,
        snapshot: &rightkit_workflow::control_plane::reducer::NodeLifecycleState,
        node_command: &rightkit_workflow::control_plane::reducer::NodeCommand,
        command: &WorkflowCommandRow,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        decision: &WorkflowApprovalDecision<'_>,
        outbox: Option<&WorkflowOutboxRow>,
    ) -> Result<WorkflowCommandApplyOutcome> {
        if node_command.kind
            != rightkit_workflow::control_plane::reducer::CommandKind::ApprovalDecide
            || !matches!(decision.state, "approved" | "rejected")
            || node_command.approval_decision
                != Some(if decision.state == "approved" {
                    rightkit_workflow::control_plane::reducer::ApprovalDecisionKind::Approved
                } else {
                    rightkit_workflow::control_plane::reducer::ApprovalDecisionKind::Rejected
                })
        {
            return Err(StorageError::Serde(
                "invalid approval decision command".into(),
            ));
        }
        let reduced =
            rightkit_workflow::control_plane::reducer::reduce_node(snapshot, node_command)
                .map_err(|reason| {
                    StorageError::Serde(format!(
                        "workflow node reducer rejected approval: {reason}"
                    ))
                })?;
        let Some((projection_node, projection_phase)) = projection.node else {
            return Err(StorageError::Serde(
                "approval decision requires node projection".into(),
            ));
        };
        if node_command.command_id != command.command_id
            || node_command.node_id != projection_node
            || event.node_id != Some(projection_node)
            || command.run_id != projection.run_id
            || node_command.expected_revision.0 != command.expected_revision as u64
            || node_command.kind.as_str() != command.command_kind
            || projection_phase != node_phase_storage(reduced.phase)
        {
            return Err(StorageError::Serde(
                "approval decision binding mismatch".into(),
            ));
        }
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        if latest_attempt_generation_conn(&tx, &command.run_id, &node_command.node_id)?
            != Some(node_command.generation as i64)
        {
            return Err(StorageError::Serde(
                "approval decision stale attempt generation".into(),
            ));
        }
        let b = tx.query_row("SELECT version_id,inputs_hash,outputs_hash,artifact_refs_json,capabilities_json,effect_request_hash,expires_at_unix_ms,role,state FROM workflow_approvals WHERE id=?1", params![decision.approval_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?))).optional()?.ok_or_else(|| StorageError::NotFound(format!("workflow approval {}", decision.approval_id)))?;
        if b.8 != "requested"
            || b.6 < decision.now_unix_ms
            || b.7 != decision.role
            || b.0 != decision.version_id
            || b.1 != decision.inputs_hash
            || b.2 != decision.outputs_hash
            || b.3 != decision.artifact_refs_json
            || b.4 != decision.capabilities_json
            || b.5 != decision.effect_request_hash
        {
            return Err(StorageError::Serde(
                "approval binding, role, or expiry mismatch".into(),
            ));
        }
        let inserted = tx.execute("INSERT OR IGNORE INTO workflow_commands(run_id,command_id,expected_revision,command_kind,payload_json,created_at,outcome_json) VALUES (?1,?2,?3,?4,?5,?6,'{}')", params![command.run_id,command.command_id,command.expected_revision,command.command_kind,command.payload_json,command.created_at])?;
        if inserted == 0 {
            let prior = tx.query_row(
                "SELECT outcome_json FROM workflow_commands WHERE run_id=?1 AND command_id=?2",
                params![command.run_id, command.command_id],
                |r| r.get(0),
            )?;
            tx.commit()?;
            return Ok(WorkflowCommandApplyOutcome::Duplicate {
                outcome_json: prior,
            });
        }
        let applied = Self::append_with_projection_in_tx(&tx, event, projection)?;
        if tx.execute("UPDATE workflow_approvals SET state=?1,decision_id=?2,decision_actor=?3,decided_at_unix_ms=?4,updated_at=?5 WHERE id=?6 AND state='requested' AND decision_id IS NULL", params![decision.state,decision.decision_id,decision.actor,decision.now_unix_ms,command.created_at,decision.approval_id])? != 1 {
            return Err(StorageError::Serde("approval decision lost idempotency race".into()));
        }
        tx.execute(
            "UPDATE workflow_attention SET state='resolved',updated_at=?1 WHERE approval_id=?2 AND state IN ('open','read')",
            params![command.created_at, decision.approval_id],
        )?;
        if let Some(row) = outbox {
            tx.execute("INSERT INTO workflow_outbox(id,run_id,kind,topic,dedupe_key,payload_json,state,available_at,attempt_count,max_attempts,last_error_json,next_attempt_at,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![row.id,row.run_id,row.kind,row.topic,row.dedupe_key,row.payload_json,row.state,row.available_at,row.attempt_count,row.max_attempts,row.last_error_json,row.next_attempt_at,row.created_at,row.updated_at])?;
        }
        tx.commit()?;
        Ok(WorkflowCommandApplyOutcome::Applied(applied))
    }

    #[cfg(test)]
    pub(crate) fn apply_command_with_fault(
        &self,
        command: &WorkflowCommandRow,
        outcome_json: &str,
        event: &JournalAppend<'_>,
        projection: &ProjectionUpdate<'_>,
        queue: Option<&WorkflowQueueRow>,
        wait: Option<&WorkflowWaitRow>,
        approval: Option<&WorkflowApprovalRow>,
        effect: Option<&WorkflowEffectRow>,
        attention: Option<&WorkflowAttentionRow>,
        outbox: Option<&WorkflowOutboxRow>,
        fail_at: WorkflowCommandBoundary,
    ) -> Result<WorkflowCommandApplyOutcome> {
        self.apply_command_at_boundary(
            command,
            outcome_json,
            event,
            projection,
            queue,
            wait,
            approval,
            effect,
            attention,
            outbox,
            Some(fail_at),
        )
    }
}

fn fail_if(
    selected: Option<WorkflowCommandBoundary>,
    boundary: WorkflowCommandBoundary,
) -> Result<()> {
    if selected == Some(boundary) {
        return Err(StorageError::Serde(format!(
            "injected workflow transaction fault at {boundary:?}"
        )));
    }
    Ok(())
}

fn node_phase_storage(phase: rightkit_workflow::control_plane::reducer::NodePhase) -> &'static str {
    use rightkit_workflow::control_plane::reducer::NodePhase;
    match phase {
        NodePhase::Ready => "ready",
        NodePhase::Claimed => "claimed",
        NodePhase::Running => "running",
        NodePhase::Waiting => "waiting",
        NodePhase::Succeeded => "succeeded",
        NodePhase::Skipped => "skipped",
        NodePhase::Failed => "failed",
        NodePhase::Cancelled => "cancelled",
    }
}

pub(crate) fn latest_attempt_generation_conn(
    conn: &Connection,
    run_id: &str,
    node_id: &str,
) -> Result<Option<i64>> {
    let attempt = conn
        .query_row(
            "SELECT lease_fence FROM workflow_attempts WHERE run_id=?1 AND node_id=?2 ORDER BY attempt DESC LIMIT 1",
            params![run_id, node_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if attempt.is_some() {
        return Ok(attempt);
    }
    Ok(conn
        .query_row(
            "SELECT claim_fence FROM workflow_queue WHERE run_id=?1 AND node_id=?2 AND claim_fence IS NOT NULL",
            params![run_id, node_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?)
}

// --------------------------------------------------------------------------
// WP-06 / WP-07 trigger-event + occurrence writer (idempotent admission)
// --------------------------------------------------------------------------

/// Trigger-event + occurrence writer. Its admission operations are:
/// * [`WorkflowTriggerWriter::admit_hmac_envelope`] — unified authenticated
///   event/occurrence/run admission in one transaction.
/// * [`WorkflowTriggerWriter::insert_trigger_event_idempotent`] —
///   redelivery-safe INSERT for inbound events. Returns
///   [`TriggerEventInsert::Duplicate`] instead of an error when the
///   `(source, dedupe_key)` pair already exists.
/// * [`WorkflowTriggerWriter::admit_occurrence`] — idempotent occurrence
///   admission against `(trigger_id, dedupe_key)`. The caller supplies the
///   decision; the writer stores it once. A duplicate `(trigger_id,
///   dedupe_key)` is rejected with `StorageError::Constraint` so callers can
///   distinguish "raced with another admission" from "first write".
///
/// The compatibility primitives are single-statement INSERTs against unique
/// indexes. The unified coordinator owns its full transaction. Reader methods
/// live on [`WorkflowRepository`].
fn apply_run_projection(
    tx: &Transaction<'_>,
    existing: &WorkflowRunRow,
    projection: &ProjectionUpdate<'_>,
) -> Result<()> {
    let next_phase = projection.run_phase.unwrap_or(&existing.lifecycle_phase);
    let next_terminal_at = projection
        .run_terminal_at
        .map(|s| s.to_string())
        .or_else(|| existing.terminal_at.clone());
    let next_terminal_failure_class = projection
        .run_terminal_failure_class
        .map(|s| s.to_string())
        .or_else(|| existing.terminal_failure_class.clone());
    let next_revision = projection
        .run_lifecycle_revision
        .unwrap_or(existing.lifecycle_revision + 1);
    tx.execute(
        "UPDATE workflow_runs
            SET lifecycle_phase        = ?2,
                lifecycle_revision     = ?3,
                terminal_at            = ?4,
                terminal_failure_class = ?5
          WHERE id = ?1",
        params![
            existing.id,
            next_phase,
            next_revision,
            next_terminal_at,
            next_terminal_failure_class,
        ],
    )?;
    Ok(())
}

fn insert_run_on_first_event(
    tx: &Transaction<'_>,
    projection: &ProjectionUpdate<'_>,
) -> Result<()> {
    // The journal writer trusts that whoever is calling has *just* inserted
    // (or is about to insert) the run row outside this transaction. Here we
    // enforce that: a phase must be set, and the run identity must be
    // available. Identity comes from the caller via the journal event
    // implicitly through `projection.run_id`. The `started_at`/`workflow_id`
    // /`workflow_version_id`/`initiated_by_*` columns are not in
    // `ProjectionUpdate` (those are out-of-band fields set by the runner)
    // — so we look them up if a row exists, otherwise bail. In practice the
    // runner pre-creates the run row and the journal writer only updates it;
    // making that explicit avoids a hidden state-machine bug where a
    // journal event lands without a parent run row.
    let present: bool = tx
        .query_row(
            "SELECT 1 FROM workflow_runs WHERE id = ?1",
            params![projection.run_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !present {
        return Err(StorageError::Serde(format!(
            "workflow_journal: refusing to append for unknown run id {}; \
             the runner must insert the workflow_runs row first",
            projection.run_id
        )));
    }
    Ok(())
}

fn upsert_node_state(
    tx: &Transaction<'_>,
    run_id: &str,
    node_id: &str,
    phase: &str,
    last_event_seq: i64,
    last_revision: i64,
    projection: &ProjectionUpdate<'_>,
) -> Result<()> {
    let attempt = projection.node_attempt;
    let last_failure_class: Option<&str> = projection.node_last_failure_class;
    let (lease_holder, lease_expires, lease_cleared) = match &projection.node_lease {
        Some(l) if l.cleared => (None, None, true),
        Some(l) => (Some(l.holder), Some(l.expires_at), false),
        None => (None, None, false),
    };
    if lease_cleared {
        tx.execute(
            "UPDATE workflow_node_states
                SET phase = ?3,
                    last_reducer_revision = ?4,
                    last_event_seq = ?5,
                    lease_holder = NULL,
                    lease_expires_at = NULL,
                    last_failure_class = ?6,
                    last_updated_at = ?7,
                    attempt = COALESCE(?8, attempt)
              WHERE run_id = ?1 AND node_id = ?2",
            params![
                run_id,
                node_id,
                phase,
                last_revision,
                last_event_seq,
                last_failure_class,
                chrono_like_now_iso(),
                attempt,
            ],
        )?;
    } else {
        tx.execute(
            "INSERT INTO workflow_node_states(
                run_id, node_id, phase, attempt,
                lease_holder, lease_expires_at,
                last_reducer_revision, last_event_seq,
                last_failure_class, last_updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(run_id, node_id) DO UPDATE SET
                 phase = excluded.phase,
                 lease_holder = excluded.lease_holder,
                 lease_expires_at = excluded.lease_expires_at,
                 last_reducer_revision = excluded.last_reducer_revision,
                 last_event_seq = excluded.last_event_seq,
                 last_failure_class = excluded.last_failure_class,
                 last_updated_at = excluded.last_updated_at,
                 attempt = CASE WHEN ?11 IS NULL
                     THEN workflow_node_states.attempt
                     ELSE excluded.attempt
                 END",
            params![
                run_id,
                node_id,
                phase,
                attempt.unwrap_or(0),
                lease_holder,
                lease_expires,
                last_revision,
                last_event_seq,
                last_failure_class,
                chrono_like_now_iso(),
                attempt,
            ],
        )?;
    }
    Ok(())
}

fn upsert_run_lease(tx: &Transaction<'_>, lease: &WorkflowRunLeaseRow) -> Result<()> {
    tx.execute(
        "INSERT INTO workflow_run_lease(
            run_id, lease_holder, lease_expires_at, lease_fence, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(run_id) DO UPDATE SET
             lease_holder = excluded.lease_holder,
             lease_expires_at = excluded.lease_expires_at,
             lease_fence = MAX(workflow_run_lease.lease_fence, excluded.lease_fence),
             updated_at = excluded.updated_at",
        params![
            lease.run_id,
            lease.lease_holder,
            lease.lease_expires_at,
            lease.lease_fence,
            lease.updated_at,
        ],
    )?;
    Ok(())
}

fn chrono_like_now_iso() -> String {
    // Cheap ISO-8601 / RFC3339-ish UTC string built from `SystemTime`. We
    // avoid pulling chrono into storage just to format a single column;
    // resolution is one second, which is enough for `last_updated_at`
    // (the journal's `created_at` is the authoritative ordering signal).
    // If a caller has its own `now` strategy they can supply it via
    // `WorkflowRunLeaseRow::updated_at` instead of relying on this.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_unix_as_iso(secs)
}

/// UTC RFC 3339 text (`1970-01-01T00:00:00Z`) for `secs`; a negative
/// timestamp reads as the epoch. Same layout as the storage crate root's
/// `format_unix_as_iso`, kept here so the workflow module does not reach into
/// the private lib.rs surface.
fn format_unix_as_iso(secs: i64) -> String {
    if secs < 0 {
        return "1970-01-01T00:00:00Z".to_string();
    }
    rightkit_time::DateTime::from_unix(secs, 0)
        .map(|dt| dt.format_rfc3339())
        .unwrap_or_default()
}
