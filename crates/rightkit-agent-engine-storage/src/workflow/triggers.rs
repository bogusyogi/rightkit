//! Trigger admission and persistence: writes to `workflow_triggers`,
//! `workflow_trigger_events`, and `workflow_occurrences`, and — when a
//! trigger admits a run — delegates to [`WorkflowJournalWriter`] inside
//! the same transaction.

use super::journal::WorkflowJournalWriter;
use super::models::*;
use crate::error::{Result, StorageError};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};

pub struct WorkflowTriggerWriter<'a> {
    conn: &'a Connection,
}

impl<'a> WorkflowTriggerWriter<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Unified HMAC trigger admission. Raw artifact reference, trust/signature
    /// metadata, normalized event, occurrence and optional run admission share
    /// one transaction.
    pub fn admit_hmac_envelope(
        &self,
        artifact_store: &dyn WorkflowTriggerArtifactStore,
        envelope: &HmacAdmittedTriggerEnvelope,
        run: Option<&TriggerRunAdmission<'_>>,
    ) -> Result<TriggerAdmissionOutcome> {
        if envelope.hmac_key_id.is_empty()
            || !envelope.signature_hash.starts_with("sha256:")
            || envelope.event.raw_artifact_id.as_deref() != Some(envelope.raw_artifact.id.as_str())
            || envelope.event.content_hash != envelope.raw_artifact.content_hash
            || envelope.event.source != envelope.raw_artifact.source
            || envelope.raw_artifact.bytes.is_empty()
        {
            return Err(StorageError::Serde(
                "trigger requires verified HMAC and raw artifact ref".into(),
            ));
        }
        if let Some((event_id, occurrence_id, run_id)) = self.conn.query_row(
            "SELECT a.trigger_event_id,a.occurrence_id,a.run_id FROM workflow_trigger_admissions a JOIN workflow_trigger_events e ON e.id=a.trigger_event_id WHERE e.source=?1 AND e.dedupe_key=?2",
            params![envelope.event.source,envelope.event.dedupe_key], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?)),
        ).optional()? {
            return Ok(TriggerAdmissionOutcome { trigger_event_id:event_id,occurrence_id,run_id,duplicate:true,trigger_advanced:false });
        }
        let artifact_put = artifact_store.put_content_addressed(&envelope.raw_artifact)?;
        let raw_artifact = &artifact_put.row;
        if raw_artifact.id != envelope.raw_artifact.id
            || raw_artifact.content_hash != envelope.raw_artifact.content_hash
            || raw_artifact.source != envelope.raw_artifact.source
            || raw_artifact.byte_length != envelope.raw_artifact.bytes.len() as i64
            || raw_artifact.storage_uri.is_empty()
        {
            if artifact_put.created {
                artifact_store.remove_content_addressed(raw_artifact)?;
            }
            return Err(StorageError::Serde(
                "artifact store returned mismatched trigger artifact".into(),
            ));
        }
        let result = (|| -> Result<TriggerAdmissionOutcome> {
            let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
            if let Some((event_id, occurrence_id, run_id)) = tx.query_row(
            "SELECT a.trigger_event_id,a.occurrence_id,a.run_id FROM workflow_trigger_admissions a JOIN workflow_trigger_events e ON e.id=a.trigger_event_id WHERE e.source=?1 AND e.dedupe_key=?2",
            params![envelope.event.source,envelope.event.dedupe_key], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?)),
        ).optional()? {
            tx.commit()?;
            return Ok(TriggerAdmissionOutcome { trigger_event_id:event_id,occurrence_id,run_id,duplicate:true,trigger_advanced:false });
        }
            tx.execute(
            "INSERT OR IGNORE INTO workflow_trigger_raw_artifacts(id,content_hash,source,provenance_json,trust_json,storage_uri,metadata_json,byte_length,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                raw_artifact.id,
                raw_artifact.content_hash,
                raw_artifact.source,
                raw_artifact.provenance_json,
                raw_artifact.trust_json,
                raw_artifact.storage_uri,
                raw_artifact.metadata_json,
                raw_artifact.byte_length,
                raw_artifact.created_at,
            ],
        )?;
            let stored_artifact: (String, String, String, i64) = tx.query_row(
            "SELECT content_hash,source,storage_uri,byte_length FROM workflow_trigger_raw_artifacts WHERE id=?1",
            params![envelope.raw_artifact.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
            if stored_artifact.0 != envelope.raw_artifact.content_hash
                || stored_artifact.1 != envelope.raw_artifact.source
                || stored_artifact.2 != raw_artifact.storage_uri
                || stored_artifact.3 != envelope.raw_artifact.bytes.len() as i64
            {
                return Err(StorageError::Serde(
                    "trigger artifact id reused with different content".into(),
                ));
            }
            let trust_json = serde_json::json!({"hmacKeyId":envelope.hmac_key_id,"signatureHash":envelope.signature_hash,"admitted":true}).to_string();
            tx.execute("INSERT INTO workflow_trigger_events(id,trigger_id,source,event_type,external_id,dedupe_key,content_hash,trust_json,data_json,raw_artifact_id,occurred_at,received_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)", params![envelope.event.id,envelope.event.trigger_id,envelope.event.source,envelope.event.event_type,envelope.event.external_id,envelope.event.dedupe_key,envelope.event.content_hash,trust_json,envelope.event.data_json,envelope.event.raw_artifact_id,envelope.event.occurred_at,envelope.event.received_at])?;
            tx.execute("INSERT INTO workflow_occurrences(id,trigger_id,trigger_event_id,nominal_at_unix_ms,decision,decision_reason,selected_version_id,concurrency_key,dedupe_key,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![envelope.occurrence.id,envelope.occurrence.trigger_id,envelope.event.id,envelope.occurrence.nominal_at_unix_ms,envelope.occurrence.decision,envelope.occurrence.decision_reason,envelope.occurrence.selected_version_id,envelope.occurrence.concurrency_key,envelope.occurrence.dedupe_key,envelope.occurrence.created_at])?;
            let run_id = if let Some(admit) = run {
                if !matches!(admit.reducer_command.kind.as_str(), "admit" | "start")
                    || admit.reducer_command.command_id != admit.command.command_id
                    || admit.reducer_command.kind != admit.command.command_kind
                    || admit.reducer_command.expected_revision.0
                        != admit.command.expected_revision as u64
                    || admit.reducer_command.payload_json != admit.command.payload_json
                    || admit.reducer_command.run_id.stable().as_str() != admit.run.id
                {
                    return Err(StorageError::Serde(
                        "trigger run admission command mismatch".into(),
                    ));
                }
                let reduced = rightkit_workflow::control_plane::reducer::reduce(
                    admit.snapshot,
                    admit.reducer_command,
                );
                if let rightkit_workflow::control_plane::reducer::ReducerOutcome::Rejected {
                    reason,
                    ..
                } = reduced
                {
                    return Err(StorageError::Serde(format!(
                        "trigger run reducer rejected: {reason}"
                    )));
                }
                if admit.run.initiated_by_ref.as_deref() != Some(envelope.occurrence.id.as_str())
                    || admit.command.run_id != admit.run.id
                    || admit.event.run_id != admit.run.id
                    || admit.projection.run_id != admit.run.id
                    || admit.root_queue.run_id != admit.run.id
                    || envelope.occurrence.selected_version_id.as_deref()
                        != Some(admit.run.workflow_version_id.as_str())
                {
                    return Err(StorageError::Serde("trigger run binding mismatch".into()));
                }
                tx.execute("INSERT INTO workflow_runs(id,workflow_id,workflow_version_id,parent_run_id,lifecycle_phase,lifecycle_revision,initiated_by_kind,initiated_by_ref,trigger_payload_json,input_json,started_at,terminal_at,terminal_failure_class,runs_root_durable) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![admit.run.id,admit.run.workflow_id,admit.run.workflow_version_id,admit.run.parent_run_id,admit.run.lifecycle_phase,admit.run.lifecycle_revision,admit.run.initiated_by_kind,admit.run.initiated_by_ref,admit.run.trigger_payload_json,admit.run.input_json,admit.run.started_at,admit.run.terminal_at,admit.run.terminal_failure_class,admit.run.runs_root_durable as i64])?;
                tx.execute("INSERT INTO workflow_commands(run_id,command_id,expected_revision,command_kind,payload_json,created_at,outcome_json) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![admit.command.run_id,admit.command.command_id,admit.command.expected_revision,admit.command.command_kind,admit.command.payload_json,admit.command.created_at,admit.outcome_json])?;
                WorkflowJournalWriter::append_with_projection_in_tx(
                    &tx,
                    admit.event,
                    admit.projection,
                )?;
                tx.execute("INSERT INTO workflow_queue(run_id,node_id,available_at,priority) VALUES (?1,?2,?3,?4)", params![admit.root_queue.run_id,admit.root_queue.node_id,admit.root_queue.available_at,admit.root_queue.priority])?;
                Some(admit.run.id.clone())
            } else {
                None
            };
            tx.execute(
            "INSERT INTO workflow_trigger_admissions(trigger_event_id,occurrence_id,run_id,raw_artifact_id,content_hash,hmac_key_id,signature_hash,admitted_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                envelope.event.id,
                envelope.occurrence.id,
                run_id,
                envelope.event.raw_artifact_id,
                envelope.event.content_hash,
                envelope.hmac_key_id,
                envelope.signature_hash,
                envelope.event.received_at,
            ],
        )?;
            tx.commit()?;
            Ok(TriggerAdmissionOutcome {
                trigger_event_id: envelope.event.id.clone(),
                occurrence_id: envelope.occurrence.id.clone(),
                run_id,
                duplicate: false,
                trigger_advanced: false,
            })
        })();
        if result.is_err() && artifact_put.created {
            artifact_store.remove_content_addressed(raw_artifact)?;
        }
        result
    }

    /// Atomically admits a trusted daemon-originated event, its policy
    /// decision, and an optional root run. Manual/run-now triggers do not
    /// manufacture HMAC evidence or raw-artifact rows.
    pub fn admit_internal_event(
        &self,
        event: &WorkflowTriggerEventRow,
        occurrence: &WorkflowOccurrenceRow,
        run: Option<&TriggerRunAdmission<'_>>,
    ) -> Result<TriggerAdmissionOutcome> {
        self.admit_internal_event_with_schedule(event, occurrence, run, None)
    }

    pub fn admit_scheduled_event(
        &self,
        event: &WorkflowTriggerEventRow,
        occurrence: &WorkflowOccurrenceRow,
        run: Option<&TriggerRunAdmission<'_>>,
        expected_next_fire_at_unix_ms: i64,
        next_fire_at_unix_ms: i64,
        updated_at: &str,
    ) -> Result<TriggerAdmissionOutcome> {
        self.admit_internal_event_with_schedule(
            event,
            occurrence,
            run,
            Some((
                expected_next_fire_at_unix_ms,
                next_fire_at_unix_ms,
                updated_at,
            )),
        )
    }

    fn admit_internal_event_with_schedule(
        &self,
        event: &WorkflowTriggerEventRow,
        occurrence: &WorkflowOccurrenceRow,
        run: Option<&TriggerRunAdmission<'_>>,
        schedule: Option<(i64, i64, &str)>,
    ) -> Result<TriggerAdmissionOutcome> {
        if event.trigger_id.as_deref() != Some(occurrence.trigger_id.as_str())
            || occurrence.trigger_event_id.as_deref() != Some(event.id.as_str())
            || event.source.trim().is_empty()
            || event.dedupe_key.trim().is_empty()
        {
            return Err(StorageError::Serde(
                "internal trigger event/occurrence binding mismatch".into(),
            ));
        }
        if let Some((event_id, occurrence_id, run_id)) = self
            .conn
            .query_row(
                "SELECT e.id,o.id,r.id FROM workflow_trigger_events e JOIN workflow_occurrences o ON o.trigger_event_id=e.id LEFT JOIN workflow_runs r ON r.initiated_by_ref=o.id WHERE e.source=?1 AND e.dedupe_key=?2",
                params![event.source, event.dedupe_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
            )
            .optional()?
        {
            let trigger_advanced = if let Some((_, next, _)) = schedule {
                self.conn
                    .query_row(
                        "SELECT next_fire_at_unix_ms>=?1 FROM workflow_triggers WHERE id=?2",
                        params![next, occurrence.trigger_id],
                        |row| row.get::<_, bool>(0),
                    )
                    .optional()?
                    .unwrap_or(false)
            } else {
                false
            };
            return Ok(TriggerAdmissionOutcome {
                trigger_event_id: event_id,
                occurrence_id,
                run_id,
                duplicate: true,
                trigger_advanced,
            });
        }
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        if let Some((expected, next, updated_at)) = schedule {
            let changed = tx.execute(
                "UPDATE workflow_triggers SET next_fire_at_unix_ms=?1,revision=revision+1,updated_at=?2 WHERE id=?3 AND enabled=1 AND kind='cron' AND next_fire_at_unix_ms=?4",
                params![next, updated_at, occurrence.trigger_id, expected],
            )?;
            if changed != 1 {
                if let Some((event_id, occurrence_id, run_id)) = tx
                    .query_row(
                        "SELECT e.id,o.id,r.id FROM workflow_trigger_events e JOIN workflow_occurrences o ON o.trigger_event_id=e.id LEFT JOIN workflow_runs r ON r.initiated_by_ref=o.id WHERE e.source=?1 AND e.dedupe_key=?2",
                        params![event.source, event.dedupe_key],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
                    )
                    .optional()?
                {
                    tx.commit()?;
                    return Ok(TriggerAdmissionOutcome {
                        trigger_event_id: event_id,
                        occurrence_id,
                        run_id,
                        duplicate: true,
                        trigger_advanced: true,
                    });
                }
                return Err(StorageError::Serde(
                    "scheduled trigger cursor lost CAS".into(),
                ));
            }
        }
        tx.execute(
            "INSERT INTO workflow_trigger_events(id,trigger_id,source,event_type,external_id,dedupe_key,content_hash,trust_json,data_json,raw_artifact_id,occurred_at,received_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL,?10,?11)",
            params![event.id,event.trigger_id,event.source,event.event_type,event.external_id,event.dedupe_key,event.content_hash,event.trust_json,event.data_json,event.occurred_at,event.received_at],
        )?;
        tx.execute(
            "INSERT INTO workflow_occurrences(id,trigger_id,trigger_event_id,nominal_at_unix_ms,decision,decision_reason,selected_version_id,concurrency_key,dedupe_key,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![occurrence.id,occurrence.trigger_id,event.id,occurrence.nominal_at_unix_ms,occurrence.decision,occurrence.decision_reason,occurrence.selected_version_id,occurrence.concurrency_key,occurrence.dedupe_key,occurrence.created_at],
        )?;
        let run_id = if let Some(admit) = run {
            if !matches!(admit.reducer_command.kind.as_str(), "admit" | "start")
                || admit.reducer_command.command_id != admit.command.command_id
                || admit.reducer_command.kind != admit.command.command_kind
                || admit.reducer_command.expected_revision.0
                    != admit.command.expected_revision as u64
                || admit.reducer_command.payload_json != admit.command.payload_json
                || admit.reducer_command.run_id.stable().as_str() != admit.run.id
                || admit.run.initiated_by_ref.as_deref() != Some(occurrence.id.as_str())
                || admit.command.run_id != admit.run.id
                || admit.event.run_id != admit.run.id
                || admit.projection.run_id != admit.run.id
                || admit.root_queue.run_id != admit.run.id
                || occurrence.selected_version_id.as_deref()
                    != Some(admit.run.workflow_version_id.as_str())
            {
                return Err(StorageError::Serde(
                    "internal trigger run admission binding mismatch".into(),
                ));
            }
            if let rightkit_workflow::control_plane::reducer::ReducerOutcome::Rejected {
                reason,
                ..
            } = rightkit_workflow::control_plane::reducer::reduce(
                admit.snapshot,
                admit.reducer_command,
            ) {
                return Err(StorageError::Serde(format!(
                    "internal trigger reducer rejected: {reason}"
                )));
            }
            tx.execute("INSERT INTO workflow_runs(id,workflow_id,workflow_version_id,parent_run_id,lifecycle_phase,lifecycle_revision,initiated_by_kind,initiated_by_ref,trigger_payload_json,input_json,started_at,terminal_at,terminal_failure_class,runs_root_durable) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![admit.run.id,admit.run.workflow_id,admit.run.workflow_version_id,admit.run.parent_run_id,admit.run.lifecycle_phase,admit.run.lifecycle_revision,admit.run.initiated_by_kind,admit.run.initiated_by_ref,admit.run.trigger_payload_json,admit.run.input_json,admit.run.started_at,admit.run.terminal_at,admit.run.terminal_failure_class,admit.run.runs_root_durable as i64])?;
            tx.execute("INSERT INTO workflow_commands(run_id,command_id,expected_revision,command_kind,payload_json,created_at,outcome_json) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![admit.command.run_id,admit.command.command_id,admit.command.expected_revision,admit.command.command_kind,admit.command.payload_json,admit.command.created_at,admit.outcome_json])?;
            WorkflowJournalWriter::append_with_projection_in_tx(
                &tx,
                admit.event,
                admit.projection,
            )?;
            tx.execute("INSERT INTO workflow_queue(run_id,node_id,available_at,priority) VALUES (?1,?2,?3,?4)", params![admit.root_queue.run_id,admit.root_queue.node_id,admit.root_queue.available_at,admit.root_queue.priority])?;
            Some(admit.run.id.clone())
        } else {
            None
        };
        tx.commit()?;
        Ok(TriggerAdmissionOutcome {
            trigger_event_id: event.id.clone(),
            occurrence_id: occurrence.id.clone(),
            run_id,
            duplicate: false,
            trigger_advanced: schedule.is_some(),
        })
    }

    fn find_trigger_event_by_dedupe(
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

    /// Insert a trigger event, or return the existing row when
    /// `(source, dedupe_key)` is already present. This is the durable
    /// idempotency hook the supervisor uses for webhook / GitHub / internal
    /// redelivery: the upstream can fire the same event multiple times and
    /// the storage layer collapses the duplicates without surfacing a
    /// constraint error.
    pub fn insert_trigger_event_idempotent(
        &self,
        event: &WorkflowTriggerEventRow,
    ) -> Result<TriggerEventInsert> {
        // Fast-path probe: if the row already exists, return it without an
        // INSERT attempt. This avoids a needless constraint-violation round
        // trip on the common redelivery case.
        if let Some(existing) =
            self.find_trigger_event_by_dedupe(&event.source, &event.dedupe_key)?
        {
            return Ok(TriggerEventInsert::Duplicate(existing));
        }
        match self.conn.execute(
            "INSERT INTO workflow_trigger_events(
                id, trigger_id, source, event_type, external_id, dedupe_key,
                content_hash, trust_json, data_json, raw_artifact_id,
                occurred_at, received_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                event.id,
                event.trigger_id,
                event.source,
                event.event_type,
                event.external_id,
                event.dedupe_key,
                event.content_hash,
                event.trust_json,
                event.data_json,
                event.raw_artifact_id,
                event.occurred_at,
                event.received_at,
            ],
        ) {
            Ok(_) => Ok(TriggerEventInsert::Inserted(event.clone())),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                // Lost a race with another writer. Re-read and report
                // Duplicate with the persisted row so the caller sees the
                // canonical record.
                let existing = self
                    .find_trigger_event_by_dedupe(&event.source, &event.dedupe_key)?
                    .ok_or_else(|| {
                        StorageError::Serde(format!(
                            "workflow_trigger_events: constraint violation on insert but no row found for source={} dedupe_key={}",
                            event.source, event.dedupe_key
                        ))
                    })?;
                Ok(TriggerEventInsert::Duplicate(existing))
            }
            Err(e) => Err(StorageError::from(e)),
        }
    }

    /// Insert an occurrence row. The `(trigger_id, dedupe_key)` unique
    /// index enforces idempotency; a duplicate is surfaced as
    /// `StorageError::Constraint` so the supervisor can distinguish
    /// "already admitted" from "first admission". The returned variant is
    /// derived from `row.decision` so callers can match on the typed
    /// outcome.
    pub fn admit_occurrence(&self, row: &WorkflowOccurrenceRow) -> Result<OccurrenceAdmission> {
        self.conn.execute(
            "INSERT INTO workflow_occurrences(
                id, trigger_id, trigger_event_id, nominal_at_unix_ms,
                decision, decision_reason, selected_version_id,
                concurrency_key, dedupe_key, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                row.id,
                row.trigger_id,
                row.trigger_event_id,
                row.nominal_at_unix_ms,
                row.decision,
                row.decision_reason,
                row.selected_version_id,
                row.concurrency_key,
                row.dedupe_key,
                row.created_at,
            ],
        )?;
        Ok(match row.decision.as_str() {
            "admitted" => OccurrenceAdmission::Admitted(row.clone()),
            "coalesced" => OccurrenceAdmission::Coalesced(row.clone()),
            "skipped" => OccurrenceAdmission::Skipped(row.clone()),
            "queued" => OccurrenceAdmission::Queued(row.clone()),
            "replaced" => OccurrenceAdmission::Replaced(row.clone()),
            "rejected" => OccurrenceAdmission::Rejected(row.clone()),
            other => {
                return Err(StorageError::Serde(format!(
                    "workflow_occurrences: refusing to admit with unknown decision {other:?}"
                )));
            }
        })
    }
}
