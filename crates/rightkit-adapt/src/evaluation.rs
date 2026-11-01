//! Scoring of candidate proposals.
//! FM-091: scoring is advisory only — held-out validation via `EvalGate` plus
//! executed bakeoff thresholds in `skill_opt` own promotion. This engine
//! enforces that proposals without held-out evidence stay below the promote
//! threshold unless their base kind already clears it (governed flow).

use crate::types::{Proposal, ProposalKind, ProposalStatus};

/// Tunable parameters for the [`EvaluationEngine`].
#[derive(Debug, Clone, Copy)]
pub struct EvaluationConfig {
    /// Minimum score required for promotion.
    pub promote_threshold: f64,
    /// Per-character weight applied to evidence length.
    pub evidence_weight: f64,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        Self {
            promote_threshold: 0.6,
            evidence_weight: 0.01,
        }
    }
}

/// Scores proposals and decides promotion eligibility.
#[derive(Debug, Default, Clone)]
pub struct EvaluationEngine {
    config: EvaluationConfig,
}

impl EvaluationEngine {
    /// Create an engine with the given configuration.
    pub fn new(config: EvaluationConfig) -> Self {
        Self { config }
    }

    /// Access the configuration.
    pub fn config(&self) -> &EvaluationConfig {
        &self.config
    }

    fn base_score(kind: ProposalKind) -> f64 {
        match kind {
            ProposalKind::Memory => 0.5,
            ProposalKind::HarnessRule => 0.65,
            ProposalKind::Workflow => 0.55,
            ProposalKind::SkillTweak => 0.45,
            // Synthesized skills are advisory: a bare one stays a candidate
            // (below the 0.6 promote threshold) for human review, as skills are
            // user-owned. Strong evidence can still lift it.
            ProposalKind::Skill => 0.55,
        }
    }

    /// Score a proposal in place and mark it `Evaluated`. The evidence boost is
    /// drawn ONLY from `held_out_evidence` — evidence NOT used to mint the
    /// proposal — so a proposal cannot inflate its own score on the same data it
    /// was extracted from (train-on-test leakage). `None` ⇒ base score only,
    /// which keeps un-validated proposals conservatively below the promote
    /// threshold unless their base kind already clears it.
    pub fn evaluate(&self, proposal: &mut Proposal, held_out_evidence: Option<&str>) {
        let base = Self::base_score(proposal.kind);
        let evidence_factor = held_out_evidence
            .map(|h| (h.len() as f64 * self.config.evidence_weight).min(0.3))
            .unwrap_or(0.0);
        let score = (base + evidence_factor).clamp(0.0, 1.0);
        proposal.score = score;
        proposal.status = ProposalStatus::Evaluated;
    }

    /// Whether an evaluated proposal meets the promotion threshold.
    pub fn should_promote(&self, proposal: &Proposal) -> bool {
        proposal.score >= self.config.promote_threshold
            && proposal.status == ProposalStatus::Evaluated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(kind: ProposalKind, evidence: &str) -> Proposal {
        Proposal {
            id: "p".to_string(),
            kind,
            summary: "s".to_string(),
            evidence: evidence.to_string(),
            score: 0.0,
            status: ProposalStatus::Candidate,
            created_at: String::new(),
            age_sessions: 0,
            rejection_reason: None,
        }
    }

    #[test]
    fn evaluate_sets_score_and_status() {
        let engine = EvaluationEngine::default();
        let mut p = proposal(ProposalKind::Memory, "");
        engine.evaluate(&mut p, None);
        assert_eq!(p.status, ProposalStatus::Evaluated);
        assert!((p.score - 0.5).abs() < 1e-9);
    }

    #[test]
    fn evaluate_harness_scores_highest_base() {
        let engine = EvaluationEngine::default();
        let mut harness = proposal(ProposalKind::HarnessRule, "");
        let mut memory = proposal(ProposalKind::Memory, "");
        let mut workflow = proposal(ProposalKind::Workflow, "");
        let mut skill = proposal(ProposalKind::SkillTweak, "");
        engine.evaluate(&mut harness, None);
        engine.evaluate(&mut memory, None);
        engine.evaluate(&mut workflow, None);
        engine.evaluate(&mut skill, None);
        assert!(harness.score > memory.score);
        assert!(harness.score > workflow.score);
        assert!(harness.score > skill.score);
    }

    #[test]
    fn evaluate_evidence_boosts_score() {
        let engine = EvaluationEngine::default();
        let mut bare = proposal(ProposalKind::Memory, "");
        let mut rich = proposal(ProposalKind::Memory, "");
        engine.evaluate(&mut bare, None);
        engine.evaluate(&mut rich, Some("aaaaaaaaaa")); // 10 chars of held-out evidence
        assert!(rich.score > bare.score);
    }

    #[test]
    fn evaluate_clamps_to_one() {
        let engine = EvaluationEngine::default();
        let long = "x".repeat(10_000);
        let mut p = proposal(ProposalKind::HarnessRule, "");
        engine.evaluate(&mut p, Some(&long));
        assert!(p.score <= 1.0);
        // base 0.65 + capped evidence factor 0.3 = 0.95
        assert!((p.score - 0.95).abs() < 1e-9);
    }

    #[test]
    fn evaluate_ignores_self_evidence_without_holdout() {
        let engine = EvaluationEngine::default();
        let mut bare = proposal(ProposalKind::Memory, "");
        let mut rich_self = proposal(ProposalKind::Memory, &"x".repeat(10_000)); // huge SELF evidence
        engine.evaluate(&mut bare, None);
        engine.evaluate(&mut rich_self, None); // no held-out ⇒ self-evidence must NOT boost
        assert!(
            (bare.score - rich_self.score).abs() < 1e-9,
            "self-evidence must not inflate score without held-out validation"
        );
    }

    #[test]
    fn should_promote_above_threshold() {
        let engine = EvaluationEngine::default();
        let mut p = proposal(ProposalKind::HarnessRule, "");
        engine.evaluate(&mut p, None);
        assert!(engine.should_promote(&p));
    }

    #[test]
    fn should_promote_false_below() {
        let engine = EvaluationEngine::default();
        let mut p = proposal(ProposalKind::SkillTweak, ""); // base 0.45 < 0.6
        engine.evaluate(&mut p, None);
        assert!(!engine.should_promote(&p));
    }

    #[test]
    fn should_promote_requires_evaluated() {
        let engine = EvaluationEngine::default();
        let mut p = proposal(ProposalKind::HarnessRule, "");
        // High score but not evaluated.
        p.score = 0.9;
        assert!(!engine.should_promote(&p));
    }
}
