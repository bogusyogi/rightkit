// rightkit-agent-learning 0.1.0 (published as rightkit-adapt until 0.1.2) consumed by exact registry
// pin: the evaluation gate and the proposal store over an in-memory agent database. No files are
// written.
use rightkit_agent_engine_storage::Storage;
use rightkit_agent_learning::{EvalEvidence, EvalGate, GateOutcome, ProposalStore};
use std::sync::Arc;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let weak = EvalEvidence {
        train_pass_rate_pct: 10.0,
        heldout_pass_rate_pct: 10.0,
        safety_pass_rate_pct: 10.0,
        cost_ratio_to_baseline: 5.0,
        latency_ratio_to_baseline: 5.0,
        pass_rate_pct: 10.0,
        regression_count: 3,
        new_untested_capability_requested: false,
        total_runs: 1,
        expected_run_count_min: 10,
    };
    let outcome = EvalGate::new().evaluate(&weak);
    check(
        !matches!(outcome, GateOutcome::Proceed { .. }),
        "weak evidence does not proceed",
    );
    let storage = Arc::new(Storage::open_in_memory().expect("in-memory storage"));
    let store = ProposalStore::new(storage);
    check(
        store
            .list_pending_review()
            .map(|p| p.is_empty())
            .unwrap_or(false),
        "no pending proposals at start",
    );
}
