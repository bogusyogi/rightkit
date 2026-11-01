// Test bodies for StallGuardrail. Included from stall_guardrail.rs — imports
// are provided by the parent `mod stall_guardrail_tests { use super::*; .. }`.

fn tc(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.into(),
        arguments_json: args.to_string(),
    }
}

// -------- Sessions rule 32: the fourth identical call in a row is an Ask --------

#[test]
fn first_three_identical_calls_are_allowed_and_the_fourth_asks() {
    let mut g = StallGuardrail::new();
    let call = tc("read_file", serde_json::json!({"path": "a.txt"}));
    for _ in 0..3 {
        assert_eq!(g.record_call(&call), StallSeverity::Allow);
    }
    let fourth = g.record_call(&call);
    assert_eq!(fourth, StallSeverity::Ask { count: 4 });
    assert!(fourth.is_ask());
    assert_eq!(g.run_length(), 4);
}

#[test]
fn later_identical_calls_keep_asking() {
    let mut g = StallGuardrail::new();
    let call = tc("read_file", serde_json::json!({"path": "a.txt"}));
    for _ in 0..4 {
        let _ = g.record_call(&call);
    }
    assert_eq!(g.record_call(&call), StallSeverity::Ask { count: 5 });
}

#[test]
fn another_call_in_between_restarts_the_count() {
    let mut g = StallGuardrail::new();
    let a = tc("tool_a", serde_json::json!({"path": "a.txt"}));
    let b = tc("tool_b", serde_json::json!({"path": "b.txt"}));
    for _ in 0..3 {
        let _ = g.record_call(&a);
    }
    assert_eq!(g.record_call(&b), StallSeverity::Allow);
    assert_eq!(g.run_length(), 1);
    // `a` starts over: it needs three more runs before it asks again.
    for _ in 0..3 {
        assert_eq!(g.record_call(&a), StallSeverity::Allow);
    }
    assert!(g.record_call(&a).is_ask());
}

#[test]
fn varied_args_never_ask() {
    let mut g = StallGuardrail::new();
    for index in 0..8 {
        let call = tc(
            "shell",
            serde_json::json!({"command": format!("check-{index}")}),
        );
        assert_eq!(g.record_call(&call), StallSeverity::Allow);
    }
}

#[test]
fn reset_clears_the_run() {
    let mut g = StallGuardrail::new();
    let call = tc("read_file", serde_json::json!({"path": "a.txt"}));
    for _ in 0..5 {
        let _ = g.record_call(&call);
    }
    g.reset();
    assert_eq!(g.run_length(), 0);
    assert_eq!(g.record_call(&call), StallSeverity::Allow);
}

// -------- Signature normalization (key-order invariance) --------

#[test]
fn normalize_args_hash_ignores_object_key_order() {
    let a = r#"{"path":"a.txt","limit":10}"#;
    let b = r#"{"limit":10,"path":"a.txt"}"#;
    assert_eq!(
        normalize_args_hash(a),
        normalize_args_hash(b),
        "two argument JSONs differing only by key order must share a hash"
    );
}

#[test]
fn normalize_args_hash_distinguishes_arg_values() {
    let a = r#"{"path":"a.txt"}"#;
    let b = r#"{"path":"b.txt"}"#;
    assert_ne!(
        normalize_args_hash(a),
        normalize_args_hash(b),
        "different argument values must produce different hashes"
    );
}

#[test]
fn normalize_args_hash_falls_back_to_raw_string_for_invalid_json() {
    let s = "not valid json";
    assert_eq!(normalize_args_hash(s), normalize_args_hash(s));
}

// -------- Messages --------

#[test]
fn repeated_call_messages_name_the_tool_and_the_reason() {
    let reason = repeated_call_reason("read_file", 4);
    assert!(reason.starts_with(REPEATED_CALL_REASON));
    assert!(reason.contains("read_file"));
    assert!(reason.contains('4'));
    let denied = repeated_call_denied_message("read_file");
    assert!(denied.contains("read_file"));
    assert!(denied.contains(REPEATED_CALL_REASON));
}

// -------- A repeat whose result changed is progress --------

#[test]
fn repeated_call_with_changing_results_is_progress() {
    let mut g = StallGuardrail::new();
    let read = tc("browser_read", serde_json::json!({"session": "bs-0"}));
    for page in 0..6 {
        let severity = g.record_call(&read);
        assert!(
            !severity.is_ask(),
            "read {page} after a page change must run: {severity:?}"
        );
        g.observe_result(&read, &format!("page state {page}"));
    }
    // Unchanged results are still a loop.
    for _ in 0..3 {
        let _ = g.record_call(&read);
        g.observe_result(&read, "page state 5");
    }
    assert!(g.record_call(&read).is_ask());
}
