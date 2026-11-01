//! WP-11 / ADR-0021: deterministic evaluation gate for optimization proposals.
//! FM-091 executed bakeoff gate — promotion requires held-out evidence; this
//! gate is the production check called from `skill_opt::execute_epoch` and
//! from workflow `ProposalBuilder::build`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EvalEvidence {
    /// Training cohort correctness rate; never substitutes for heldout data.
    pub train_pass_rate_pct: f64,
    /// Heldout cohort correctness rate used for promotion.
    pub heldout_pass_rate_pct: f64,
    /// Safety checks passing across the heldout cohort.
    pub safety_pass_rate_pct: f64,
    /// Candidate cost relative to baseline (1.0 means unchanged).
    pub cost_ratio_to_baseline: f64,
    /// Candidate latency relative to baseline (1.0 means unchanged).
    pub latency_ratio_to_baseline: f64,
    pub pass_rate_pct: f64,
    pub regression_count: u32,
    pub new_untested_capability_requested: bool,
    pub total_runs: u32,
    pub expected_run_count_min: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateOutcome {
    Proceed { reason: String },
    Block { reason: String },
    HoldForReview { reason: String },
}

#[derive(Debug, Clone, Copy)]
pub struct EvalGate {
    minimum_pass_rate_pct: f64,
}

impl Default for EvalGate {
    fn default() -> Self {
        Self::new()
    }
}

impl EvalGate {
    pub fn new() -> Self {
        Self {
            minimum_pass_rate_pct: 95.0,
        }
    }

    pub fn evaluate(&self, evidence: &EvalEvidence) -> GateOutcome {
        if !evidence.train_pass_rate_pct.is_finite()
            || !evidence.heldout_pass_rate_pct.is_finite()
            || evidence.train_pass_rate_pct < self.minimum_pass_rate_pct
            || evidence.heldout_pass_rate_pct < self.minimum_pass_rate_pct
        {
            return GateOutcome::Block {
                reason: "training or heldout correctness is below 95% or is not finite".to_string(),
            };
        }
        if !evidence.safety_pass_rate_pct.is_finite() || evidence.safety_pass_rate_pct < 100.0 {
            return GateOutcome::Block {
                reason: "heldout safety evidence is incomplete".to_string(),
            };
        }
        if !evidence.cost_ratio_to_baseline.is_finite()
            || !evidence.latency_ratio_to_baseline.is_finite()
            || evidence.cost_ratio_to_baseline > 1.10
            || evidence.latency_ratio_to_baseline > 1.10
        {
            return GateOutcome::HoldForReview {
                reason: "cost or latency exceeds the 10% review threshold".to_string(),
            };
        }
        if !evidence.pass_rate_pct.is_finite()
            || evidence.pass_rate_pct < self.minimum_pass_rate_pct
        {
            return GateOutcome::Block {
                reason: "pass rate is below the 95% safety threshold or is not finite".to_string(),
            };
        }
        if evidence.regression_count > 0 {
            return GateOutcome::Block {
                reason: "evaluation detected one or more regressions".to_string(),
            };
        }
        if evidence.new_untested_capability_requested {
            return GateOutcome::Block {
                reason: "proposal requests an untested capability".to_string(),
            };
        }
        if evidence.total_runs < evidence.expected_run_count_min {
            return GateOutcome::HoldForReview {
                reason: "insufficient evaluation runs for a conclusive decision".to_string(),
            };
        }
        GateOutcome::Proceed {
            reason: "evaluation meets the pass-rate, regression, capability, and sample-size gates"
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_evidence() -> EvalEvidence {
        EvalEvidence {
            train_pass_rate_pct: 95.0,
            heldout_pass_rate_pct: 95.0,
            safety_pass_rate_pct: 100.0,
            cost_ratio_to_baseline: 1.0,
            latency_ratio_to_baseline: 1.0,
            pass_rate_pct: 95.0,
            regression_count: 0,
            new_untested_capability_requested: false,
            total_runs: 10,
            expected_run_count_min: 10,
        }
    }

    #[test]
    fn all_thresholds_met_proceeds() {
        let gate = EvalGate::new();
        assert!(matches!(
            gate.evaluate(&passing_evidence()),
            GateOutcome::Proceed { .. }
        ));
    }

    #[test]
    fn train_pass_rate_just_below_threshold_blocks() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.train_pass_rate_pct = 94.999;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::Block { .. }
        ));
    }

    #[test]
    fn train_pass_rate_at_exact_threshold_passes_that_check() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.train_pass_rate_pct = 95.0;
        evidence.heldout_pass_rate_pct = 95.0;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::Proceed { .. }
        ));
    }

    #[test]
    fn non_finite_train_pass_rate_blocks() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.train_pass_rate_pct = f64::NAN;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::Block { .. }
        ));
    }

    #[test]
    fn non_finite_heldout_pass_rate_blocks() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.heldout_pass_rate_pct = f64::INFINITY;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::Block { .. }
        ));
    }

    #[test]
    fn safety_below_100_percent_blocks_even_with_good_pass_rates() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.safety_pass_rate_pct = 99.999;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::Block { .. }
        ));
    }

    #[test]
    fn cost_ratio_over_threshold_holds_for_review_not_block() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.cost_ratio_to_baseline = 1.11;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::HoldForReview { .. }
        ));
    }

    #[test]
    fn cost_ratio_at_exact_threshold_proceeds() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.cost_ratio_to_baseline = 1.10;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::Proceed { .. }
        ));
    }

    #[test]
    fn latency_ratio_over_threshold_holds_for_review() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.latency_ratio_to_baseline = 1.5;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::HoldForReview { .. }
        ));
    }

    #[test]
    fn regression_count_nonzero_blocks_even_after_cost_latency_pass() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.regression_count = 1;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::Block { .. }
        ));
    }

    #[test]
    fn untested_capability_requested_blocks() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.new_untested_capability_requested = true;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::Block { .. }
        ));
    }

    #[test]
    fn insufficient_run_count_holds_for_review() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.total_runs = 5;
        evidence.expected_run_count_min = 10;
        assert!(matches!(
            gate.evaluate(&evidence),
            GateOutcome::HoldForReview { .. }
        ));
    }

    #[test]
    fn overall_pass_rate_below_threshold_blocks_before_regression_check() {
        let gate = EvalGate::new();
        let mut evidence = passing_evidence();
        evidence.pass_rate_pct = 90.0;
        evidence.regression_count = 3;
        let outcome = gate.evaluate(&evidence);
        assert!(matches!(outcome, GateOutcome::Block { .. }));
    }
}
