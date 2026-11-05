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
        Ok(rightkit_db::write_txn_unchecked(self.conn)?)
    }

    pub fn publish_version(&self, workflow_id: &str, version_id: &str, now: &str) -> Result<bool> {
        let tx = rightkit_db::write_txn_unchecked(self.conn)?;
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
