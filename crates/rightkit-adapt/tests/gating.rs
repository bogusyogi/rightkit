//! WP-11 / ADR-0021 gate integration tests.

use rightkit_adapt::{EvalEvidence, EvalGate, GateOutcome};

fn evidence() -> EvalEvidence {
    EvalEvidence {
        train_pass_rate_pct: 99.0,
        heldout_pass_rate_pct: 99.0,
        safety_pass_rate_pct: 100.0,
        cost_ratio_to_baseline: 1.0,
        latency_ratio_to_baseline: 1.0,
        pass_rate_pct: 98.0,
        regression_count: 0,
        new_untested_capability_requested: false,
        total_runs: 30,
        expected_run_count_min: 30,
    }
}

#[test]
fn proceed_requires_complete_clean_evidence() {
    assert!(matches!(
        EvalGate::new().evaluate(&evidence()),
        GateOutcome::Proceed { .. }
    ));
}

#[test]
fn block_covers_low_rate_regression_and_untested_capability() {
    let mut low_rate = evidence();
    low_rate.pass_rate_pct = 94.99;
    assert!(matches!(
        EvalGate::new().evaluate(&low_rate),
        GateOutcome::Block { .. }
    ));

    let mut regression = evidence();
    regression.regression_count = 1;
    assert!(matches!(
        EvalGate::new().evaluate(&regression),
        GateOutcome::Block { .. }
    ));

    let mut capability = evidence();
    capability.new_untested_capability_requested = true;
    assert!(matches!(
        EvalGate::new().evaluate(&capability),
        GateOutcome::Block { .. }
    ));
}

#[test]
fn insufficient_runs_hold_for_review() {
    let mut insufficient = evidence();
    insufficient.total_runs = 29;
    assert!(matches!(
        EvalGate::new().evaluate(&insufficient),
        GateOutcome::HoldForReview { .. }
    ));
}

#[test]
fn nan_is_blocked() {
    let mut invalid = evidence();
    invalid.pass_rate_pct = f64::NAN;
    assert!(matches!(
        EvalGate::new().evaluate(&invalid),
        GateOutcome::Block { .. }
    ));
}
