//! Read-only queries plus fenced CAS transitions for queues, attempts,
//! waits, effects, attention, & outbox rows. The connection here is
//! shared with the writer at the daemon layer.

use super::journal::latest_attempt_generation_conn;
use super::mappers::{map_effect_row, map_import_quarantine_row, map_outbox_row, map_wait_row};
use super::models::*;
use crate::error::{Result, StorageError};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

// --------------------------------------------------------------------------
// Repository — read-only
// --------------------------------------------------------------------------

/// Read-only access to the workflow tables. No transactions needed; the
/// `Connection` here is shared with the writer at the daemon layer.
pub struct WorkflowRepository<'a> {
    conn: &'a Connection,
}

impl<'a> WorkflowRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Persist an immutable learning proposal. Replays with same id are no-ops.
    pub fn insert_learning_proposal(
        &self,
        id: &str,
        workflow_id: &str,
        proposed_by: &str,
        proposal_json: &str,
        status: &str,
        created_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO learning_proposals
             (id, workflow_id, proposed_by, proposal_json, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                workflow_id,
                proposed_by,
                proposal_json,
                status,
                created_at
            ],
        )?;
        Ok(())
    }

    /// Persist one evaluation receipt. Replays with same id are no-ops.
    pub fn insert_learning_eval(
        &self,
        id: &str,
        proposal_id: &str,
        pass_rate_pct: f64,
        regression_count: i64,
        total_runs: i64,
        evidence_json: &str,
        created_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO learning_eval_runs
             (id, proposal_id, pass_rate_pct, regression_count, total_runs, evidence_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                proposal_id,
                pass_rate_pct,
                regression_count,
                total_runs,
                evidence_json,
                created_at
            ],
        )?;
        Ok(())
    }

    pub fn find_learning_proposal(&self, id: &str) -> Result<Option<(String, String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT workflow_id,status,proposal_json FROM learning_proposals WHERE id=?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    pub fn find_learning_eval(
        &self,
        id: &str,
        proposal_id: &str,
    ) -> Result<Option<(f64, i64, i64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT pass_rate_pct,regression_count,total_runs,evidence_json FROM learning_eval_runs WHERE id=?1 AND proposal_id=?2",
                params![id, proposal_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?)
    }

    pub fn find_definition(&self, id: &str) -> Result<Option<WorkflowDefinitionRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, source_kind, created_at, updated_at
                   FROM workflow_definitions WHERE id = ?1",
                params![id],
                |row| {
                    Ok(WorkflowDefinitionRow {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        source_kind: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn list_definitions(&self, limit: i64) -> Result<Vec<WorkflowDefinitionRow>> {
        let mut stmt = self.conn.prepare("SELECT id,name,source_kind,created_at,updated_at FROM workflow_definitions ORDER BY updated_at DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(WorkflowDefinitionRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_kind: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_versions(&self, workflow_id: &str) -> Result<Vec<WorkflowVersionRow>> {
        let mut stmt = self.conn.prepare("SELECT id,workflow_id,version_number,content_hash,definition_json,compiled_plan_json,publication_state,created_at,published_at,created_by,notes FROM workflow_versions WHERE workflow_id=?1 ORDER BY version_number DESC")?;
        let rows = stmt
            .query_map(params![workflow_id], |row| {
                Ok(WorkflowVersionRow {
                    id: row.get(0)?,
                    workflow_id: row.get(1)?,
                    version_number: row.get(2)?,
                    content_hash: row.get(3)?,
                    definition_json: row.get(4)?,
                    compiled_plan_json: row.get(5)?,
                    publication_state: row.get(6)?,
                    created_at: row.get(7)?,
                    published_at: row.get(8)?,
                    created_by: row.get(9)?,
                    notes: row.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_version(&self, id: &str) -> Result<Option<WorkflowVersionRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, workflow_id, version_number, content_hash,
                        definition_json, compiled_plan_json, publication_state,
                        created_at, published_at, created_by, notes
                   FROM workflow_versions WHERE id = ?1",
                params![id],
                |row| {
                    Ok(WorkflowVersionRow {
                        id: row.get(0)?,
                        workflow_id: row.get(1)?,
                        version_number: row.get(2)?,
                        content_hash: row.get(3)?,
                        definition_json: row.get(4)?,
                        compiled_plan_json: row.get(5)?,
                        publication_state: row.get(6)?,
                        created_at: row.get(7)?,
                        published_at: row.get(8)?,
                        created_by: row.get(9)?,
                        notes: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn find_run(&self, id: &str) -> Result<Option<WorkflowRunRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, workflow_id, workflow_version_id, parent_run_id,
                        lifecycle_phase, lifecycle_revision,
                        initiated_by_kind, initiated_by_ref,
                        trigger_payload_json, input_json,
                        started_at, terminal_at, terminal_failure_class,
                        runs_root_durable
                   FROM workflow_runs WHERE id = ?1",
                params![id],
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
            .optional()?)
    }

    pub fn list_runs_by_phase(&self, phase: &str, limit: i64) -> Result<Vec<WorkflowRunRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workflow_id, workflow_version_id, parent_run_id,
                    lifecycle_phase, lifecycle_revision,
                    initiated_by_kind, initiated_by_ref,
                    trigger_payload_json, input_json,
                    started_at, terminal_at, terminal_failure_class,
                    runs_root_durable
               FROM workflow_runs
              WHERE lifecycle_phase = ?1
              ORDER BY started_at ASC
              LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![phase, limit], |row| {
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
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_runs(&self, limit: i64) -> Result<Vec<WorkflowRunRow>> {
        let mut stmt = self.conn.prepare("SELECT id,workflow_id,workflow_version_id,parent_run_id,lifecycle_phase,lifecycle_revision,initiated_by_kind,initiated_by_ref,trigger_payload_json,input_json,started_at,terminal_at,terminal_failure_class,runs_root_durable FROM workflow_runs ORDER BY started_at DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], |row| {
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
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_journal(
        &self,
        run_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<WorkflowJournalRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, seq, revision, event_kind, node_id, payload_json, created_at
               FROM workflow_journal
              WHERE run_id = ?1 AND seq > ?2
              ORDER BY seq ASC
              LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![run_id, after_seq, limit], |row| {
                Ok(WorkflowJournalRow {
                    run_id: row.get(0)?,
                    seq: row.get(1)?,
                    revision: row.get(2)?,
                    event_kind: row.get(3)?,
                    node_id: row.get(4)?,
                    payload_json: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_node_state(
        &self,
        run_id: &str,
        node_id: &str,
    ) -> Result<Option<WorkflowNodeStateRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT run_id, node_id, phase, attempt, lease_holder,
                        lease_expires_at, last_reducer_revision, last_event_seq,
                        last_failure_class, last_updated_at
                   FROM workflow_node_states
                  WHERE run_id = ?1 AND node_id = ?2",
                params![run_id, node_id],
                |row| {
                    Ok(WorkflowNodeStateRow {
                        run_id: row.get(0)?,
                        node_id: row.get(1)?,
                        phase: row.get(2)?,
                        attempt: row.get(3)?,
                        lease_holder: row.get(4)?,
                        lease_expires_at: row.get(5)?,
                        last_reducer_revision: row.get(6)?,
                        last_event_seq: row.get(7)?,
                        last_failure_class: row.get(8)?,
                        last_updated_at: row.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn list_node_states(&self, run_id: &str) -> Result<Vec<WorkflowNodeStateRow>> {
        let mut stmt = self.conn.prepare("SELECT run_id,node_id,phase,attempt,lease_holder,lease_expires_at,last_reducer_revision,last_event_seq,last_failure_class,last_updated_at FROM workflow_node_states WHERE run_id=?1 ORDER BY node_id")?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                Ok(WorkflowNodeStateRow {
                    run_id: row.get(0)?,
                    node_id: row.get(1)?,
                    phase: row.get(2)?,
                    attempt: row.get(3)?,
                    lease_holder: row.get(4)?,
                    lease_expires_at: row.get(5)?,
                    last_reducer_revision: row.get(6)?,
                    last_event_seq: row.get(7)?,
                    last_failure_class: row.get(8)?,
                    last_updated_at: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_run_lease(&self, run_id: &str) -> Result<Option<WorkflowRunLeaseRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT run_id, lease_holder, lease_expires_at, lease_fence, updated_at
                   FROM workflow_run_lease
                  WHERE run_id = ?1",
                params![run_id],
                |row| {
                    Ok(WorkflowRunLeaseRow {
                        run_id: row.get(0)?,
                        lease_holder: row.get(1)?,
                        lease_expires_at: row.get(2)?,
                        lease_fence: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn find_queue_node(&self, run_id: &str, node_id: &str) -> Result<Option<WorkflowQueueRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT run_id,node_id,available_at,priority,claimed_by,claim_fence
               FROM workflow_queue WHERE run_id=?1 AND node_id=?2",
                params![run_id, node_id],
                |row| {
                    Ok(WorkflowQueueRow {
                        run_id: row.get(0)?,
                        node_id: row.get(1)?,
                        available_at: row.get(2)?,
                        priority: row.get(3)?,
                        claimed_by: row.get(4)?,
                        claim_fence: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn find_attempt(&self, id: &str) -> Result<Option<WorkflowAttemptRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id,run_id,node_id,attempt,lease_fence,state,started_at,finished_at
               FROM workflow_attempts WHERE id=?1",
                params![id],
                |row| {
                    Ok(WorkflowAttemptRow {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        node_id: row.get(2)?,
                        attempt: row.get(3)?,
                        lease_fence: row.get(4)?,
                        state: row.get(5)?,
                        started_at: row.get(6)?,
                        finished_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn latest_attempt_generation(&self, run_id: &str, node_id: &str) -> Result<Option<i64>> {
        latest_attempt_generation_conn(self.conn, run_id, node_id)
    }

    pub fn list_attempts_by_node_order(&self, run_id: &str) -> Result<Vec<WorkflowAttemptRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,attempt,lease_fence,state,started_at,finished_at FROM workflow_attempts WHERE run_id=?1 ORDER BY node_id,attempt")?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                Ok(WorkflowAttemptRow {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    node_id: row.get(2)?,
                    attempt: row.get(3)?,
                    lease_fence: row.get(4)?,
                    state: row.get(5)?,
                    started_at: row.get(6)?,
                    finished_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_effects_by_created_order(&self, run_id: &str) -> Result<Vec<WorkflowEffectRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,adapter,operation,destination_hash,payload_hash,idempotency_key,state,attempt_count,max_attempts,receipt_json,outcome_json,reconciliation_json,failure_json,requested_at_unix_ms,ttl_unix_ms,created_at,updated_at FROM workflow_effects WHERE run_id=?1 ORDER BY created_at,id")?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok::<WorkflowEffectRow, rusqlite::Error>(WorkflowEffectRow {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    node_id: r.get(2)?,
                    adapter: r.get(3)?,
                    operation: r.get(4)?,
                    destination_hash: r.get(5)?,
                    payload_hash: r.get(6)?,
                    idempotency_key: r.get(7)?,
                    state: r.get(8)?,
                    attempt_count: r.get(9)?,
                    max_attempts: r.get(10)?,
                    receipt_json: r.get(11)?,
                    outcome_json: r.get(12)?,
                    reconciliation_json: r.get(13)?,
                    failure_json: r.get(14)?,
                    requested_at_unix_ms: r.get(15)?,
                    ttl_unix_ms: r.get(16)?,
                    created_at: r.get(17)?,
                    updated_at: r.get(18)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn find_effect(&self, id: &str) -> Result<Option<WorkflowEffectRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,adapter,operation,destination_hash,payload_hash,idempotency_key,state,attempt_count,max_attempts,receipt_json,outcome_json,reconciliation_json,failure_json,requested_at_unix_ms,ttl_unix_ms,created_at,updated_at FROM workflow_effects WHERE id=?1")?;
        let mut rows = stmt.query(params![id])?;
        Ok(rows
            .next()?
            .map(|r| {
                Ok::<WorkflowEffectRow, rusqlite::Error>(WorkflowEffectRow {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    node_id: r.get(2)?,
                    adapter: r.get(3)?,
                    operation: r.get(4)?,
                    destination_hash: r.get(5)?,
                    payload_hash: r.get(6)?,
                    idempotency_key: r.get(7)?,
                    state: r.get(8)?,
                    attempt_count: r.get(9)?,
                    max_attempts: r.get(10)?,
                    receipt_json: r.get(11)?,
                    outcome_json: r.get(12)?,
                    reconciliation_json: r.get(13)?,
                    failure_json: r.get(14)?,
                    requested_at_unix_ms: r.get(15)?,
                    ttl_unix_ms: r.get(16)?,
                    created_at: r.get(17)?,
                    updated_at: r.get(18)?,
                })
            })
            .transpose()?)
    }

    pub fn list_attempts(&self, run_id: &str) -> Result<Vec<WorkflowAttemptRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,attempt,lease_fence,state,started_at,finished_at FROM workflow_attempts WHERE run_id=?1 ORDER BY started_at DESC")?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                Ok(WorkflowAttemptRow {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    node_id: row.get(2)?,
                    attempt: row.get(3)?,
                    lease_fence: row.get(4)?,
                    state: row.get(5)?,
                    started_at: row.get(6)?,
                    finished_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_due_waits(&self, now_unix_ms: i64, limit: i64) -> Result<Vec<WorkflowWaitRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,kind,state,correlation_key,topic,filter_expression,filter_hash,wake_at_unix_ms,deadline_unix_ms,matched_count,max_matches,created_at,updated_at FROM workflow_waits WHERE state='open' AND ((wake_at_unix_ms IS NOT NULL AND wake_at_unix_ms<=?1) OR (deadline_unix_ms IS NOT NULL AND deadline_unix_ms<=?1)) ORDER BY COALESCE(wake_at_unix_ms,deadline_unix_ms),created_at LIMIT ?2")?;
        let rows = stmt
            .query_map(params![now_unix_ms, limit], map_wait_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_open_event_waits(
        &self,
        topic: &str,
        correlation_key: &str,
        limit: i64,
    ) -> Result<Vec<WorkflowWaitRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,kind,state,correlation_key,topic,filter_expression,filter_hash,wake_at_unix_ms,deadline_unix_ms,matched_count,max_matches,created_at,updated_at FROM workflow_waits WHERE state='open' AND kind='event' AND topic=?1 AND correlation_key=?2 ORDER BY created_at LIMIT ?3")?;
        let rows = stmt
            .query_map(params![topic, correlation_key, limit], map_wait_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn matching_event_waits(
        &self,
        topic: &str,
        correlation_key: &str,
        event: &Value,
        limit: i64,
    ) -> Result<Vec<WorkflowWaitRow>> {
        Ok(self
            .list_open_event_waits(topic, correlation_key, limit)?
            .into_iter()
            .filter(|wait| {
                wait.filter_expression.as_deref().is_some_and(|expression| {
                    wait.filter_hash.as_deref().is_some_and(|hash| {
                        rightkit_workflow::control_plane::waits::matches_event_filter(
                            expression, hash, event,
                        )
                    })
                })
            })
            .collect())
    }

    pub fn resolve_wait(
        &self,
        id: &str,
        expected_state: &str,
        resolved_state: &str,
        updated_at: &str,
    ) -> Result<bool> {
        if !matches!(resolved_state, "matched" | "timed_out" | "cancelled") {
            return Err(StorageError::Serde("invalid resolved wait state".into()));
        }
        Ok(self.conn.execute(
            "UPDATE workflow_waits SET state=?1,matched_count=matched_count+CASE WHEN ?1='matched' THEN 1 ELSE 0 END,updated_at=?2 WHERE id=?3 AND state=?4",
            params![resolved_state, updated_at, id, expected_state],
        )? == 1)
    }

    pub fn list_effects(&self, run_id: &str) -> Result<Vec<WorkflowEffectRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,adapter,operation,destination_hash,payload_hash,idempotency_key,state,attempt_count,max_attempts,receipt_json,outcome_json,reconciliation_json,failure_json,requested_at_unix_ms,ttl_unix_ms,created_at,updated_at FROM workflow_effects WHERE run_id=?1 ORDER BY created_at DESC")?;
        let rows = stmt
            .query_map(params![run_id], map_effect_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_pending_effects(&self, limit: i64) -> Result<Vec<WorkflowEffectRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,adapter,operation,destination_hash,payload_hash,idempotency_key,state,attempt_count,max_attempts,receipt_json,outcome_json,reconciliation_json,failure_json,requested_at_unix_ms,ttl_unix_ms,created_at,updated_at FROM workflow_effects WHERE state IN ('planned','outcome_unknown','reconciling') AND attempt_count<max_attempts ORDER BY requested_at_unix_ms,id LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], map_effect_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_recoverable_effects(
        &self,
        now_unix_ms: i64,
        limit: i64,
    ) -> Result<Vec<WorkflowEffectRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,adapter,operation,destination_hash,payload_hash,idempotency_key,state,attempt_count,max_attempts,receipt_json,outcome_json,reconciliation_json,failure_json,requested_at_unix_ms,ttl_unix_ms,created_at,updated_at FROM workflow_effects WHERE attempt_count<max_attempts AND (state IN ('planned','outcome_unknown','reconciling') OR (state='dispatching' AND claim_expires_at_unix_ms<=?1)) ORDER BY CASE state WHEN 'dispatching' THEN 0 WHEN 'outcome_unknown' THEN 1 WHEN 'reconciling' THEN 2 ELSE 3 END,requested_at_unix_ms,id LIMIT ?2")?;
        let rows = stmt
            .query_map(params![now_unix_ms, limit], map_effect_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn claim_effect(&self, claim: &WorkflowEffectClaim<'_>) -> Result<bool> {
        if claim.fence <= 0 || claim.holder.is_empty() {
            return Err(StorageError::Serde("invalid effect claim fence".into()));
        }
        if !matches!(
            claim.expected_state,
            "planned" | "outcome_unknown" | "reconciling" | "dispatching"
        ) {
            return Err(StorageError::Serde("invalid effect claim state".into()));
        }
        let claimed_state = if claim.expected_state == "planned" {
            "dispatching"
        } else {
            "reconciling"
        };
        let changed = self.conn.execute(
            "UPDATE workflow_effects SET state=?1,attempt_count=attempt_count+1,claim_holder=?2,claim_fence=?3,claim_expires_at_unix_ms=?4,updated_at=?5 WHERE id=?6 AND state=?7 AND idempotency_key=?8 AND claim_fence<?3 AND attempt_count<max_attempts AND (?7<>'dispatching' OR claim_expires_at_unix_ms<=?9)",
            params![claimed_state,claim.holder,claim.fence,claim.expires_at_unix_ms,claim.updated_at,claim.id,claim.expected_state,claim.idempotency_key,claim.now_unix_ms],
        )?;
        if changed == 1 {
            return Ok(true);
        }
        Ok(self.conn.query_row(
            "SELECT count(*) FROM workflow_effects WHERE id=?1 AND state=?2 AND idempotency_key=?3 AND claim_holder=?4 AND claim_fence=?5",
            params![claim.id,claimed_state,claim.idempotency_key,claim.holder,claim.fence],
            |row| row.get::<_,i64>(0),
        )? == 1)
    }

    pub fn record_effect_outcome(&self, outcome: &WorkflowEffectOutcome<'_>) -> Result<bool> {
        if !matches!(
            outcome.state,
            "confirmed" | "failed" | "outcome_unknown" | "reconciling" | "cancelled"
        ) {
            return Err(StorageError::Serde("invalid effect outcome state".into()));
        }
        Ok(self.conn.execute(
            "UPDATE workflow_effects SET state=?1,receipt_json=?2,outcome_json=?3,reconciliation_json=?4,failure_json=?5,updated_at=?6,claim_expires_at_unix_ms=NULL WHERE id=?7 AND idempotency_key=?8 AND claim_holder=?9 AND claim_fence=?10 AND state IN ('dispatching','reconciling','outcome_unknown')",
            params![outcome.state,outcome.receipt_json,outcome.outcome_json,outcome.reconciliation_json,outcome.failure_json,outcome.updated_at,outcome.id,outcome.idempotency_key,outcome.holder,outcome.fence],
        )? == 1)
    }

    pub fn resolve_attention_by_node(
        &self,
        run_id: &str,
        node_id: &str,
        updated_at: &str,
    ) -> Result<usize> {
        Ok(self.conn.execute("UPDATE workflow_attention SET state='resolved',updated_at=?1 WHERE run_id=?2 AND node_id=?3 AND state IN ('open','read')", params![updated_at,run_id,node_id])?)
    }

    pub fn resolve_attention_by_effect(&self, effect_id: &str, updated_at: &str) -> Result<usize> {
        Ok(self.conn.execute("UPDATE workflow_attention SET state='resolved',updated_at=?1 WHERE effect_id=?2 AND state IN ('open','read')", params![updated_at,effect_id])?)
    }

    pub fn resolve_attention_by_approval(
        &self,
        approval_id: &str,
        updated_at: &str,
    ) -> Result<usize> {
        Ok(self.conn.execute("UPDATE workflow_attention SET state='resolved',updated_at=?1 WHERE approval_id=?2 AND state IN ('open','read')", params![updated_at,approval_id])?)
    }

    pub fn list_triggers(&self, workflow_id: &str) -> Result<Vec<WorkflowTriggerRow>> {
        let mut stmt = self.conn.prepare("SELECT id,workflow_id,kind,enabled,cron_expression,timezone,version_pin,dedupe_key,overlap_policy,misfire_policy,concurrency_cap,next_fire_at_unix_ms,revision,created_at,updated_at FROM workflow_triggers WHERE workflow_id=?1 ORDER BY created_at DESC")?;
        let rows = stmt
            .query_map(params![workflow_id], |r| {
                Ok(WorkflowTriggerRow {
                    id: r.get(0)?,
                    workflow_id: r.get(1)?,
                    kind: r.get(2)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                    cron_expression: r.get(4)?,
                    timezone: r.get(5)?,
                    version_pin: r.get(6)?,
                    dedupe_key: r.get(7)?,
                    overlap_policy: r.get(8)?,
                    misfire_policy: r.get(9)?,
                    concurrency_cap: r.get(10)?,
                    next_fire_at_unix_ms: r.get(11)?,
                    revision: r.get(12)?,
                    created_at: r.get(13)?,
                    updated_at: r.get(14)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_due_cron_triggers(
        &self,
        now_unix_ms: i64,
        limit: i64,
    ) -> Result<Vec<WorkflowTriggerRow>> {
        let mut stmt = self.conn.prepare("SELECT id,workflow_id,kind,enabled,cron_expression,timezone,version_pin,dedupe_key,overlap_policy,misfire_policy,concurrency_cap,next_fire_at_unix_ms,revision,created_at,updated_at FROM workflow_triggers WHERE enabled=1 AND kind='cron' AND next_fire_at_unix_ms<=?1 ORDER BY next_fire_at_unix_ms,id LIMIT ?2")?;
        let rows = stmt
            .query_map(params![now_unix_ms, limit], |r| {
                Ok(WorkflowTriggerRow {
                    id: r.get(0)?,
                    workflow_id: r.get(1)?,
                    kind: r.get(2)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                    cron_expression: r.get(4)?,
                    timezone: r.get(5)?,
                    version_pin: r.get(6)?,
                    dedupe_key: r.get(7)?,
                    overlap_policy: r.get(8)?,
                    misfire_policy: r.get(9)?,
                    concurrency_cap: r.get(10)?,
                    next_fire_at_unix_ms: r.get(11)?,
                    revision: r.get(12)?,
                    created_at: r.get(13)?,
                    updated_at: r.get(14)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Enabled daemon-internal triggers. The monitor producer uses this exact
    /// set after a completed workflow run; trigger admission still owns all
    /// version, overlap, and dedupe decisions.
    pub fn list_enabled_internal_event_triggers(&self) -> Result<Vec<WorkflowTriggerRow>> {
        let mut stmt = self.conn.prepare("SELECT id,workflow_id,kind,enabled,cron_expression,timezone,version_pin,dedupe_key,overlap_policy,misfire_policy,concurrency_cap,next_fire_at_unix_ms,revision,created_at,updated_at FROM workflow_triggers WHERE enabled=1 AND kind IN ('internal_event','monitor') ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WorkflowTriggerRow {
                    id: r.get(0)?,
                    workflow_id: r.get(1)?,
                    kind: r.get(2)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                    cron_expression: r.get(4)?,
                    timezone: r.get(5)?,
                    version_pin: r.get(6)?,
                    dedupe_key: r.get(7)?,
                    overlap_policy: r.get(8)?,
                    misfire_policy: r.get(9)?,
                    concurrency_cap: r.get(10)?,
                    next_fire_at_unix_ms: r.get(11)?,
                    revision: r.get(12)?,
                    created_at: r.get(13)?,
                    updated_at: r.get(14)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_trigger(&self, id: &str) -> Result<Option<WorkflowTriggerRow>> {
        Ok(self.conn.query_row("SELECT id,workflow_id,kind,enabled,cron_expression,timezone,version_pin,dedupe_key,overlap_policy,misfire_policy,concurrency_cap,next_fire_at_unix_ms,revision,created_at,updated_at FROM workflow_triggers WHERE id=?1", params![id], |r| Ok(WorkflowTriggerRow { id:r.get(0)?,workflow_id:r.get(1)?,kind:r.get(2)?,enabled:r.get::<_, i64>(3)? != 0,cron_expression:r.get(4)?,timezone:r.get(5)?,version_pin:r.get(6)?,dedupe_key:r.get(7)?,overlap_policy:r.get(8)?,misfire_policy:r.get(9)?,concurrency_cap:r.get(10)?,next_fire_at_unix_ms:r.get(11)?,revision:r.get(12)?,created_at:r.get(13)?,updated_at:r.get(14)? })).optional()?)
    }

    /// Compatibility lookup for one legacy scheduler mapping. The mapping key
    /// is unique at the daemon boundary; duplicate rows are rejected rather
    /// than selecting an arbitrary execution authority.
    pub fn find_trigger_by_dedupe_key(&self, key: &str) -> Result<Option<WorkflowTriggerRow>> {
        let rows: Vec<_> = self.conn.prepare("SELECT id,workflow_id,kind,enabled,cron_expression,timezone,version_pin,dedupe_key,overlap_policy,misfire_policy,concurrency_cap,next_fire_at_unix_ms,revision,created_at,updated_at FROM workflow_triggers WHERE dedupe_key=?1 LIMIT 2")?.query_map(params![key], |r| Ok(WorkflowTriggerRow { id:r.get(0)?,workflow_id:r.get(1)?,kind:r.get(2)?,enabled:r.get::<_, i64>(3)? != 0,cron_expression:r.get(4)?,timezone:r.get(5)?,version_pin:r.get(6)?,dedupe_key:r.get(7)?,overlap_policy:r.get(8)?,misfire_policy:r.get(9)?,concurrency_cap:r.get(10)?,next_fire_at_unix_ms:r.get(11)?,revision:r.get(12)?,created_at:r.get(13)?,updated_at:r.get(14)? }))?.collect::<rusqlite::Result<_>>()?;
        if rows.len() > 1 {
            return Err(StorageError::Serde(
                "ambiguous legacy schedule trigger mapping".into(),
            ));
        }
        Ok(rows.into_iter().next())
    }

    pub fn list_occurrences(&self, trigger_id: &str) -> Result<Vec<WorkflowOccurrenceRow>> {
        let mut stmt = self.conn.prepare("SELECT id,trigger_id,trigger_event_id,nominal_at_unix_ms,decision,decision_reason,selected_version_id,concurrency_key,dedupe_key,created_at FROM workflow_occurrences WHERE trigger_id=?1 ORDER BY created_at DESC")?;
        let rows = stmt
            .query_map(params![trigger_id], |r| {
                Ok(WorkflowOccurrenceRow {
                    id: r.get(0)?,
                    trigger_id: r.get(1)?,
                    trigger_event_id: r.get(2)?,
                    nominal_at_unix_ms: r.get(3)?,
                    decision: r.get(4)?,
                    decision_reason: r.get(5)?,
                    selected_version_id: r.get(6)?,
                    concurrency_key: r.get(7)?,
                    dedupe_key: r.get(8)?,
                    created_at: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn insert_trigger(&self, row: &WorkflowTriggerRow) -> Result<()> {
        self.conn.execute("INSERT INTO workflow_triggers(id,workflow_id,kind,enabled,cron_expression,timezone,version_pin,dedupe_key,overlap_policy,misfire_policy,concurrency_cap,next_fire_at_unix_ms,revision,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)", params![row.id,row.workflow_id,row.kind,row.enabled,row.cron_expression,row.timezone,row.version_pin,row.dedupe_key,row.overlap_policy,row.misfire_policy,row.concurrency_cap,row.next_fire_at_unix_ms,row.revision,row.created_at,row.updated_at])?;
        Ok(())
    }
    pub fn update_legacy_trigger(&self, row: &WorkflowTriggerRow) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_triggers SET workflow_id=?1,kind=?2,enabled=?3,cron_expression=?4,timezone=?5,version_pin=?6,overlap_policy=?7,misfire_policy=?8,concurrency_cap=?9,next_fire_at_unix_ms=?10,revision=revision+1,updated_at=?11 WHERE id=?12 AND dedupe_key=?13",
            params![row.workflow_id,row.kind,row.enabled,row.cron_expression,row.timezone,row.version_pin,row.overlap_policy,row.misfire_policy,row.concurrency_cap,row.next_fire_at_unix_ms,row.updated_at,row.id,row.dedupe_key],
        )?;
        Ok(())
    }
    pub fn set_trigger_enabled(&self, id: &str, enabled: bool, now: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE workflow_triggers SET enabled=?1,revision=revision+1,updated_at=?2 WHERE id=?3",
            params![enabled, now, id],
        )? == 1)
    }
    pub fn delete_trigger(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM workflow_triggers WHERE id=?1", params![id])?
            == 1)
    }
    pub fn insert_occurrence(&self, row: &WorkflowOccurrenceRow) -> Result<()> {
        self.conn.execute("INSERT INTO workflow_occurrences(id,trigger_id,trigger_event_id,nominal_at_unix_ms,decision,decision_reason,selected_version_id,concurrency_key,dedupe_key,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![row.id,row.trigger_id,row.trigger_event_id,row.nominal_at_unix_ms,row.decision,row.decision_reason,row.selected_version_id,row.concurrency_key,row.dedupe_key,row.created_at])?;
        Ok(())
    }

    pub fn list_evidence(&self, run_id: &str) -> Result<Vec<WorkflowEvidenceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id,c.run_id,c.node_id,e.evidence_type,e.evidence_hash,e.artifact_ref,e.created_at
               FROM workflow_evidence e JOIN workflow_claims c ON c.id=e.claim_id
              WHERE c.run_id=?1 ORDER BY e.created_at,e.id",
        )?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                Ok(WorkflowEvidenceRow {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    node_id: row.get(2)?,
                    kind: row.get(3)?,
                    content_hash: row.get(4)?,
                    artifact_ref: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_open_attention(&self, limit: i64) -> Result<Vec<WorkflowAttentionRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,node_id,effect_id,approval_id,kind,state,severity,title,summary,deep_link_json,dedupe_key,created_at,updated_at FROM workflow_attention WHERE state IN ('open','read') ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(WorkflowAttentionRow {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    node_id: row.get(2)?,
                    effect_id: row.get(3)?,
                    approval_id: row.get(4)?,
                    kind: row.get(5)?,
                    state: row.get(6)?,
                    severity: row.get(7)?,
                    title: row.get(8)?,
                    summary: row.get(9)?,
                    deep_link_json: row.get(10)?,
                    dedupe_key: row.get(11)?,
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_due_outbox(&self, now: &str, limit: i64) -> Result<Vec<WorkflowOutboxRow>> {
        let mut stmt = self.conn.prepare("SELECT id,run_id,kind,topic,dedupe_key,payload_json,state,available_at,attempt_count,max_attempts,last_error_json,next_attempt_at,created_at,updated_at FROM workflow_outbox WHERE (state='pending' OR (state='failed' AND next_attempt_at IS NOT NULL) OR (state='sending' AND claim_expires_at<=?1)) AND available_at<=?1 AND (next_attempt_at IS NULL OR next_attempt_at<=?1) AND attempt_count<max_attempts ORDER BY available_at,id LIMIT ?2")?;
        let rows = stmt
            .query_map(params![now, limit], map_outbox_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn claim_outbox(
        &self,
        id: &str,
        holder: &str,
        fence: i64,
        now: &str,
        expires_at: &str,
    ) -> Result<bool> {
        if holder.is_empty() || fence <= 0 {
            return Err(StorageError::Serde("invalid outbox claim".into()));
        }
        Ok(self.conn.execute("UPDATE workflow_outbox SET state='sending',claim_holder=?1,claim_fence=?2,claim_expires_at=?3,updated_at=?4 WHERE id=?5 AND claim_fence<?2 AND (state IN ('pending','failed') OR (state='sending' AND claim_expires_at<=?4)) AND attempt_count<max_attempts", params![holder,fence,expires_at,now,id])? == 1)
    }

    pub fn ack_outbox(&self, id: &str, holder: &str, fence: i64, now: &str) -> Result<bool> {
        Ok(self.conn.execute("UPDATE workflow_outbox SET state='sent',claim_expires_at=NULL,updated_at=?1 WHERE id=?2 AND state='sending' AND claim_holder=?3 AND claim_fence=?4", params![now,id,holder,fence])? == 1)
    }

    pub fn fail_outbox(
        &self,
        id: &str,
        holder: &str,
        fence: i64,
        error_json: &str,
        retry_at: Option<&str>,
        now: &str,
    ) -> Result<bool> {
        let Some(retry_at) = retry_at else {
            return Err(StorageError::Serde(
                "terminal outbox failure requires dead-letter attention".into(),
            ));
        };
        Ok(self.conn.execute("UPDATE workflow_outbox SET state='pending',attempt_count=attempt_count+1,last_error_json=?1,next_attempt_at=?2,claim_expires_at=NULL,updated_at=?3 WHERE id=?4 AND state='sending' AND claim_holder=?5 AND claim_fence=?6 AND attempt_count+1<max_attempts", params![error_json,retry_at,now,id,holder,fence])? == 1)
    }

    pub fn dead_letter_outbox(
        &self,
        id: &str,
        holder: &str,
        fence: i64,
        error_json: &str,
        now: &str,
        attention: &WorkflowAttentionRow,
    ) -> Result<bool> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let run_id = tx
            .query_row(
                "SELECT run_id FROM workflow_outbox WHERE id=?1 AND state='sending' AND claim_holder=?2 AND claim_fence=?3",
                params![id, holder, fence],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(run_id) = run_id else {
            return Ok(false);
        };
        if attention.run_id.as_deref() != Some(run_id.as_str())
            || attention.kind != "dead_letter"
            || attention.state != "open"
            || attention.dedupe_key != format!("outbox:{id}")
        {
            return Err(StorageError::Serde(
                "outbox dead-letter attention binding mismatch".into(),
            ));
        }
        tx.execute("UPDATE workflow_outbox SET state='failed',attempt_count=max_attempts,last_error_json=?1,next_attempt_at=NULL,claim_expires_at=NULL,updated_at=?2 WHERE id=?3 AND state='sending' AND claim_holder=?4 AND claim_fence=?5", params![error_json,now,id,holder,fence])?;
        tx.execute("INSERT INTO workflow_attention(id,run_id,node_id,effect_id,approval_id,kind,state,severity,title,summary,deep_link_json,dedupe_key,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(dedupe_key) DO UPDATE SET state='open',severity=excluded.severity,summary=excluded.summary,updated_at=excluded.updated_at", params![attention.id,attention.run_id,attention.node_id,attention.effect_id,attention.approval_id,attention.kind,attention.state,attention.severity,attention.title,attention.summary,attention.deep_link_json,attention.dedupe_key,attention.created_at,attention.updated_at])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn find_approval(&self, id: &str) -> Result<Option<WorkflowApprovalRow>> {
        Ok(self.conn.query_row("SELECT id,run_id,node_id,version_id,inputs_hash,outputs_hash,artifact_refs_json,capabilities_json,effect_request_hash,expires_at_unix_ms,role,one_shot,state,decision_id,decision_actor,decision_reason,decision_signature,decided_at_unix_ms,created_at,updated_at FROM workflow_approvals WHERE id=?1", params![id], |row| Ok(WorkflowApprovalRow {
            id: row.get(0)?, run_id: row.get(1)?, node_id: row.get(2)?, version_id: row.get(3)?, inputs_hash: row.get(4)?, outputs_hash: row.get(5)?, artifact_refs_json: row.get(6)?, capabilities_json: row.get(7)?, effect_request_hash: row.get(8)?, expires_at_unix_ms: row.get(9)?, role: row.get(10)?, one_shot: row.get::<_, i64>(11)? != 0, state: row.get(12)?, decision_id: row.get(13)?, decision_actor: row.get(14)?, decision_reason: row.get(15)?, decision_signature: row.get(16)?, decided_at_unix_ms: row.get(17)?, created_at: row.get(18)?, updated_at: row.get(19)?,
        })).optional()?)
    }

    pub fn find_claim(&self, id: &str) -> Result<Option<WorkflowClaimRow>> {
        Ok(self.conn.query_row(
            "SELECT id,run_id,node_id,claim_key,statement,state,statement_hash,created_at,updated_at
               FROM workflow_claims WHERE id=?1", params![id], |row| Ok(WorkflowClaimRow {
                id: row.get(0)?, run_id: row.get(1)?, node_id: row.get(2)?, claim_key: row.get(3)?,
                statement: row.get(4)?, state: row.get(5)?, statement_hash: row.get(6)?,
                created_at: row.get(7)?, updated_at: row.get(8)?,
            }),
        ).optional()?)
    }

    pub fn list_claim_evidence(&self, claim_id: &str) -> Result<Vec<WorkflowClaimEvidenceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,claim_id,artifact_ref,evidence_type,source_json,verdict,evidence_hash,
                    reviewer_attempt_id,created_at
               FROM workflow_evidence WHERE claim_id=?1 ORDER BY created_at,id",
        )?;
        let rows = stmt
            .query_map(params![claim_id], |row| {
                Ok(WorkflowClaimEvidenceRow {
                    id: row.get(0)?,
                    claim_id: row.get(1)?,
                    artifact_ref: row.get(2)?,
                    evidence_type: row.get(3)?,
                    source_json: row.get(4)?,
                    verdict: row.get(5)?,
                    evidence_hash: row.get(6)?,
                    reviewer_attempt_id: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Replay every journal event for `run_id` in order. Used by the
    /// projection rebuilder in WP-02-T03 and by supervisor restart in WP-03.
    pub fn replay_journal(&self, run_id: &str) -> Result<Vec<WorkflowJournalRow>> {
        self.list_journal(run_id, 0, i64::MAX)
    }

    // ---- WP-06 / WP-07 focused finders ----

    /// Look up a trigger event by its `(source, dedupe_key)` pair. Returns
    /// `None` for an unknown pair.
    pub fn find_trigger_event_by_dedupe(
        &self,
        source: &str,
        dedupe_key: &str,
    ) -> Result<Option<WorkflowTriggerEventRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, trigger_id, source, event_type, external_id, dedupe_key,
                        content_hash, trust_json, data_json, raw_artifact_id,
                        occurred_at, received_at
                   FROM workflow_trigger_events
                  WHERE source = ?1 AND dedupe_key = ?2",
                params![source, dedupe_key],
                |row| {
                    Ok(WorkflowTriggerEventRow {
                        id: row.get(0)?,
                        trigger_id: row.get(1)?,
                        source: row.get(2)?,
                        event_type: row.get(3)?,
                        external_id: row.get(4)?,
                        dedupe_key: row.get(5)?,
                        content_hash: row.get(6)?,
                        trust_json: row.get(7)?,
                        data_json: row.get(8)?,
                        raw_artifact_id: row.get(9)?,
                        occurred_at: row.get(10)?,
                        received_at: row.get(11)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn find_trigger_raw_artifact(
        &self,
        id: &str,
    ) -> Result<Option<WorkflowTriggerRawArtifactRow>> {
        Ok(self.conn.query_row(
            "SELECT id,content_hash,source,provenance_json,trust_json,storage_uri,metadata_json,byte_length,created_at FROM workflow_trigger_raw_artifacts WHERE id=?1",
            params![id],
            |row| Ok(WorkflowTriggerRawArtifactRow {
                id: row.get(0)?,
                content_hash: row.get(1)?,
                source: row.get(2)?,
                provenance_json: row.get(3)?,
                trust_json: row.get(4)?,
                storage_uri: row.get(5)?,
                metadata_json: row.get(6)?,
                byte_length: row.get(7)?,
                created_at: row.get(8)?,
            }),
        ).optional()?)
    }

    /// Look up a single occurrence by id.
    pub fn find_occurrence(&self, id: &str) -> Result<Option<WorkflowOccurrenceRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, trigger_id, trigger_event_id, nominal_at_unix_ms,
                        decision, decision_reason, selected_version_id,
                        concurrency_key, dedupe_key, created_at
                   FROM workflow_occurrences WHERE id = ?1",
                params![id],
                |row| {
                    Ok(WorkflowOccurrenceRow {
                        id: row.get(0)?,
                        trigger_id: row.get(1)?,
                        trigger_event_id: row.get(2)?,
                        nominal_at_unix_ms: row.get(3)?,
                        decision: row.get(4)?,
                        decision_reason: row.get(5)?,
                        selected_version_id: row.get(6)?,
                        concurrency_key: row.get(7)?,
                        dedupe_key: row.get(8)?,
                        created_at: row.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    /// Look up a single occurrence by `(trigger_id, dedupe_key)`.
    pub fn find_occurrence_by_dedupe(
        &self,
        trigger_id: &str,
        dedupe_key: &str,
    ) -> Result<Option<WorkflowOccurrenceRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, trigger_id, trigger_event_id, nominal_at_unix_ms,
                        decision, decision_reason, selected_version_id,
                        concurrency_key, dedupe_key, created_at
                   FROM workflow_occurrences
                  WHERE trigger_id = ?1 AND dedupe_key = ?2",
                params![trigger_id, dedupe_key],
                |row| {
                    Ok(WorkflowOccurrenceRow {
                        id: row.get(0)?,
                        trigger_id: row.get(1)?,
                        trigger_event_id: row.get(2)?,
                        nominal_at_unix_ms: row.get(3)?,
                        decision: row.get(4)?,
                        decision_reason: row.get(5)?,
                        selected_version_id: row.get(6)?,
                        concurrency_key: row.get(7)?,
                        dedupe_key: row.get(8)?,
                        created_at: row.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    /// Check journal/projection invariants without mutating storage.
    pub fn diagnose_integrity(&self, run_id: &str) -> Result<Vec<WorkflowIntegrityDiagnostic>> {
        let mut findings = Vec::new();
        let Some(run) = self.find_run(run_id)? else {
            findings.push(WorkflowIntegrityDiagnostic::MissingRun {
                run_id: run_id.to_string(),
            });
            return Ok(findings);
        };
        let journal = self.replay_journal(run_id)?;
        for (idx, event) in journal.iter().enumerate() {
            let expected = (idx + 1) as i64;
            if event.seq != expected {
                findings.push(WorkflowIntegrityDiagnostic::SequenceGap {
                    run_id: run_id.to_string(),
                    expected,
                    observed: event.seq,
                });
                break;
            }
        }
        let max_revision = journal.iter().map(|e| e.revision).max().unwrap_or(0);
        if run.lifecycle_revision != max_revision {
            findings.push(WorkflowIntegrityDiagnostic::ProjectionRevisionMismatch {
                run_id: run_id.to_string(),
                projection: run.lifecycle_revision,
                journal: max_revision,
            });
        }
        Ok(findings)
    }

    /// Rebuild authoritative lifecycle/node projections from append-only
    /// journal rows. A caller must run `diagnose_integrity` first; any finding
    /// is a dispatch stop condition until this repair completes cleanly.
    pub fn rebuild_projections(&self, run_id: &str) -> Result<()> {
        let journal = self.replay_journal(run_id)?;
        let Some(_) = self.find_run(run_id)? else {
            return Err(StorageError::NotFound(format!("workflow run {run_id}")));
        };
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        tx.execute(
            "DELETE FROM workflow_node_states WHERE run_id=?1",
            params![run_id],
        )?;
        let mut run_phase = "queued";
        let mut revision = 0_i64;
        for event in journal {
            revision = event.revision;
            run_phase = match event.event_kind.as_str() {
                "run_started" => "running",
                "run_completed" => "succeeded",
                "run_failed" => "failed",
                "run_cancelled" => "cancelled",
                "run_paused" => "suspended",
                "run_resumed" => "running",
                _ => run_phase,
            };
            if let Some(node_id) = event.node_id {
                let phase = match event.event_kind.as_str() {
                    "node_claimed" => "leased",
                    "node_started" => "running",
                    "node_completed" => "completed",
                    "node_retry_scheduled" => "ready",
                    "node_skipped" => "skipped",
                    "node_failed" => "failed",
                    "node_cancelled" => "cancelled",
                    _ => continue,
                };
                tx.execute("INSERT INTO workflow_node_states(run_id,node_id,phase,attempt,last_reducer_revision,last_event_seq,last_updated_at) VALUES (?1,?2,?3,0,?4,?5,?6) ON CONFLICT(run_id,node_id) DO UPDATE SET phase=excluded.phase,last_reducer_revision=excluded.last_reducer_revision,last_event_seq=excluded.last_event_seq,last_updated_at=excluded.last_updated_at", params![run_id,node_id,phase,event.revision,event.seq,event.created_at])?;
            }
        }
        tx.execute(
            "UPDATE workflow_runs SET lifecycle_phase=?2,lifecycle_revision=?3 WHERE id=?1",
            params![run_id, run_phase, revision],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Nonempty integrity diagnostics mean supervisor dispatch must remain
    /// disabled. This is a query seam for health/repair endpoints.
    pub fn dispatch_is_degraded(&self, run_id: &str) -> Result<bool> {
        Ok(!self.diagnose_integrity(run_id)?.is_empty())
    }

    pub fn enqueue_node(&self, row: &WorkflowQueueRow) -> Result<()> {
        self.conn.execute("INSERT INTO workflow_queue(run_id,node_id,available_at,priority) VALUES (?1,?2,?3,?4) ON CONFLICT(run_id,node_id) DO UPDATE SET available_at=excluded.available_at, priority=excluded.priority", params![row.run_id,row.node_id,row.available_at,row.priority])?;
        Ok(())
    }

    pub fn claim_next_node(
        &self,
        now: &str,
        holder: &str,
        fence: i64,
    ) -> Result<Option<WorkflowQueueRow>> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let row: Option<WorkflowQueueRow> = tx.query_row("SELECT run_id,node_id,available_at,priority,claimed_by,claim_fence FROM workflow_queue WHERE available_at <= ?1 AND claimed_by IS NULL ORDER BY priority DESC, available_at, run_id, node_id LIMIT 1", params![now], |r| Ok(WorkflowQueueRow { run_id:r.get(0)?, node_id:r.get(1)?, available_at:r.get(2)?, priority:r.get(3)?, claimed_by:r.get(4)?, claim_fence:r.get(5)? })).optional()?;
        if let Some(mut claimed) = row {
            let changed = tx.execute("UPDATE workflow_queue SET claimed_by=?1, claim_fence=?2 WHERE run_id=?3 AND node_id=?4 AND claimed_by IS NULL", params![holder,fence,claimed.run_id,claimed.node_id])?;
            if changed != 1 {
                tx.commit()?;
                return Ok(None);
            }
            claimed.claimed_by = Some(holder.to_string());
            claimed.claim_fence = Some(fence);
            tx.commit()?;
            return Ok(Some(claimed));
        }
        tx.commit()?;
        Ok(None)
    }

    /// Claim an executor-selected node by exact `(run_id,node_id)`. This CAS
    /// prevents a stale snapshot from claiming work another dispatcher owns.
    pub fn claim_node(
        &self,
        run_id: &str,
        node_id: &str,
        now: &str,
        holder: &str,
        fence: i64,
    ) -> Result<Option<WorkflowQueueRow>> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let changed = tx.execute(
            "UPDATE workflow_queue SET claimed_by=?1,claim_fence=?2
              WHERE run_id=?3 AND node_id=?4 AND available_at<=?5 AND claimed_by IS NULL",
            params![holder, fence, run_id, node_id, now],
        )?;
        let claimed = if changed == 1 {
            Some(tx.query_row(
                "SELECT run_id,node_id,available_at,priority,claimed_by,claim_fence FROM workflow_queue WHERE run_id=?1 AND node_id=?2",
                params![run_id,node_id], |row| Ok(WorkflowQueueRow { run_id: row.get(0)?, node_id: row.get(1)?, available_at: row.get(2)?, priority: row.get(3)?, claimed_by: row.get(4)?, claim_fence: row.get(5)? }),
            )?)
        } else {
            None
        };
        tx.commit()?;
        Ok(claimed)
    }

    /// Release only matching fenced ownership; stale workers cannot expose a
    /// newer claim back to queue.
    pub fn release_node_claim(
        &self,
        run_id: &str,
        node_id: &str,
        holder: &str,
        fence: i64,
    ) -> Result<bool> {
        Ok(self.conn.execute("UPDATE workflow_queue SET claimed_by=NULL,claim_fence=NULL WHERE run_id=?1 AND node_id=?2 AND claimed_by=?3 AND claim_fence=?4", params![run_id,node_id,holder,fence])? == 1)
    }

    pub fn record_command(&self, row: &WorkflowCommandRow) -> Result<bool> {
        let changed = self.conn.execute("INSERT OR IGNORE INTO workflow_commands(run_id,command_id,expected_revision,command_kind,payload_json,created_at) VALUES (?1,?2,?3,?4,?5,?6)", params![row.run_id,row.command_id,row.expected_revision,row.command_kind,row.payload_json,row.created_at])?;
        Ok(changed == 1)
    }

    pub fn insert_claim(&self, row: &WorkflowClaimRow) -> Result<()> {
        self.conn.execute("INSERT INTO workflow_claims(id,run_id,node_id,claim_key,statement,state,statement_hash,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![row.id,row.run_id,row.node_id,row.claim_key,row.statement,row.state,row.statement_hash,row.created_at,row.updated_at])?;
        Ok(())
    }

    pub fn insert_claim_evidence(&self, row: &WorkflowClaimEvidenceRow) -> Result<()> {
        self.conn.execute("INSERT INTO workflow_evidence(id,claim_id,artifact_ref,evidence_type,source_json,verdict,evidence_hash,reviewer_attempt_id,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![row.id,row.claim_id,row.artifact_ref,row.evidence_type,row.source_json,row.verdict,row.evidence_hash,row.reviewer_attempt_id,row.created_at])?;
        Ok(())
    }

    /// Record an attempt before dispatch. The unique generation index means a
    /// reclaimed node cannot receive writes from an earlier owner.
    pub fn insert_attempt(&self, row: &WorkflowAttemptRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workflow_attempts(
                id,run_id,node_id,attempt,lease_fence,state,started_at,finished_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                row.id,
                row.run_id,
                row.node_id,
                row.attempt,
                row.lease_fence,
                row.state,
                row.started_at,
                row.finished_at
            ],
        )?;
        Ok(())
    }

    /// Refresh a live attempt only when its exact fencing generation and
    /// current holder still own its queue claim. Returns `false` for stale or
    /// expired ownership; callers preserve that message as audit evidence.
    pub fn heartbeat_attempt(&self, update: &WorkflowAttemptUpdate<'_>) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE workflow_attempts
                SET heartbeat_at = ?4
              WHERE id = ?1 AND lease_fence = ?2
                AND state IN ('leased','running')
                AND EXISTS (
                    SELECT 1 FROM workflow_queue q
                     WHERE q.run_id = workflow_attempts.run_id
                       AND q.node_id = workflow_attempts.node_id
                       AND q.claimed_by = ?3
                       AND q.claim_fence = ?2
                )",
            params![update.id, update.generation, update.holder, update.at],
        )?;
        Ok(changed == 1)
    }

    /// Complete a live attempt with generation fencing. Completion clears the
    /// queue claim only in its same transaction, so a stale executor cannot
    /// erase a newer claimant's lease.
    pub fn complete_attempt(
        &self,
        update: &WorkflowAttemptUpdate<'_>,
        terminal_state: &str,
        result_json: Option<&str>,
        failure_json: Option<&str>,
    ) -> Result<bool> {
        if !matches!(
            terminal_state,
            "succeeded" | "failed" | "cancelled" | "uncertain"
        ) {
            return Err(StorageError::Serde(format!(
                "workflow_attempts: invalid terminal state {terminal_state:?}"
            )));
        }
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let changed = tx.execute(
            "UPDATE workflow_attempts
                SET state = ?4, finished_at = ?5, heartbeat_at = ?5,
                    result_json = ?6, failure_json = ?7
              WHERE id = ?1 AND lease_fence = ?2
                AND state IN ('leased','running')
                AND EXISTS (
                    SELECT 1 FROM workflow_queue q
                     WHERE q.run_id = workflow_attempts.run_id
                       AND q.node_id = workflow_attempts.node_id
                       AND q.claimed_by = ?3 AND q.claim_fence = ?2
                )",
            params![
                update.id,
                update.generation,
                update.holder,
                terminal_state,
                update.at,
                result_json,
                failure_json
            ],
        )?;
        if changed == 1 {
            tx.execute(
                "UPDATE workflow_queue
                    SET claimed_by = NULL, claim_fence = NULL
                  WHERE claimed_by = ?1 AND claim_fence = ?2
                    AND EXISTS (
                        SELECT 1 FROM workflow_attempts a
                         WHERE a.id = ?3 AND a.run_id = workflow_queue.run_id
                           AND a.node_id = workflow_queue.node_id
                    )",
                params![update.holder, update.generation, update.id],
            )?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn insert_import_quarantine(&self, row: &WorkflowImportQuarantineRow) -> Result<()> {
        self.conn.execute("INSERT INTO workflow_import_quarantine(id,raw_artifact_ref,content_hash,signature_status,manifest_json,state,reviewer,reason,created_at,reviewed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![row.id,row.raw_artifact_ref,row.content_hash,row.signature_status,row.manifest_json,row.state,row.reviewer,row.reason,row.created_at,row.reviewed_at])?;
        Ok(())
    }

    pub fn list_import_quarantine(&self, limit: i64) -> Result<Vec<WorkflowImportQuarantineRow>> {
        let mut stmt = self.conn.prepare("SELECT id,raw_artifact_ref,content_hash,signature_status,manifest_json,state,reviewer,reason,created_at,reviewed_at FROM workflow_import_quarantine ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], map_import_quarantine_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn review_import_quarantine(
        &self,
        id: &str,
        reviewer: &str,
        reason: &str,
        at: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute("UPDATE workflow_import_quarantine SET reviewer=?2,reason=?3,reviewed_at=?4,state='disabled' WHERE id=?1", params![id, reviewer, reason, at])? == 1)
    }

    pub fn insert_promotion_receipt(&self, row: &WorkflowPromotionReceiptRow) -> Result<()> {
        self.conn.execute("INSERT INTO workflow_promotion_receipts(proposal_id,approver,benchmark_id,activation_id,rollback_id,monitoring_id,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![row.proposal_id,row.approver,row.benchmark_id,row.activation_id,row.rollback_id,row.monitoring_id,row.state,row.created_at,row.updated_at])?;
        Ok(())
    }

    pub fn list_promotion_receipts(&self, limit: i64) -> Result<Vec<WorkflowPromotionReceiptRow>> {
        let mut stmt = self.conn.prepare("SELECT proposal_id,approver,benchmark_id,activation_id,rollback_id,monitoring_id,state,created_at,updated_at FROM workflow_promotion_receipts ORDER BY updated_at DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(WorkflowPromotionReceiptRow {
                    proposal_id: row.get(0)?,
                    approver: row.get(1)?,
                    benchmark_id: row.get(2)?,
                    activation_id: row.get(3)?,
                    rollback_id: row.get(4)?,
                    monitoring_id: row.get(5)?,
                    state: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn rollback_promotion(&self, proposal_id: &str, at: &str) -> Result<bool> {
        Ok(self.conn.execute("UPDATE workflow_promotion_receipts SET state='rolled_back',updated_at=?2 WHERE proposal_id=?1 AND state='active'", params![proposal_id, at])? == 1)
    }

    pub fn record_monitoring(&self, row: &WorkflowMonitoringReceiptRow) -> Result<()> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        tx.execute("INSERT INTO workflow_monitoring_receipts(id,proposal_id,monitoring_id,passed,reason,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![row.id,row.proposal_id,row.monitoring_id,row.passed,row.reason,row.created_at])?;
        if !row.passed {
            tx.execute("UPDATE workflow_promotion_receipts SET state='rolled_back',updated_at=?2 WHERE proposal_id=?1", params![row.proposal_id,row.created_at])?;
        }
        tx.commit()?;
        Ok(())
    }
}
