//! Core data types for the learning pipeline.

use serde::{Deserialize, Serialize};

/// The category of a learning proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalKind {
    /// A durable memory item.
    Memory,
    /// A harness-level rule (e.g. "avoid X").
    HarnessRule,
    /// A reusable multi-step workflow.
    Workflow,
    /// A tweak to an existing skill.
    SkillTweak,
    /// A newly synthesized skill distilled from a successful complex run.
    Skill,
}

/// Lifecycle status of a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    /// Freshly extracted, not yet scored.
    Candidate,
    /// Scored by the evaluator.
    Evaluated,
    /// Accepted and deployed/eligible for deployment.
    Promoted,
    /// Rejected.
    Rejected,
    /// Aged out before promotion.
    Expired,
}

/// A single learning proposal candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Stable identifier, e.g. `prop-0`.
    pub id: String,
    /// Category of the proposal.
    pub kind: ProposalKind,
    /// Short human-readable summary.
    pub summary: String,
    /// Supporting evidence text.
    pub evidence: String,
    /// Score set by the evaluator; 0.0 until evaluated.
    pub score: f64,
    /// Current lifecycle status.
    pub status: ProposalStatus,
    /// Creation timestamp (opaque string).
    pub created_at: String,
    /// How many sessions have elapsed since creation (for forgetting).
    pub age_sessions: u32,
    /// Why this proposal was rejected (None unless status == Rejected). Mirrors
    /// SkillOpt's RejectedEdit reason so rejected approaches aren't silently lost.
    #[serde(default)]
    pub rejection_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_proposal() -> Proposal {
        Proposal {
            id: "prop-0".into(),
            kind: ProposalKind::SkillTweak,
            summary: "tweak retry policy".into(),
            evidence: "3 successful reruns".into(),
            score: 0.87,
            status: ProposalStatus::Evaluated,
            created_at: "2026-01-01T00:00:00Z".into(),
            age_sessions: 2,
            rejection_reason: None,
        }
    }

    #[test]
    fn proposal_serde_round_trip_preserves_all_fields() {
        let proposal = sample_proposal();
        let json = serde_json::to_string(&proposal).expect("serializes");
        let restored: Proposal = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(restored.id, proposal.id);
        assert_eq!(restored.kind, proposal.kind);
        assert_eq!(restored.summary, proposal.summary);
        assert_eq!(restored.evidence, proposal.evidence);
        assert_eq!(restored.score, proposal.score);
        assert_eq!(restored.status, proposal.status);
        assert_eq!(restored.created_at, proposal.created_at);
        assert_eq!(restored.age_sessions, proposal.age_sessions);
        assert_eq!(restored.rejection_reason, proposal.rejection_reason);
    }

    #[test]
    fn rejection_reason_defaults_to_none_when_absent_from_json() {
        // Older records persisted before `rejection_reason` existed must still
        // deserialize; `#[serde(default)]` is the contract under test.
        let json = r#"{
            "id": "prop-1",
            "kind": "Memory",
            "summary": "s",
            "evidence": "e",
            "score": 0.5,
            "status": "Candidate",
            "created_at": "2026-01-01T00:00:00Z",
            "age_sessions": 0
        }"#;
        let restored: Proposal = serde_json::from_str(json).expect("deserializes without field");
        assert_eq!(restored.rejection_reason, None);
    }

    #[test]
    fn rejection_reason_round_trips_when_present() {
        let mut proposal = sample_proposal();
        proposal.status = ProposalStatus::Rejected;
        proposal.rejection_reason = Some("regression detected".into());
        let json = serde_json::to_string(&proposal).expect("serializes");
        let restored: Proposal = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(
            restored.rejection_reason,
            Some("regression detected".to_string())
        );
    }

    #[test]
    fn all_proposal_kind_variants_round_trip() {
        for kind in [
            ProposalKind::Memory,
            ProposalKind::HarnessRule,
            ProposalKind::Workflow,
            ProposalKind::SkillTweak,
            ProposalKind::Skill,
        ] {
            let json = serde_json::to_string(&kind).expect("serializes");
            let restored: ProposalKind = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(restored, kind);
        }
    }

    #[test]
    fn all_proposal_status_variants_round_trip() {
        for status in [
            ProposalStatus::Candidate,
            ProposalStatus::Evaluated,
            ProposalStatus::Promoted,
            ProposalStatus::Rejected,
            ProposalStatus::Expired,
        ] {
            let json = serde_json::to_string(&status).expect("serializes");
            let restored: ProposalStatus = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(restored, status);
        }
    }
}
