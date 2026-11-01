//! Durable storage for the autonomous workflow control plane (ADR-0021).
//!
//! Typed writers and one repository are the only things allowed to touch
//! workflow_* tables:
//!
//! * [`WorkflowJournalWriter`] — appends a single `JournalEvent` row to
//!   `workflow_journal` and applies the corresponding projection updates
//!   (`workflow_runs`, `workflow_node_states`, optional
//!   `workflow_run_lease`) **inside the same SQLite transaction**. Splitting
//!   those writes across two transactions would let a daemon crash leave the
//!   journal ahead of the projections, which is the exact inconsistency the
//!   reducer's optimistic-revision check is designed to reject on restart.
//!
//! * [`WorkflowRepository`] — typed queries plus fenced CAS transitions for
//!   queues, attempts, waits, effects, attention, & outbox rows.
//!
//! * [`WorkflowDefinitionWriter`] — separate from the journal writer because
//!   definitions and versions are content-addressed and indexed
//!   (`UNIQUE(workflow_id, version_number)`); they belong to a different
//!   transaction boundary (publish = single transaction).
//!
//! Row structs mirror durable columns; payload JSON stays opaque until its
//! owning domain adapter validates it.

#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::too_many_arguments)]

mod journal;
mod mappers;
mod models;
mod runs;
mod triggers;

pub use journal::WorkflowJournalWriter;
pub use models::*;
pub use runs::WorkflowRepository;
pub use triggers::WorkflowTriggerWriter;

use crate::error::Result;
use rightkit_db::rusqlite::{params, Connection, Transaction};

pub struct WorkflowDefinitionWriter<'a> {
    conn: &'a Connection,
}

impl<'a> WorkflowDefinitionWriter<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Upsert a definition row. Idempotent on `id`.
    pub fn upsert_definition(&self, def: &WorkflowDefinitionRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workflow_definitions(id, name, source_kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 source_kind = excluded.source_kind,
                 updated_at = excluded.updated_at",
            params![
                def.id,
                def.name,
                def.source_kind,
                def.created_at,
                def.updated_at
            ],
        )?;
        Ok(())
    }

    /// Insert a new version row. Returns `StorageError::Constraint` if the
    /// `(workflow_id, version_number)` pair is already taken — the daemon
    /// translates that into a "raced with another publish" rejection.
    pub fn insert_version(&self, row: &WorkflowVersionRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workflow_versions(
                id, workflow_id, version_number, content_hash,
                definition_json, compiled_plan_json, publication_state,
                created_at, published_at, created_by, notes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                row.id,
                row.workflow_id,
                row.version_number,
                row.content_hash,
                row.definition_json,
                row.compiled_plan_json,
                row.publication_state,
                row.created_at,
                row.published_at,
                row.created_by,
                row.notes,
            ],
        )?;
        Ok(())
    }

    /// Insert a version inside a caller-supplied transaction, used by the
    /// publish path that wants the definition upsert and the version
    /// insert in one atomic step.
    pub fn insert_version_in_tx(tx: &Transaction<'_>, row: &WorkflowVersionRow) -> Result<()> {
        tx.execute(
            "INSERT INTO workflow_versions(
                id, workflow_id, version_number, content_hash,
                definition_json, compiled_plan_json, publication_state,
                created_at, published_at, created_by, notes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                row.id,
                row.workflow_id,
                row.version_number,
                row.content_hash,
                row.definition_json,
                row.compiled_plan_json,
                row.publication_state,
                row.created_at,
                row.published_at,
                row.created_by,
                row.notes,
            ],
        )?;
        Ok(())
    }

    /// Begin a transaction for atomic publish. Caller commits.
    pub fn begin_publish(&self) -> Result<Transaction<'_>> {
        Ok(rightkit_db::write_txn_unchecked(&self.conn)?)
    }

    pub fn publish_version(&self, workflow_id: &str, version_id: &str, now: &str) -> Result<bool> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let changed = tx.execute("UPDATE workflow_versions SET publication_state='published',published_at=?1 WHERE id=?2 AND workflow_id=?3 AND publication_state='draft'", params![now, version_id, workflow_id])?;
        if changed != 1 {
            tx.rollback()?;
            return Ok(false);
        }
        tx.execute("UPDATE workflow_versions SET publication_state='retired' WHERE workflow_id=?1 AND id<>?2 AND publication_state='published'", params![workflow_id,version_id])?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Storage;
    // Only the tests construct error variants directly; keeping this out of
    // the module header stops it being an unused import in release builds.
    use crate::error::StorageError;

    struct TestTriggerArtifactStore;

    impl WorkflowTriggerArtifactStore for TestTriggerArtifactStore {
        fn put_content_addressed(
            &self,
            artifact: &WorkflowTriggerRawArtifactWrite,
        ) -> Result<WorkflowTriggerArtifactPut> {
            Ok(WorkflowTriggerArtifactPut {
                row: WorkflowTriggerRawArtifactRow {
                    id: artifact.id.clone(),
                    content_hash: artifact.content_hash.clone(),
                    source: artifact.source.clone(),
                    provenance_json: artifact.provenance_json.clone(),
                    trust_json: artifact.trust_json.clone(),
                    storage_uri: format!("artifact://{}", artifact.content_hash),
                    metadata_json: serde_json::json!({"mediaType":artifact.media_type}).to_string(),
                    byte_length: artifact.bytes.len() as i64,
                    created_at: artifact.created_at.clone(),
                },
                created: true,
            })
        }

        fn remove_content_addressed(&self, _: &WorkflowTriggerRawArtifactRow) -> Result<()> {
            Ok(())
        }
    }

    struct FailingTriggerArtifactStore;

    impl WorkflowTriggerArtifactStore for FailingTriggerArtifactStore {
        fn put_content_addressed(
            &self,
            _: &WorkflowTriggerRawArtifactWrite,
        ) -> Result<WorkflowTriggerArtifactPut> {
            Err(StorageError::Serde("artifact store unavailable".into()))
        }

        fn remove_content_addressed(&self, _: &WorkflowTriggerRawArtifactRow) -> Result<()> {
            Ok(())
        }
    }

    fn open_memory() -> Connection {
        // Use the public `Storage::open_in_memory` so we exercise the
        // real migration runner. The `conn` field is `pub(crate)` so the
        // submodule test below is allowed to take it by value.
        let storage = Storage::open_in_memory().expect("memory db");
        storage.conn
    }

    fn seed_run(conn: &Connection, run_id: &str) {
        conn.execute(
            "INSERT INTO workflow_runs(
                id, workflow_id, workflow_version_id,
                lifecycle_phase, lifecycle_revision,
                initiated_by_kind, input_json, started_at, runs_root_durable
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run_id,
                "wfd-seed",
                "wfv-seed",
                "queued",
                0_i64,
                "manual",
                "{}",
                "2026-08-07T00:00:00Z",
                1_i64,
            ],
        )
        .unwrap();
    }

    #[test]
    fn publish_claim_preserves_one_published_version_under_race() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("publish-race.sqlite");
        let storage = Storage::open(&path).expect("storage");
        for (id, number, state) in [
            ("wfv-one", 1, "published"),
            ("wfv-two", 2, "draft"),
            ("wfv-three", 3, "draft"),
        ] {
            storage
                .conn()
                .execute(
                    "INSERT INTO workflow_versions(id,workflow_id,version_number,content_hash,definition_json,compiled_plan_json,publication_state,created_at,created_by,notes) VALUES (?1,'wfd-race',?2,?3,'{}','{}',?4,'now','test','')",
                    params![id, number, format!("sha256:{number}"), state],
                )
                .expect("version");
        }
        assert!(!WorkflowDefinitionWriter::new(storage.conn())
            .publish_version("wfd-race", "wfv-missing", "later")
            .expect("invalid publish"));
        let still_published: String = storage
            .conn()
            .query_row(
                "SELECT id FROM workflow_versions WHERE workflow_id='wfd-race' AND publication_state='published'",
                [],
                |row| row.get(0),
            )
            .expect("existing published");
        assert_eq!(still_published, "wfv-one");
        drop(storage);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut threads = Vec::new();
        for version in ["wfv-two", "wfv-three"] {
            let path = path.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                let storage = Storage::open(path).expect("race storage");
                barrier.wait();
                WorkflowDefinitionWriter::new(storage.conn())
                    .publish_version("wfd-race", version, "race")
                    .expect("race publish")
            }));
        }
        assert!(threads
            .into_iter()
            .all(|thread| thread.join().expect("join")));
        let storage = Storage::open(&path).expect("reopen");
        let published: i64 = storage
            .conn()
            .query_row(
                "SELECT count(*) FROM workflow_versions WHERE workflow_id='wfd-race' AND publication_state='published'",
                [],
                |row| row.get(0),
            )
            .expect("published count");
        assert_eq!(published, 1);
    }

    #[test]
    fn append_with_projection_bumps_seq_and_revision_atomically() {
        let conn = open_memory();
        let run_id = "wfr-test-1";
        seed_run(&conn, run_id);

        let writer = WorkflowJournalWriter::new(&conn);
        let projection = ProjectionUpdate {
            run_id,
            run_phase: Some("running"),
            ..Default::default()
        };
        let event = JournalAppend {
            run_id,
            event_kind: "run_started",
            node_id: None,
            payload_json: "{}",
            created_at: "2026-08-07T00:00:01Z",
        };
        let outcome = writer
            .append_with_projection(&event, &projection)
            .expect("append");
        assert_eq!(outcome.seq, 1);
        assert_eq!(outcome.revision, 1);

        // Read back: journal has 1 row, run's revision advanced.
        let repo = WorkflowRepository::new(&conn);
        let journal = repo.replay_journal(run_id).expect("replay");
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].seq, 1);
        assert_eq!(journal[0].revision, 1);
        let run = repo.find_run(run_id).expect("find_run").expect("present");
        assert_eq!(run.lifecycle_phase, "running");
        assert_eq!(run.lifecycle_revision, 1);
    }

    #[test]
    fn rebuild_restores_projections_and_corruption_degrades_dispatch() {
        let conn = open_memory();
        let run_id = "wfr-rebuild";
        seed_run(&conn, run_id);
        let writer = WorkflowJournalWriter::new(&conn);
        writer
            .append_with_projection(
                &JournalAppend {
                    run_id,
                    event_kind: "run_started",
                    node_id: None,
                    payload_json: "{}",
                    created_at: "2026-08-07T00:00:01Z",
                },
                &ProjectionUpdate {
                    run_id,
                    run_phase: Some("running"),
                    ..Default::default()
                },
            )
            .expect("start");
        writer
            .append_with_projection(
                &JournalAppend {
                    run_id,
                    event_kind: "node_completed",
                    node_id: Some("node-1"),
                    payload_json: "{}",
                    created_at: "2026-08-07T00:00:02Z",
                },
                &ProjectionUpdate {
                    run_id,
                    node: Some(("node-1", "completed")),
                    ..Default::default()
                },
            )
            .expect("node");
        let repo = WorkflowRepository::new(&conn);
        assert!(!repo.dispatch_is_degraded(run_id).expect("healthy"));
        conn.execute(
            "UPDATE workflow_runs SET lifecycle_revision=99 WHERE id=?1",
            params![run_id],
        )
        .expect("corrupt");
        assert!(repo.dispatch_is_degraded(run_id).expect("degraded"));
        repo.rebuild_projections(run_id).expect("rebuild");
        assert!(!repo.dispatch_is_degraded(run_id).expect("repaired"));
        assert_eq!(
            repo.find_node_state(run_id, "node-1")
                .expect("node")
                .expect("present")
                .phase,
            "completed"
        );
    }

    #[test]
    fn run_coordinator_rejects_unbound_reducer_command() {
        let conn = open_memory();
        let run_id = "wfr-bound";
        seed_run(&conn, run_id);
        let writer = WorkflowJournalWriter::new(&conn);
        let snapshot = rightkit_workflow::control_plane::domain::LifecycleState {
            workflow_id: rightkit_workflow::control_plane::domain::WorkflowId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowDefinition,
                "seed",
            ),
            version_id: rightkit_workflow::control_plane::domain::WorkflowVersionId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowVersion,
                "seed",
            ),
            revision: rightkit_workflow::control_plane::domain::Revision(0),
            phase: rightkit_workflow::control_plane::domain::LifecyclePhase::Pending,
        };
        let reducer_command = rightkit_workflow::control_plane::reducer::Command {
            command_id: "cmd-bound".into(),
            kind: "start".into(),
            run_id: rightkit_workflow::control_plane::domain::RunId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowRun,
                "bound",
            ),
            expected_revision: rightkit_workflow::control_plane::domain::Revision(0),
            payload_json: "{}".into(),
        };
        let err = writer
            .apply_command_from_snapshot(
                &snapshot,
                &reducer_command,
                &WorkflowCommandRow {
                    run_id: run_id.into(),
                    command_id: "cmd-bound".into(),
                    expected_revision: 0,
                    command_kind: "fail".into(),
                    payload_json: "{}".into(),
                    created_at: "now".into(),
                },
                "{}",
                &JournalAppend {
                    run_id,
                    event_kind: "run_started",
                    node_id: None,
                    payload_json: "{}",
                    created_at: "now",
                },
                &ProjectionUpdate {
                    run_id,
                    run_phase: Some("running"),
                    ..Default::default()
                },
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect_err("mismatched persisted command must be refused");
        assert!(matches!(err, StorageError::Serde(_)));
        assert!(WorkflowRepository::new(&conn)
            .replay_journal(run_id)
            .expect("journal")
            .is_empty());
    }

    #[test]
    fn admission_is_idempotent_and_run_collision_rolls_back() {
        let conn = open_memory();
        let writer = WorkflowJournalWriter::new(&conn);
        let snapshot = rightkit_workflow::control_plane::domain::LifecycleState {
            workflow_id: rightkit_workflow::control_plane::domain::WorkflowId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowDefinition,
                "seed",
            ),
            version_id: rightkit_workflow::control_plane::domain::WorkflowVersionId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowVersion,
                "seed",
            ),
            revision: rightkit_workflow::control_plane::domain::Revision(0),
            phase: rightkit_workflow::control_plane::domain::LifecyclePhase::Pending,
        };
        let run_id = "wfr_admit";
        let reducer_command = rightkit_workflow::control_plane::reducer::Command {
            command_id: "cmd-admit".into(),
            kind: "start".into(),
            run_id: rightkit_workflow::control_plane::domain::RunId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowRun,
                "admit",
            ),
            expected_revision: rightkit_workflow::control_plane::domain::Revision(0),
            payload_json: "{}".into(),
        };
        let run = WorkflowRunRow {
            id: run_id.into(),
            workflow_id: "wfd-seed".into(),
            workflow_version_id: "wfv-seed".into(),
            parent_run_id: None,
            lifecycle_phase: "pending".into(),
            lifecycle_revision: 0,
            initiated_by_kind: "manual".into(),
            initiated_by_ref: Some("operator".into()),
            trigger_payload_json: None,
            input_json: "{}".into(),
            started_at: "2026-08-07T00:00:00Z".into(),
            terminal_at: None,
            terminal_failure_class: None,
            runs_root_durable: true,
        };
        let command = WorkflowCommandRow {
            run_id: run_id.into(),
            command_id: "cmd-admit".into(),
            expected_revision: 0,
            command_kind: "start".into(),
            payload_json: "{}".into(),
            created_at: "2026-08-07T00:00:00Z".into(),
        };
        let event = JournalAppend {
            run_id,
            event_kind: "run_started",
            node_id: None,
            payload_json: "{}",
            created_at: "2026-08-07T00:00:00Z",
        };
        let projection = ProjectionUpdate {
            run_id,
            run_phase: Some("running"),
            ..Default::default()
        };
        let root = WorkflowQueueRow {
            run_id: run_id.into(),
            node_id: "root".into(),
            available_at: "2026-08-07T00:00:00Z".into(),
            priority: 0,
            claimed_by: None,
            claim_fence: None,
        };
        assert!(matches!(
            writer
                .admit_run_from_snapshot(
                    &snapshot,
                    &reducer_command,
                    &run,
                    &command,
                    "{}",
                    &event,
                    &projection,
                    &root
                )
                .expect("admit"),
            WorkflowCommandApplyOutcome::Applied(_)
        ));
        assert!(matches!(
            writer
                .admit_run_from_snapshot(
                    &snapshot,
                    &reducer_command,
                    &run,
                    &command,
                    "{}",
                    &event,
                    &projection,
                    &root
                )
                .expect("dedupe"),
            WorkflowCommandApplyOutcome::Duplicate { .. }
        ));
        let repo = WorkflowRepository::new(&conn);
        assert_eq!(repo.replay_journal(run_id).expect("journal").len(), 1);
        let collision_command = WorkflowCommandRow {
            command_id: "cmd-collision".into(),
            ..command.clone()
        };
        assert!(writer
            .admit_run_from_snapshot(
                &snapshot,
                &rightkit_workflow::control_plane::reducer::Command {
                    command_id: "cmd-collision".into(),
                    ..reducer_command.clone()
                },
                &run,
                &collision_command,
                "{}",
                &event,
                &projection,
                &root
            )
            .is_err());
        assert!(repo.find_run(run_id).expect("run").is_some());
        assert_eq!(repo.replay_journal(run_id).expect("journal").len(), 1);
    }

    #[test]
    fn hmac_trigger_admission_is_atomic_and_idempotent() {
        let conn = open_memory();
        let event = WorkflowTriggerEventRow {
            id: "wfte-trigger-admit".into(),
            trigger_id: Some("wft-trigger-admit".into()),
            source: "webhook".into(),
            event_type: "push".into(),
            external_id: Some("delivery-1".into()),
            dedupe_key: "delivery-1".into(),
            content_hash: "sha256:body".into(),
            trust_json: "{}".into(),
            data_json: "{}".into(),
            raw_artifact_id: Some("artifact-1".into()),
            occurred_at: "2026-08-07T00:00:00Z".into(),
            received_at: "2026-08-07T00:00:01Z".into(),
        };
        let occurrence = WorkflowOccurrenceRow {
            id: "wfo-trigger-admit".into(),
            trigger_id: "wft-trigger-admit".into(),
            trigger_event_id: Some(event.id.clone()),
            nominal_at_unix_ms: 1,
            decision: "admitted".into(),
            decision_reason: "eligible".into(),
            selected_version_id: Some("wfv-seed".into()),
            concurrency_key: Some("workspace-1".into()),
            dedupe_key: "delivery-1".into(),
            created_at: "2026-08-07T00:00:01Z".into(),
        };
        let envelope = HmacAdmittedTriggerEnvelope {
            event,
            occurrence,
            raw_artifact: WorkflowTriggerRawArtifactWrite {
                id: "artifact-1".into(),
                content_hash: "sha256:body".into(),
                source: "webhook".into(),
                provenance_json: "{}".into(),
                trust_json: "{\"authenticated\":true}".into(),
                media_type: "application/json".into(),
                bytes: br#"{"push":true}"#.to_vec(),
                created_at: "2026-08-07T00:00:01Z".into(),
            },
            hmac_key_id: "webhook-key-1".into(),
            signature_hash: "sha256:signature".into(),
        };
        let run_id = "wfr_trigger_admit";
        let snapshot = rightkit_workflow::control_plane::domain::LifecycleState {
            workflow_id: rightkit_workflow::control_plane::domain::WorkflowId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowDefinition,
                "seed",
            ),
            version_id: rightkit_workflow::control_plane::domain::WorkflowVersionId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowVersion,
                "seed",
            ),
            revision: rightkit_workflow::control_plane::domain::Revision(0),
            phase: rightkit_workflow::control_plane::domain::LifecyclePhase::Pending,
        };
        let reducer_command = rightkit_workflow::control_plane::reducer::Command {
            command_id: "cmd-trigger-admit".into(),
            kind: "start".into(),
            run_id: rightkit_workflow::control_plane::domain::RunId::new(
                rightkit_workflow::control_plane::ids::Prefix::WorkflowRun,
                "trigger_admit",
            ),
            expected_revision: rightkit_workflow::control_plane::domain::Revision(0),
            payload_json: "{}".into(),
        };
        let run = WorkflowRunRow {
            id: run_id.into(),
            workflow_id: "wfd-seed".into(),
            workflow_version_id: "wfv-seed".into(),
            parent_run_id: None,
            lifecycle_phase: "pending".into(),
            lifecycle_revision: 0,
            initiated_by_kind: "trigger".into(),
            initiated_by_ref: Some(envelope.occurrence.id.clone()),
            trigger_payload_json: Some("{}".into()),
            input_json: "{}".into(),
            started_at: "2026-08-07T00:00:01Z".into(),
            terminal_at: None,
            terminal_failure_class: None,
            runs_root_durable: true,
        };
        let command = WorkflowCommandRow {
            run_id: run_id.into(),
            command_id: "cmd-trigger-admit".into(),
            expected_revision: 0,
            command_kind: "start".into(),
            payload_json: "{}".into(),
            created_at: "2026-08-07T00:00:01Z".into(),
        };
        let journal = JournalAppend {
            run_id,
            event_kind: "run_started",
            node_id: None,
            payload_json: "{}",
            created_at: "2026-08-07T00:00:01Z",
        };
        let projection = ProjectionUpdate {
            run_id,
            run_phase: Some("running"),
            ..Default::default()
        };
        let root_queue = WorkflowQueueRow {
            run_id: run_id.into(),
            node_id: "root".into(),
            available_at: "2026-08-07T00:00:01Z".into(),
            priority: 0,
            claimed_by: None,
            claim_fence: None,
        };
        let admission = TriggerRunAdmission {
            snapshot: &snapshot,
            reducer_command: &reducer_command,
            run: &run,
            command: &command,
            outcome_json: "{}",
            event: &journal,
            projection: &projection,
            root_queue: &root_queue,
        };
        let writer = WorkflowTriggerWriter::new(&conn);
        let artifact_store = TestTriggerArtifactStore;
        let applied = writer
            .admit_hmac_envelope(&artifact_store, &envelope, Some(&admission))
            .expect("admit trigger run");
        assert!(!applied.duplicate);
        assert_eq!(applied.run_id.as_deref(), Some(run_id));
        let duplicate = writer
            .admit_hmac_envelope(&artifact_store, &envelope, Some(&admission))
            .expect("dedupe trigger run");
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.trigger_event_id, envelope.event.id);
        assert_eq!(duplicate.occurrence_id, envelope.occurrence.id);
        assert_eq!(duplicate.run_id.as_deref(), Some(run_id));

        for table in [
            "workflow_trigger_events",
            "workflow_trigger_raw_artifacts",
            "workflow_occurrences",
            "workflow_trigger_admissions",
            "workflow_runs",
            "workflow_commands",
            "workflow_journal",
            "workflow_queue",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count admitted rows");
            assert_eq!(count, 1, "{table} must contain one admitted row");
        }

        let mut rollback_envelope = envelope.clone();
        rollback_envelope.event.id = "wfte-trigger-rollback".into();
        rollback_envelope.event.dedupe_key = "delivery-rollback".into();
        rollback_envelope.event.raw_artifact_id = Some("artifact-rollback".into());
        rollback_envelope.occurrence.id = "wfo-trigger-rollback".into();
        rollback_envelope.occurrence.dedupe_key = "delivery-rollback".into();
        rollback_envelope.occurrence.trigger_event_id = Some(rollback_envelope.event.id.clone());
        rollback_envelope.raw_artifact.id = "artifact-rollback".into();
        writer
            .admit_hmac_envelope(&artifact_store, &rollback_envelope, Some(&admission))
            .expect_err("run binding failure must roll back event and occurrence");
        let rolled_back: i64 = conn
            .query_row(
                "SELECT (SELECT count(*) FROM workflow_trigger_events WHERE id=?1) + (SELECT count(*) FROM workflow_occurrences WHERE id=?2) + (SELECT count(*) FROM workflow_trigger_raw_artifacts WHERE id=?3)",
                params![rollback_envelope.event.id, rollback_envelope.occurrence.id, rollback_envelope.raw_artifact.id],
                |row| row.get(0),
            )
            .expect("rollback counts");
        assert_eq!(rolled_back, 0);

        let mut unavailable_envelope = rollback_envelope;
        unavailable_envelope.event.id = "wfte-trigger-unavailable".into();
        unavailable_envelope.event.dedupe_key = "delivery-unavailable".into();
        unavailable_envelope.event.raw_artifact_id = Some("artifact-unavailable".into());
        unavailable_envelope.occurrence.id = "wfo-trigger-unavailable".into();
        unavailable_envelope.occurrence.dedupe_key = "delivery-unavailable".into();
        unavailable_envelope.occurrence.trigger_event_id =
            Some(unavailable_envelope.event.id.clone());
        unavailable_envelope.raw_artifact.id = "artifact-unavailable".into();
        writer
            .admit_hmac_envelope(&FailingTriggerArtifactStore, &unavailable_envelope, None)
            .expect_err("unavailable artifact store must fail closed");
        let unavailable_rows: i64 = conn
            .query_row(
                "SELECT count(*) FROM workflow_trigger_events WHERE id=?1",
                params![unavailable_envelope.event.id],
                |row| row.get(0),
            )
            .expect("unavailable artifact event count");
        assert_eq!(unavailable_rows, 0);
    }

    #[test]
    fn wait_effect_and_attention_runtime_apis_are_fenced() {
        let conn = open_memory();
        seed_run(&conn, "wfr-runtime");
        conn.execute("INSERT INTO workflow_waits(id,run_id,node_id,kind,state,correlation_key,topic,wake_at_unix_ms,matched_count,max_matches,created_at,updated_at) VALUES ('wfw-due','wfr-runtime','timer','timer','open','timer',NULL,10,0,1,'now','now')", []).expect("due wait");
        conn.execute("INSERT INTO workflow_waits(id,run_id,node_id,kind,state,correlation_key,topic,deadline_unix_ms,matched_count,max_matches,created_at,updated_at) VALUES ('wfw-event','wfr-runtime','event','event','open','correlation','topic',100,0,1,'now','now')", []).expect("event wait");
        conn.execute("INSERT INTO workflow_effects(id,run_id,node_id,adapter,operation,destination_hash,payload_hash,idempotency_key,state,attempt_count,max_attempts,requested_at_unix_ms,ttl_unix_ms,created_at,updated_at) VALUES ('wfe-runtime','wfr-runtime','effect','draft','create','sha256:destination','sha256:payload','effect-key','planned',0,2,1,100,'now','now')", []).expect("effect");
        for (id, node_id, effect_id, approval_id) in [
            ("attn-node", Some("node"), None, None),
            ("attn-effect", None, Some("wfe-runtime"), None),
            ("attn-approval", None, None, Some("wfp-runtime")),
        ] {
            conn.execute("INSERT INTO workflow_attention(id,run_id,node_id,effect_id,approval_id,kind,state,severity,title,summary,deep_link_json,dedupe_key,created_at,updated_at) VALUES (?1,'wfr-runtime',?2,?3,?4,'review','open','warning','title','summary','{}',?1,'now','now')", params![id,node_id,effect_id,approval_id]).expect("attention");
        }
        let repo = WorkflowRepository::new(&conn);
        assert_eq!(repo.list_due_waits(10, 10).expect("due").len(), 1);
        assert_eq!(
            repo.list_open_event_waits("topic", "correlation", 10)
                .expect("event waits")
                .len(),
            1
        );
        assert!(repo
            .resolve_wait("wfw-due", "open", "matched", "later")
            .expect("resolve wait"));
        assert_eq!(repo.list_pending_effects(10).expect("effects").len(), 1);
        let claim = WorkflowEffectClaim {
            id: "wfe-runtime",
            expected_state: "planned",
            holder: "worker",
            fence: 1,
            idempotency_key: "effect-key",
            now_unix_ms: 1,
            expires_at_unix_ms: 50,
            updated_at: "later",
        };
        assert!(repo.claim_effect(&claim).expect("claim"));
        assert!(repo.claim_effect(&claim).expect("idempotent claim"));
        assert!(repo
            .record_effect_outcome(&WorkflowEffectOutcome {
                id: "wfe-runtime",
                idempotency_key: "effect-key",
                holder: "worker",
                fence: 1,
                state: "confirmed",
                receipt_json: Some("{}"),
                outcome_json: Some("{}"),
                reconciliation_json: None,
                failure_json: None,
                updated_at: "done",
            })
            .expect("effect outcome"));
        assert_eq!(
            repo.resolve_attention_by_node("wfr-runtime", "node", "done")
                .expect("node attention"),
            1
        );
        assert_eq!(
            repo.resolve_attention_by_effect("wfe-runtime", "done")
                .expect("effect attention"),
            1
        );
        assert_eq!(
            repo.resolve_attention_by_approval("wfp-runtime", "done")
                .expect("approval attention"),
            1
        );
    }

    #[test]
    fn outbox_dead_letter_is_fenced_and_visible_for_review() {
        let conn = open_memory();
        seed_run(&conn, "wfr-outbox");
        conn.execute("INSERT INTO workflow_outbox(id,run_id,kind,topic,dedupe_key,payload_json,state,available_at,attempt_count,max_attempts,created_at,updated_at) VALUES ('outbox-1','wfr-outbox','failure','internal.review','delivery-1','{}','pending','now',0,2,'now','now')", []).unwrap();
        let repo = WorkflowRepository::new(&conn);
        assert_eq!(repo.list_due_outbox("now", 1).unwrap().len(), 1);
        assert!(repo
            .claim_outbox("outbox-1", "worker", 1, "now", "later")
            .unwrap());
        let attention = WorkflowAttentionRow {
            id: "attention-outbox-1".into(),
            run_id: Some("wfr-outbox".into()),
            node_id: None,
            effect_id: None,
            approval_id: None,
            kind: "dead_letter".into(),
            state: "open".into(),
            severity: "error".into(),
            title: "Delivery failed".into(),
            summary: "Unknown destination".into(),
            deep_link_json: "{}".into(),
            dedupe_key: "outbox:outbox-1".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        assert!(repo
            .dead_letter_outbox("outbox-1", "worker", 1, "{}", "now", &attention)
            .unwrap());
        assert_eq!(repo.list_open_attention(10).unwrap(), vec![attention]);
        assert!(!repo.ack_outbox("outbox-1", "worker", 1, "later").unwrap());
    }

    #[test]
    fn command_transaction_rolls_back_on_outbox_conflict() {
        let conn = open_memory();
        let run_id = "wfr-command-rollback";
        seed_run(&conn, run_id);
        let writer = WorkflowJournalWriter::new(&conn);
        conn.execute(
            "INSERT INTO workflow_outbox(id,run_id,kind,topic,dedupe_key,payload_json,state,available_at,attempt_count,max_attempts,created_at,updated_at) VALUES ('existing',?1,'event','workflow','duplicate','{}','pending','2026-08-07T00:00:00Z',0,1,'2026-08-07T00:00:00Z','2026-08-07T00:00:00Z')",
            params![run_id],
        ).expect("existing outbox");
        let outbox = WorkflowOutboxRow {
            id: "new".into(),
            run_id: run_id.into(),
            kind: "event".into(),
            topic: "workflow".into(),
            dedupe_key: "duplicate".into(),
            payload_json: "{}".into(),
            state: "pending".into(),
            available_at: "2026-08-07T00:00:00Z".into(),
            attempt_count: 0,
            max_attempts: 1,
            last_error_json: None,
            next_attempt_at: None,
            created_at: "2026-08-07T00:00:00Z".into(),
            updated_at: "2026-08-07T00:00:00Z".into(),
        };
        let err = writer
            .apply_command(
                &WorkflowCommandRow {
                    run_id: run_id.into(),
                    command_id: "cmd-rb".into(),
                    expected_revision: 0,
                    command_kind: "start".into(),
                    payload_json: "{}".into(),
                    created_at: "2026-08-07T00:00:01Z".into(),
                },
                r#"{"status":"applied"}"#,
                &JournalAppend {
                    run_id,
                    event_kind: "run_started",
                    node_id: None,
                    payload_json: "{}",
                    created_at: "2026-08-07T00:00:01Z",
                },
                &ProjectionUpdate {
                    run_id,
                    run_phase: Some("running"),
                    ..Default::default()
                },
                None,
                None,
                None,
                None,
                None,
                Some(&outbox),
            )
            .expect_err("duplicate outbox must abort complete command transaction");
        assert!(matches!(err, StorageError::Sqlite(_)));
        let repo = WorkflowRepository::new(&conn);
        assert!(repo.replay_journal(run_id).expect("journal").is_empty());
        assert!(repo
            .record_command(&WorkflowCommandRow {
                run_id: run_id.into(),
                command_id: "cmd-rb".into(),
                expected_revision: 0,
                command_kind: "start".into(),
                payload_json: "{}".into(),
                created_at: "later".into()
            })
            .expect("record after rollback"));
    }

    #[test]
    fn command_transaction_faults_rollback_every_boundary() {
        for boundary in [
            WorkflowCommandBoundary::Command,
            WorkflowCommandBoundary::JournalProjection,
            WorkflowCommandBoundary::Queue,
            WorkflowCommandBoundary::Attention,
            WorkflowCommandBoundary::Outbox,
        ] {
            let conn = open_memory();
            let run_id = format!("wfr-fault-{boundary:?}");
            seed_run(&conn, &run_id);
            let writer = WorkflowJournalWriter::new(&conn);
            let err = writer
                .apply_command_with_fault(
                    &WorkflowCommandRow {
                        run_id: run_id.clone(),
                        command_id: "cmd".into(),
                        expected_revision: 0,
                        command_kind: "start".into(),
                        payload_json: "{}".into(),
                        created_at: "2026-08-07T00:00:01Z".into(),
                    },
                    "{}",
                    &JournalAppend {
                        run_id: &run_id,
                        event_kind: "run_started",
                        node_id: None,
                        payload_json: "{}",
                        created_at: "2026-08-07T00:00:01Z",
                    },
                    &ProjectionUpdate {
                        run_id: &run_id,
                        run_phase: Some("running"),
                        ..Default::default()
                    },
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    boundary,
                )
                .expect_err("injected boundary must abort");
            assert!(matches!(err, StorageError::Serde(_)));
            let repo = WorkflowRepository::new(&conn);
            assert!(repo.replay_journal(&run_id).expect("journal").is_empty());
            assert_eq!(
                repo.find_run(&run_id)
                    .expect("run")
                    .expect("present")
                    .lifecycle_revision,
                0
            );
        }
    }

    #[test]
    fn node_state_projection_upserts_with_lease() {
        let conn = open_memory();
        let run_id = "wfr-test-2";
        seed_run(&conn, run_id);
        let writer = WorkflowJournalWriter::new(&conn);

        // First event: claim node for execution with a lease.
        let lease = WorkflowRunLeaseRow {
            run_id: run_id.to_string(),
            lease_holder: "supervisor-a".to_string(),
            lease_expires_at: "2026-08-07T00:01:00Z".to_string(),
            lease_fence: 1,
            updated_at: "2026-08-07T00:00:01Z".to_string(),
        };
        let node_lease = LeaseUpdate {
            holder: "node-worker",
            expires_at: "2026-08-07T00:00:30Z",
            cleared: false,
        };
        let projection = ProjectionUpdate {
            run_id,
            run_phase: Some("running"),
            node: Some(("node-1", "leased")),
            node_attempt: Some(1),
            node_lease: Some(node_lease),
            run_lease: Some(&lease),
            ..Default::default()
        };
        writer
            .append_with_projection(
                &JournalAppend {
                    run_id,
                    event_kind: "node_claimed",
                    node_id: Some("node-1"),
                    payload_json: "{}",
                    created_at: "2026-08-07T00:00:01Z",
                },
                &projection,
            )
            .expect("append");

        let repo = WorkflowRepository::new(&conn);
        let ns = repo
            .find_node_state(run_id, "node-1")
            .expect("query")
            .expect("present");
        assert_eq!(ns.phase, "leased");
        assert_eq!(ns.attempt, 1);
        assert_eq!(ns.lease_holder.as_deref(), Some("node-worker"));

        // Second event: complete node, clear lease.
        let clear_lease = LeaseUpdate {
            holder: "",
            expires_at: "",
            cleared: true,
        };
        let projection2 = ProjectionUpdate {
            run_id,
            run_phase: Some("running"),
            node: Some(("node-1", "completed")),
            node_attempt: None,
            node_lease: Some(clear_lease),
            ..Default::default()
        };
        writer
            .append_with_projection(
                &JournalAppend {
                    run_id,
                    event_kind: "node_completed",
                    node_id: Some("node-1"),
                    payload_json: r#"{"result":"ok"}"#,
                    created_at: "2026-08-07T00:00:02Z",
                },
                &projection2,
            )
            .expect("append");
        let ns2 = repo
            .find_node_state(run_id, "node-1")
            .expect("query")
            .expect("present");
        assert_eq!(ns2.phase, "completed");
        assert_eq!(
            ns2.attempt, 1,
            "attempt remains at 1 after the cleared write"
        );
        assert!(ns2.lease_holder.is_none());
    }

    #[test]
    fn rejects_append_for_unknown_run() {
        let conn = open_memory();
        let writer = WorkflowJournalWriter::new(&conn);
        let projection = ProjectionUpdate {
            run_id: "wfr-ghost",
            run_phase: Some("running"),
            ..Default::default()
        };
        let event = JournalAppend {
            run_id: "wfr-ghost",
            event_kind: "run_started",
            node_id: None,
            payload_json: "{}",
            created_at: "2026-08-07T00:00:00Z",
        };
        let err = writer
            .append_with_projection(&event, &projection)
            .expect_err("must reject");
        let msg = format!("{}", err);
        assert!(
            msg.contains("refusing to append"),
            "got unexpected error: {}",
            msg
        );
    }

    #[test]
    fn list_runs_by_phase_filters_and_orders() {
        let conn = open_memory();
        seed_run(&conn, "wfr-r1");
        seed_run(&conn, "wfr-r2");
        seed_run(&conn, "wfr-r3");
        // Move one to running via the writer.
        let writer = WorkflowJournalWriter::new(&conn);
        writer
            .append_with_projection(
                &JournalAppend {
                    run_id: "wfr-r2",
                    event_kind: "run_started",
                    node_id: None,
                    payload_json: "{}",
                    created_at: "2026-08-07T00:00:01Z",
                },
                &ProjectionUpdate {
                    run_id: "wfr-r2",
                    run_phase: Some("running"),
                    ..Default::default()
                },
            )
            .expect("append");
        let repo = WorkflowRepository::new(&conn);
        let queued = repo.list_runs_by_phase("queued", 100).expect("list");
        assert_eq!(queued.len(), 2);
        let running = repo.list_runs_by_phase("running", 100).expect("list");
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, "wfr-r2");
    }

    #[test]
    fn attempt_heartbeat_and_completion_require_current_generation() {
        let conn = open_memory();
        let repo = WorkflowRepository::new(&conn);
        seed_run(&conn, "wfr-fence");
        repo.enqueue_node(&WorkflowQueueRow {
            run_id: "wfr-fence".into(),
            node_id: "node-a".into(),
            available_at: "2026-08-07T00:00:00Z".into(),
            priority: 1,
            claimed_by: None,
            claim_fence: None,
        })
        .expect("queue");
        repo.claim_next_node("2026-08-07T00:00:01Z", "worker-a", 7)
            .expect("claim")
            .expect("available");
        repo.insert_attempt(&WorkflowAttemptRow {
            id: "wfa-fence".into(),
            run_id: "wfr-fence".into(),
            node_id: "node-a".into(),
            attempt: 1,
            lease_fence: 7,
            state: "running".into(),
            started_at: "2026-08-07T00:00:01Z".into(),
            finished_at: None,
        })
        .expect("attempt");
        assert!(!repo
            .heartbeat_attempt(&WorkflowAttemptUpdate {
                id: "wfa-fence",
                generation: 6,
                holder: "worker-a",
                at: "2026-08-07T00:00:02Z",
            })
            .expect("stale heartbeat"));
        assert!(repo
            .heartbeat_attempt(&WorkflowAttemptUpdate {
                id: "wfa-fence",
                generation: 7,
                holder: "worker-a",
                at: "2026-08-07T00:00:02Z",
            })
            .expect("current heartbeat"));
        assert!(repo
            .complete_attempt(
                &WorkflowAttemptUpdate {
                    id: "wfa-fence",
                    generation: 7,
                    holder: "worker-a",
                    at: "2026-08-07T00:00:03Z",
                },
                "succeeded",
                Some("{}"),
                None
            )
            .expect("complete"));
        assert!(!repo
            .complete_attempt(
                &WorkflowAttemptUpdate {
                    id: "wfa-fence",
                    generation: 7,
                    holder: "worker-a",
                    at: "2026-08-07T00:00:04Z",
                },
                "succeeded",
                Some("{}"),
                None
            )
            .expect("duplicate completion"));
        assert!(repo
            .find_queue_node("wfr-fence", "node-a")
            .expect("queue row")
            .expect("present")
            .claimed_by
            .is_none());
    }

    // ── WP-06 / WP-07 focused suites ────────────────────────────────────

    /// The trigger-event idempotency invariant: a redelivery of an
    /// already-stored `(source, dedupe_key)` pair must return the existing
    /// row, not a duplicate. The first call inserts; the second and third
    /// collapse to a `Duplicate` with the persisted row returned verbatim.
    #[test]
    fn trigger_event_idempotent_insert_collapses_redelivery() {
        let conn = open_memory();
        let writer = WorkflowTriggerWriter::new(&conn);
        let repo = WorkflowRepository::new(&conn);

        let event = WorkflowTriggerEventRow {
            id: "wfte_first".to_string(),
            trigger_id: Some("wft_t".to_string()),
            source: "github".to_string(),
            event_type: "push".to_string(),
            external_id: Some("evt-42".to_string()),
            dedupe_key: "github:evt-42".to_string(),
            content_hash: "sha256:1111".to_string(),
            trust_json: "{}".to_string(),
            data_json: r#"{"ref":"refs/heads/main"}"#.to_string(),
            raw_artifact_id: None,
            occurred_at: "2026-08-07T00:00:00Z".to_string(),
            received_at: "2026-08-07T00:00:01Z".to_string(),
        };

        match writer
            .insert_trigger_event_idempotent(&event)
            .expect("first insert")
        {
            TriggerEventInsert::Inserted(returned) => assert_eq!(returned.id, "wfte_first"),
            TriggerEventInsert::Duplicate(_) => panic!("first insert must be Inserted"),
        }

        // Second call — same dedupe key, different row id. Storage must
        // return the existing row and not insert a duplicate.
        let redelivery = WorkflowTriggerEventRow {
            id: "wfte_redelivery".to_string(),
            ..event.clone()
        };
        match writer
            .insert_trigger_event_idempotent(&redelivery)
            .expect("dedupe")
        {
            TriggerEventInsert::Duplicate(existing) => {
                assert_eq!(
                    existing.id, "wfte_first",
                    "duplicate must surface the originally stored id, not the redelivery id"
                );
                assert_eq!(existing.source, "github");
                assert_eq!(existing.dedupe_key, "github:evt-42");
            }
            TriggerEventInsert::Inserted(_) => {
                panic!("redelivery must collapse to Duplicate")
            }
        }

        // Repository lookup confirms exactly one row exists.
        let looked_up = repo
            .find_trigger_event_by_dedupe("github", "github:evt-42")
            .expect("lookup")
            .expect("present");
        assert_eq!(looked_up.id, "wfte_first");
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM workflow_trigger_events WHERE source = ?1",
                ["github"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "redelivery must not insert a second row");
    }

    /// Occurrence admission is keyed on `(trigger_id, dedupe_key)`. The
    /// first admission returns `Admitted`; the second admission of the same
    /// dedupe key fails with `StorageError::Constraint` (the storage layer
    /// surfaces a typed race rather than silently collapsing).
    #[test]
    fn occurrence_admission_is_idempotent_and_typed() {
        let conn = open_memory();
        let writer = WorkflowTriggerWriter::new(&conn);
        let repo = WorkflowRepository::new(&conn);

        let row = WorkflowOccurrenceRow {
            id: "wfo_1".to_string(),
            trigger_id: "wft_t".to_string(),
            trigger_event_id: Some("wfte_first".to_string()),
            nominal_at_unix_ms: 1_700_000_000_000,
            decision: "admitted".to_string(),
            decision_reason: "first_fire".to_string(),
            selected_version_id: Some("wfv_v1".to_string()),
            concurrency_key: Some("wf.concurrency.main".to_string()),
            dedupe_key: "wft_t:1_700_000_000_000".to_string(),
            created_at: "2026-08-07T00:00:02Z".to_string(),
        };

        match writer.admit_occurrence(&row).expect("first admit") {
            OccurrenceAdmission::Admitted(returned) => assert_eq!(returned.id, "wfo_1"),
            other => panic!("expected Admitted, got {other:?}"),
        }

        // Re-admitting the same dedupe key surfaces a constraint error so
        // the supervisor can translate it into a typed "already admitted"
        // decision rather than misinterpreting silent success.
        let err = writer
            .admit_occurrence(&WorkflowOccurrenceRow {
                id: "wfo_2".to_string(),
                ..row.clone()
            })
            .expect_err("duplicate admission must error");
        assert!(
            matches!(err, StorageError::Sqlite(_)),
            "expected sqlite constraint error, got {err:?}"
        );

        // Repository lookup returns the originally admitted row.
        let looked_up = repo
            .find_occurrence_by_dedupe("wft_t", "wft_t:1_700_000_000_000")
            .expect("lookup")
            .expect("present");
        assert_eq!(looked_up.id, "wfo_1");
        assert_eq!(looked_up.decision, "admitted");
        assert_eq!(looked_up.selected_version_id.as_deref(), Some("wfv_v1"));
    }

    /// The full set of WP-06 / WP-07 decision variants must each round-trip
    /// through `admit_occurrence` as the matching enum variant.
    #[test]
    fn occurrence_admission_typed_for_every_decision_variant() {
        let conn = open_memory();
        let writer = WorkflowTriggerWriter::new(&conn);

        let base = |id: &str, decision: &str| WorkflowOccurrenceRow {
            id: id.to_string(),
            trigger_id: "wft_t".to_string(),
            trigger_event_id: None,
            nominal_at_unix_ms: 1_700_000_000_000,
            decision: decision.to_string(),
            decision_reason: "test".to_string(),
            selected_version_id: None,
            concurrency_key: None,
            dedupe_key: format!("wft_t:{decision}:{id}"),
            created_at: "2026-08-07T00:00:00Z".to_string(),
        };

        for (decision, expected_variant) in [
            ("admitted", "Admitted"),
            ("coalesced", "Coalesced"),
            ("skipped", "Skipped"),
            ("queued", "Queued"),
            ("replaced", "Replaced"),
            ("rejected", "Rejected"),
        ] {
            let outcome = writer
                .admit_occurrence(&base(decision, decision))
                .expect("admit");
            let variant = match outcome {
                OccurrenceAdmission::Admitted(_) => "Admitted",
                OccurrenceAdmission::Coalesced(_) => "Coalesced",
                OccurrenceAdmission::Skipped(_) => "Skipped",
                OccurrenceAdmission::Queued(_) => "Queued",
                OccurrenceAdmission::Replaced(_) => "Replaced",
                OccurrenceAdmission::Rejected(_) => "Rejected",
            };
            assert_eq!(variant, expected_variant, "decision {decision}");
        }

        // An unknown decision string is refused at the storage boundary so
        // a future enum extension doesn't silently land on the wrong path.
        let err = writer
            .admit_occurrence(&base("bogus", "pending"))
            .expect_err("unknown decision must error");
        let msg = format!("{err}");
        assert!(msg.contains("unknown decision"), "got: {msg}");
    }
}
