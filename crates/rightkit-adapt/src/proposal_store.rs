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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval_gate::EvalEvidence;
    use crate::proposals::{ProposalBuilder, ProposalKind};

    fn passing_evidence() -> EvalEvidence {
        EvalEvidence {
            train_pass_rate_pct: 99.0,
            heldout_pass_rate_pct: 99.0,
            safety_pass_rate_pct: 100.0,
            cost_ratio_to_baseline: 1.0,
            latency_ratio_to_baseline: 1.0,
            pass_rate_pct: 99.0,
            regression_count: 0,
            new_untested_capability_requested: false,
            total_runs: 10,
            expected_run_count_min: 10,
        }
    }

    fn failing_evidence() -> EvalEvidence {
        let mut evidence = passing_evidence();
        evidence.train_pass_rate_pct = 10.0;
        evidence
    }

    #[allow(clippy::arc_with_non_send_sync)]
    fn store() -> ProposalStore {
        let storage = Arc::new(Storage::open_in_memory().expect("in-memory storage"));
        ProposalStore::new(storage)
    }

    fn build(workflow_id: &str, evidence: EvalEvidence) -> Proposal {
        ProposalBuilder::new(workflow_id)
            .change_kind(ProposalKind::AddNode {
                node_kind: "checkpoint".into(),
            })
            .diff_summary("add a checkpoint node")
            .evidence(evidence)
            .created_at("2026-01-01T00:00:00Z")
            .base_version("base-1", "hash-1")
            .candidate_payload("{}", "{}")
            .build()
            .expect("valid proposal builds")
    }

    #[test]
    fn upsert_then_list_pending_review_round_trips_proposal() {
        let store = store();
        // total_runs below expected_run_count_min -> HoldForReview -> pending_review status.
        let mut evidence = passing_evidence();
        evidence.total_runs = 1;
        let proposal = build("workflow-1", evidence);
        assert!(matches!(
            proposal.gate_outcome,
            crate::GateOutcome::HoldForReview { .. }
        ));

        store.upsert(&proposal).expect("upsert succeeds");
        let pending = store.list_pending_review().expect("list succeeds");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, proposal.id);
        assert_eq!(pending[0].workflow_id, "workflow-1");
    }

    #[test]
    fn proceeding_proposal_is_not_listed_as_pending_review() {
        let store = store();
        let proposal = build("workflow-2", passing_evidence());
        assert!(matches!(
            proposal.gate_outcome,
            crate::GateOutcome::Proceed { .. }
        ));
        store.upsert(&proposal).expect("upsert succeeds");
        let pending = store.list_pending_review().expect("list succeeds");
        assert!(pending.is_empty());
    }

    #[test]
    fn blocked_proposal_is_not_listed_as_pending_review() {
        let store = store();
        let proposal = build("workflow-3", failing_evidence());
        assert!(matches!(
            proposal.gate_outcome,
            crate::GateOutcome::Block { .. }
        ));
        store.upsert(&proposal).expect("upsert succeeds");
        let pending = store.list_pending_review().expect("list succeeds");
        assert!(pending.is_empty());
    }

    #[test]
    fn upsert_is_idempotent_on_conflicting_id() {
        let store = store();
        let mut evidence = passing_evidence();
        evidence.total_runs = 1;
        let proposal = build("workflow-4", evidence);
        store.upsert(&proposal).expect("first upsert succeeds");
        store.upsert(&proposal).expect("second upsert is a no-op");
        let pending = store.list_pending_review().expect("list succeeds");
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn mark_rejected_removes_proposal_from_pending_review() {
        let store = store();
        let mut evidence = passing_evidence();
        evidence.total_runs = 1;
        let proposal = build("workflow-5", evidence);
        store.upsert(&proposal).expect("upsert succeeds");
        store
            .mark_rejected(&proposal.id, "manual reviewer rejection")
            .expect("mark_rejected succeeds");
        let pending = store.list_pending_review().expect("list succeeds");
        assert!(pending.is_empty());
    }

    #[test]
    fn mark_applied_removes_proposal_from_pending_review() {
        let store = store();
        let mut evidence = passing_evidence();
        evidence.total_runs = 1;
        let proposal = build("workflow-6", evidence);
        store.upsert(&proposal).expect("upsert succeeds");
        store
            .mark_applied(&proposal.id, "2026-01-02T00:00:00Z")
            .expect("mark_applied succeeds");
        let pending = store.list_pending_review().expect("list succeeds");
        assert!(pending.is_empty());
    }
}
