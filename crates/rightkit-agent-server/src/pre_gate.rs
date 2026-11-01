//! Pre-gate wiring (F3) — runs the workspace's declared deterministic checks
//! before the conductor spins up the expensive jury, emits a `CheckRunCompleted`
//! per check, and returns whether the run may proceed to the jury.
//!
//! The decision/runner logic lives in `rightkit_agent_tools::coding_tools::check_runner`; this
//! module just maps config → `CheckSpec`, owns the conduit emission, and is
//! called from `DaemonState::run_conductor` between planning and `run_graph`.

use rightkit_agent_engine_core::Conduit;
use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_agent_tools::coding_tools::check_runner::{
    decide, run_check, CheckSpec, OnFailure, PreGateDecision,
};
use rightkit_agent_tools::coding_tools::ToolContext;
use rightkit_app_settings::agent_config::PreGateCheckConfig;

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

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};

    fn cfg(id: &str, command: &str, on_failure: &str) -> PreGateCheckConfig {
        PreGateCheckConfig {
            id: id.into(),
            command: command.into(),
            on_failure: on_failure.into(),
            ..Default::default()
        }
    }

    #[test]
    fn specs_from_config_maps_and_drops_blanks() {
        let specs = specs_from_config(&[
            cfg("fmt", "cargo fmt --check", "fail-fast"),
            cfg("", "  ", "retry"),         // blank command → dropped
            cfg("", "cargo test", "retry"), // empty id → defaults to command
        ]);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].id, "fmt");
        assert_eq!(specs[0].on_failure, OnFailure::FailFast);
        assert_eq!(specs[1].id, "cargo test");
        assert_eq!(specs[1].on_failure, OnFailure::Retry);
    }

    #[tokio::test]
    async fn empty_specs_allow_jury_with_no_events() {
        let conduit = Conduit::new(16);
        let root = std::env::temp_dir();
        let ctx = ToolContext::new("s", &root, PermissionMode::Bypass, WorkspaceTrust::Trusted);
        let decision = run_pre_gate(&conduit, "s", &ctx, &[]).await;
        assert_eq!(decision, PreGateDecision::AllowJury);
        assert!(conduit.events_after(0).is_empty());
    }

    #[tokio::test]
    async fn failing_check_skips_jury_and_emits_jury_skipped() {
        let conduit = Conduit::new(16);
        let root = std::env::temp_dir();
        let ctx = ToolContext::new("s", &root, PermissionMode::Bypass, WorkspaceTrust::Trusted);
        // A destructive command is refused by the catastrophic guard inside
        // execute_shell, so it deterministically fails the gate without spawning
        // a real subprocess.
        let specs = specs_from_config(&[cfg("danger", "rm -rf /", "fail-fast")]);
        let decision = run_pre_gate(&conduit, "s", &ctx, &specs).await;
        assert!(matches!(decision, PreGateDecision::SkipJuryFailed(_)));
        let events = conduit.events_after(0);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ProtocolEvent::CheckRunCompleted {
                passed,
                jury_skipped,
                check_id,
                ..
            } => {
                assert!(!passed);
                assert!(jury_skipped);
                assert_eq!(check_id, "danger");
            }
            other => panic!("expected CheckRunCompleted, got {other:?}"),
        }
    }
}
