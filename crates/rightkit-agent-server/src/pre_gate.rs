//! Pre-gate wiring (F3) — runs the workspace's declared deterministic checks
//! before the conductor spins up the expensive jury, emits a `CheckRunCompleted`
//! per check, and returns whether the run may proceed to the jury.
//!
//! The decision/runner logic lives in `rightkit_agent_coding_tools::check_runner`; this
//! module just maps config → `CheckSpec`, owns the conduit emission, and is
//! called from `DaemonState::run_conductor` between planning and `run_graph`.

use rightkit_agent_config::PreGateCheckConfig;
use rightkit_agent_engine_core::Conduit;
use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_agent_coding_tools::check_runner::{
    decide, run_check, CheckSpec, OnFailure, PreGateDecision,
};
use rightkit_agent_coding_tools::ToolContext;

use crate::api::now_iso;

/// Map declarative config checks into runnable `CheckSpec`s, dropping blanks.
pub fn specs_from_config(checks: &[PreGateCheckConfig]) -> Vec<CheckSpec> {
    checks
        .iter()
        .filter(|c| !c.command.trim().is_empty())
        .map(|c| CheckSpec {
            id: if c.id.trim().is_empty() {
                c.command.clone()
            } else {
                c.id.clone()
            },
            command: c.command.clone(),
            timeout_ms: c.timeout_ms,
            max_attempts: c.max_attempts.max(1),
            backoff_ms: c.backoff_ms,
            on_failure: match c.on_failure.as_str() {
                "retry" => OnFailure::Retry,
                "continue" => OnFailure::Continue,
                _ => OnFailure::FailFast,
            },
        })
        .collect()
}

/// Run the pre-gate. Emits `CheckRunCompleted` for every check executed, then
/// returns the decision. On `FailFast` the first failing check short-circuits
/// the remaining checks (the jury is skipped regardless).
pub async fn run_pre_gate(
    conduit: &Conduit,
    session_id: &str,
    ctx: &ToolContext,
    specs: &[CheckSpec],
) -> PreGateDecision {
    let mut outcomes = Vec::new();
    for spec in specs {
        let outcome = run_check(spec, ctx).await;
        let stop = !outcome.passed && spec.on_failure == OnFailure::FailFast;
        outcomes.push((spec.command.clone(), outcome));
        if stop {
            break;
        }
    }

    let decision = decide(&outcomes.iter().map(|(_, o)| o.clone()).collect::<Vec<_>>());
    let gate_failed = matches!(decision, PreGateDecision::SkipJuryFailed(_));

    for (command, outcome) in &outcomes {
        let seq = conduit.next_seq();
        conduit.publish(ProtocolEvent::CheckRunCompleted {
            session_id: session_id.to_string(),
            seq,
            created_at: now_iso(),
            check_id: outcome.id.clone(),
            command: command.clone(),
            exit_code: outcome.exit_code,
            attempts: outcome.attempts,
            duration_ms: outcome.duration_ms,
            passed: outcome.passed,
            jury_skipped: gate_failed && !outcome.passed,
        });
    }

    decision
}
