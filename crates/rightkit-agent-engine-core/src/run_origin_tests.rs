//! Focused tests for the pre-effect origin seam integration with `SessionEngine`.
//!
//! C3 task 1 evidence: the dispatch seam refuses to execute a tool call
//! when the origin sink returns `Failed` or `Deferred`. The persistence
//! path is mocked via `InMemoryOriginSink`.

#![cfg(test)]

use crate::run_origin::{
    decide_after_origin, InMemoryOriginSink, NoopOriginSink, OriginWriteResult, PreEffectDecision,
    RunOriginSink,
};
use crate::{LlmClient, LlmResponse, ScriptedLlm, SessionEngine};
use rightkit_agent_engine_protocol::ToolCall;
use std::sync::Arc;

fn noop_call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments_json: args.into(),
    }
}

fn build_engine() -> SessionEngine {
    use crate::conduit::Conduit;
    use crate::engine::SessionEngineConfig;
    use rightkit_agent_tools::coding_tools::ToolRegistry;
    let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlm::new(Vec::<LlmResponse>::new()));
    let tools = Arc::new(ToolRegistry::new());
    let conduit = Arc::new(Conduit::new(8));
    let cfg = SessionEngineConfig::default();
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);
    engine.clear_run_origin_sink();
    engine
}

#[test]
fn record_run_origin_returns_failed_when_no_sink_installed() {
    let engine = build_engine();
    let call = noop_call("c1", "read_file", "{}");
    match engine.record_run_origin(&call) {
        OriginWriteResult::Failed(reason) => {
            assert!(reason.contains("no origin sink wired"));
        }
        other => panic!("expected Failed with no-sink reason, got {other:?}"),
    }
}

#[test]
fn record_run_origin_skips_noop_probe() {
    let engine = build_engine();
    let probe = noop_call("p", "noop", "{}");
    assert!(matches!(
        engine.record_run_origin(&probe),
        OriginWriteResult::Skipped
    ));
}

#[test]
fn record_run_origin_uses_installed_sink_and_returns_seq() {
    let mut engine = build_engine();
    let sink = Arc::new(InMemoryOriginSink::always_ready());
    engine.set_run_origin_sink(sink.clone());
    let call_a = noop_call("a", "read_file", "{}");
    let call_b = noop_call("b", "write_file", "{}");
    let r_a = engine.record_run_origin(&call_a);
    let r_b = engine.record_run_origin(&call_b);
    assert_eq!(r_a.origin_seq(), Some(1));
    assert_eq!(r_b.origin_seq(), Some(2));
    assert_eq!(
        sink.recorded(),
        vec![
            ("a".into(), "read_file".into()),
            ("b".into(), "write_file".into()),
        ]
    );
}

#[test]
fn record_run_origin_translates_failed_into_abort_decision() {
    let sink = Arc::new(InMemoryOriginSink::force_outcome(
        OriginWriteResult::Failed("locked".into()),
    ));
    let mut engine = build_engine();
    engine.set_run_origin_sink(sink);
    let call = noop_call("c", "read_file", "{}");
    let result = engine.record_run_origin(&call);
    assert_eq!(decide_after_origin(&result), PreEffectDecision::Abort);
}

#[test]
fn record_run_origin_translates_deferred_into_retry_decision() {
    let sink = Arc::new(InMemoryOriginSink::with_not_ready());
    let mut engine = build_engine();
    engine.set_run_origin_sink(sink);
    let call = noop_call("c", "read_file", "{}");
    let result = engine.record_run_origin(&call);
    assert_eq!(
        decide_after_origin(&result),
        PreEffectDecision::DeferUntilReady
    );
}

#[test]
fn noop_sink_does_not_record_anything() {
    let mut engine = build_engine();
    engine.set_run_origin_sink(Arc::new(NoopOriginSink));
    let call = noop_call("c", "read_file", "{}");
    assert!(matches!(
        engine.record_run_origin(&call),
        OriginWriteResult::Failed(_)
    ));
}

#[test]
fn sink_is_replaceable_across_lifecycle() {
    // The engine should allow swapping the sink at any time (the bin will
    // re-wire it on session restart or persistent-store re-bind).
    let mut engine = build_engine();
    let first: Arc<dyn RunOriginSink> = Arc::new(InMemoryOriginSink::force_outcome(
        OriginWriteResult::Failed("first".into()),
    ));
    let second: Arc<dyn RunOriginSink> = Arc::new(InMemoryOriginSink::always_ready());
    engine.set_run_origin_sink(first);
    let r1 = engine.record_run_origin(&noop_call("c", "read_file", "{}"));
    engine.set_run_origin_sink(second);
    let r2 = engine.record_run_origin(&noop_call("d", "read_file", "{}"));
    assert!(matches!(r1, OriginWriteResult::Failed(_)));
    assert_eq!(r2.origin_seq(), Some(1));
}

#[test]
fn failure_then_recovery_keeps_engine_dispatch_safe() {
    // Pattern from real life: storage outage hits during dispatch, then
    // recovers. The sink returns Failed for the first probe, then Persisted
    // for the retry. The dispatch is forced to abort and re-enter.
    let sink = Arc::new(InMemoryOriginSink::with_failure_sequence(vec![
        OriginWriteResult::Failed("transient".into()),
    ]));
    let mut engine = build_engine();
    engine.set_run_origin_sink(sink.clone());
    let probe = noop_call("c", "read_file", "{}");
    let r1 = engine.record_run_origin(&probe);
    let r2 = engine.record_run_origin(&noop_call("d", "write_file", "{}"));
    assert!(matches!(r1, OriginWriteResult::Failed(_)));
    assert_eq!(r2.origin_seq(), Some(1));
}

#[test]
fn origin_gate_fail_closed_under_typed_failure() {
    // C3 task 1 invariant: when the sink returns `Failed`, the dispatch
    // seam translates that into `PreEffectDecision::Abort` so the tool
    // side effect NEVER runs. This is the fail-closed property of the
    // origin seam — losing the durable artifact means losing the call.
    let sink = Arc::new(InMemoryOriginSink::force_outcome(
        OriginWriteResult::Failed("fs closed".into()),
    ));
    let mut engine = build_engine();
    engine.set_run_origin_sink(sink);
    let call = noop_call("c", "write_file", r#"{"path":"a.txt"}"#);
    let result = engine.record_run_origin(&call);
    // Sink failed → engine refuses to proceed.
    assert_eq!(decide_after_origin(&result), PreEffectDecision::Abort);
    assert!(!result.is_safe_to_proceed());
    assert_eq!(result.origin_seq(), None);
}

#[test]
fn origin_gate_fail_closed_under_deferred_signal() {
    // C3 task 1 invariant: when the sink returns `Deferred` (e.g. sink not
    // ready, queue retry), the dispatch seam translates that into
    // `PreEffectDecision::DeferUntilReady` so the side effect does NOT
    // fire on the partial state.
    let sink = Arc::new(InMemoryOriginSink::with_not_ready());
    let mut engine = build_engine();
    engine.set_run_origin_sink(sink);
    let call = noop_call("c", "write_file", "{}");
    let result = engine.record_run_origin(&call);
    assert_eq!(
        decide_after_origin(&result),
        PreEffectDecision::DeferUntilReady
    );
    assert!(!result.is_safe_to_proceed());
}

#[test]
fn origin_gate_fail_closed_when_sink_missing() {
    // C3 task 1 invariant: with NO sink installed, every call returns
    // `Failed` deterministically — no accidental Persisted leak.
    let engine = build_engine();
    for i in 0..3 {
        let call = noop_call(&format!("c{i}"), "write_file", "{}");
        let result = engine.record_run_origin(&call);
        assert!(matches!(result, OriginWriteResult::Failed(_)));
    }
}

#[test]
fn decide_after_origin_classifies_every_outcome_correctly() {
    // Belt-and-suspenders check: every `OriginWriteResult` variant maps
    // to the right `PreEffectDecision`. The dispatch site relies on this
    // mapping to choose Abort / Defer / Execute; an incorrect mapping
    // would let a Failed call slip into Execute (and a Deferred call
    // could prematurely retry with side effects).
    assert_eq!(
        decide_after_origin(&OriginWriteResult::Persisted(1)),
        PreEffectDecision::Execute
    );
    assert_eq!(
        decide_after_origin(&OriginWriteResult::Skipped),
        PreEffectDecision::Execute
    );
    assert_eq!(
        decide_after_origin(&OriginWriteResult::Deferred("retry".into())),
        PreEffectDecision::DeferUntilReady
    );
    assert_eq!(
        decide_after_origin(&OriginWriteResult::Failed("fs closed".into())),
        PreEffectDecision::Abort
    );
}

#[test]
fn in_memory_sink_returns_typed_reason_on_failure() {
    // The dispatch site surfaces the failure reason verbatim in
    // `RunGovernanceRejected::origin.reason` so the audit log can
    // reconstruct what the sink reported. Verify the reason is preserved.
    let reason = "events table is locked by another writer";
    let sink = Arc::new(InMemoryOriginSink::force_outcome(
        OriginWriteResult::Failed(reason.into()),
    ));
    let call = noop_call("c", "read_file", "{}");
    let result = sink.record_origin(&call);
    match result {
        OriginWriteResult::Failed(r) => assert_eq!(r, reason),
        other => panic!("expected Failed, got {other:?}"),
    }
}
