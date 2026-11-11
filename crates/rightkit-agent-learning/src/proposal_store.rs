//! WP-11 / ADR-0021: durable workflow optimization proposal persistence.
//! FM-091 persistence — proposals are idempotent by `id` (ON CONFLICT DO NOTHING)
//! and status-mapped from `GateOutcome` (proceed/blocked/pending_review).

use crate::proposals::Proposal;
use rightkit_agent_engine_storage::{Result, Storage};
use rightkit_db::rusqlite::params;
use std::sync::Arc;

#[derive(Clone)]
pub struct ProposalStore {
    pub storage: Arc<Storage>,
}

impl ProposalStore {
    pub fn new(storage: Arc<Storage>) -> Self {
        Self { storage }
    }

    pub fn upsert(&self, proposal: &Proposal) -> Result<()> {
        let json = serde_json::to_string(proposal)?;
        let status = match &proposal.gate_outcome {
            crate::GateOutcome::Proceed { .. } => "proceed",
            crate::GateOutcome::Block { .. } => "blocked",
            crate::GateOutcome::HoldForReview { .. } => "pending_review",
        };
        self.storage.conn().execute(
            "INSERT INTO learning_proposals (id, workflow_id, proposed_by, proposal_json, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO NOTHING",
            params![proposal.id, proposal.workflow_id, proposal.proposed_by, json, status, proposal.created_at],
        )?;
        Ok(())
    }

    pub fn list_pending_review(&self) -> Result<Vec<Proposal>> {
        let mut statement = self.storage.conn().prepare(
            "SELECT proposal_json FROM learning_proposals WHERE status = 'pending_review' ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            let json = row?;
            serde_json::from_str(&json).map_err(rightkit_agent_engine_storage::StorageError::from)
        })
        .collect()
    }

    pub fn mark_applied(&self, proposal_id: &str, applied_at: &str) -> Result<()> {
        self.storage.conn().execute(
            "UPDATE learning_proposals SET status='applied', applied_at=?2 WHERE id=?1",
            params![proposal_id, applied_at],
        )?;
        Ok(())
    }

    pub fn mark_rejected(&self, proposal_id: &str, reason: &str) -> Result<()> {
        self.storage.conn().execute(
            "UPDATE learning_proposals SET status='rejected', rejection_reason=?2 WHERE id=?1",
            params![proposal_id, reason],
        )?;
        Ok(())
    }
}
