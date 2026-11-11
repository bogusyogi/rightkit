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
