// Test bodies for StallGuardrail. Included from stall_guardrail.rs — imports
// are provided by the parent `mod stall_guardrail_tests { use super::*; .. }`.

fn tc(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.into(),
        arguments_json: args.to_string(),
    }
}

// -------- Spec test 1: 1x identical -> Allow --------

#[test]
fn identical_call_once_returns_allow() {
    let mut g = StallGuardrail::new();
    let call = tc("read_file", serde_json::json!({"path": "a.txt"}));
    assert_eq!(g.record_call(&call), StallSeverity::Allow);
}

// -------- Spec test 2: 2x consecutive -> Warn (and call still executes) --------

#[test]
fn identical_call_twice_consecutive_returns_warn() {
    let mut g = StallGuardrail::new();
    let call = tc("read_file", serde_json::json!({"path": "a.txt"}));
    let _ = g.record_call(&call);
    let sev = g.record_call(&call);
    assert_eq!(sev, StallSeverity::Warn { count: 2 });
    assert!(!sev.is_skip());
    assert!(!sev.is_block());
}

// -------- Spec test 3: 4x consecutive -> Skip (synthetic tool result) --------

#[test]
fn for_each_iteration_returns_expected_severity_signature_tier() {
    let mut g = StallGuardrail::new();
    let call = tc("shell", serde_json::json!({"command": "ls"}));
    let expectations = [
        StallSeverity::Allow,
        StallSeverity::Warn { count: 2 },
        StallSeverity::Warn { count: 3 },
        StallSeverity::Skip { count: 4 },
        StallSeverity::Skip { count: 5 },
        StallSeverity::Block { count: 6 },
    ];
    for (i, expected) in expectations.iter().enumerate() {
        assert_eq!(
            g.record_call(&call),
            *expected,
            "iteration {i} should have produced the expected severity"
        );
    }
}

#[test]
fn skipped_message_contains_tool_name_and_count() {
    let m = skipped_message("read_file", 4);
    assert!(m.contains("read_file"), "message must name the tool: {m}");
    assert!(m.contains("4"), "message must surface the count: {m}");
    assert!(
        m.contains("SKIPPED") || m.contains("skipped"),
        "message must clearly say it was skipped (not the real tool output): {m}"
    );
}

#[test]
fn warn_message_distinct_from_skipped_message() {
    let w = warn_message("read_file", 2);
    let s = skipped_message("read_file", 4);
    assert!(w.contains("read_file"));
    assert!(w.contains("2"));
    assert!(!w.contains("SKIPPED"));
    assert_ne!(w, s);
}

// -------- Spec test 4: 6x consecutive -> Block; goal closes as blocked --------

#[test]
fn identical_call_six_consecutive_returns_block_and_closes_goal_as_blocked() {
    let mut g = StallGuardrail::new();
    let call = tc("read_file", serde_json::json!({"path": "a.txt"}));
    for _ in 0..5 {
        let _ = g.record_call(&call);
    }
    let sev = g.record_call(&call);
    assert_eq!(sev, StallSeverity::Block { count: 6 });

    let arc = Arc::new(Mutex::new(Some(GoalState {
        id: "g-block-spec".into(),
        text: "ship".into(),
        status: "active".into(),
        budget_usd: 0.0,
        budget_tokens: 1_000_000,
        tokens_used: 0,
        cost_usd: 0.0,
        continuation_count: 0,
        no_progress_count: GoalState::MAX_NO_PROGRESS - 1,
        gds_score: 0.0,
        verification_present: false,
        verification_approved: false,
        started_at: "t0".into(),
        closed_at: None,
        dismissed_at: None,
        decomposition_state: "missing".into(),
        plan_revision: 0,
        now: None,
        next: None,
        steps: Vec::new(),
        verification_state: "unverified".into(),
    })));
    let update = update_goal_state(&arc, false).expect("active goal should update");
    assert_eq!(
        update.decision,
        ContinueDecision::Close("blocked".into()),
        "MAX_NO_PROGRESS reached -> Close( blocked )"
    );
    assert_eq!(arc.lock().unwrap().as_ref().unwrap().status, "blocked");
}

// -------- Varied calls are normal coding work. The graduated guardrail is
//         signature-based; the separate global no-progress path still fires
//         independently if no turn makes progress. --------

#[test]
fn varied_args_do_not_trigger_signature_skip_or_block() {
    let mut g = StallGuardrail::new();

    for index in 0..6 {
        let call = tc(
            "shell",
            serde_json::json!({"command": format!("check-{index}")}),
        );
        assert_eq!(g.record_call(&call), StallSeverity::Allow);
    }
    assert_eq!(g.tool_count("shell"), 6);

    let arc = Arc::new(Mutex::new(Some(GoalState {
        id: "g-varied".into(),
        text: "ship".into(),
        status: "active".into(),
        budget_usd: 0.0,
        budget_tokens: 1_000_000,
        tokens_used: 0,
        cost_usd: 0.0,
        continuation_count: 0,
        no_progress_count: GoalState::MAX_NO_PROGRESS - 1,
        gds_score: 0.0,
        verification_present: false,
        verification_approved: false,
        started_at: "t0".into(),
        closed_at: None,
        dismissed_at: None,
        decomposition_state: "missing".into(),
        plan_revision: 0,
        now: None,
        next: None,
        steps: Vec::new(),
        verification_state: "unverified".into(),
    })));
    let update = update_goal_state(&arc, false).expect("active goal should advance");
    assert_eq!(
        update.decision,
        ContinueDecision::Close("blocked".into()),
        "the global MAX_NO_PROGRESS path fires regardless of the per-signature tier state"
    );
}

// -------- Spec test 6: interleaving identical-A and identical-B does not
//         cross-contaminate counts (A and B tracked separately). --------

#[test]
fn interleaving_a_and_b_keeps_independent_per_signature_counts() {
    let mut g = StallGuardrail::new();
    let a = tc("tool_a", serde_json::json!({"path": "a.txt"}));
    let b = tc("tool_b", serde_json::json!({"path": "b.txt"}));
    let hash_a = normalize_args_hash(&a.arguments_json);
    let hash_b = normalize_args_hash(&b.arguments_json);

    assert_eq!(g.record_call(&a), StallSeverity::Allow);
    assert_eq!(g.record_call(&b), StallSeverity::Allow);
    assert_eq!(g.signature_count("tool_a", hash_a), 1);
    assert_eq!(g.signature_count("tool_b", hash_b), 1);
    assert_eq!(g.tool_count("tool_a"), 1);
    assert_eq!(g.tool_count("tool_b"), 1);

    for _ in 0..3 {
        let _ = g.record_call(&a);
    }
    assert_eq!(g.signature_count("tool_a", hash_a), 4);
    assert_eq!(g.signature_count("tool_b", hash_b), 1);
    assert_eq!(g.tool_count("tool_a"), 4);
    assert_eq!(g.tool_count("tool_b"), 1);

    assert_eq!(g.record_call(&a), StallSeverity::Skip { count: 5 });
    assert_eq!(g.signature_count("tool_a", hash_a), 5);
    assert_eq!(g.record_call(&b), StallSeverity::Warn { count: 2 });
    assert_eq!(g.signature_count("tool_b", hash_b), 2);
}

// -------- Reset semantics --------

#[test]
fn reset_clears_signature_and_tool_tiers() {
    let mut g = StallGuardrail::new();
    let call = tc("read_file", serde_json::json!({"path": "a.txt"}));
    for _ in 0..5 {
        let _ = g.record_call(&call);
    }
    g.reset();
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

// -------- Block-message shape (parity with `stalled_reason`) --------

#[test]
fn block_message_is_concrete_and_mentions_count() {
    let m = block_message("read_file", 6);
    assert!(m.contains("read_file"));
    assert!(m.contains("6"));
    assert!(m.contains("blocked"));
}

#[test]
fn repeated_call_with_changing_results_is_progress() {
    let mut g = StallGuardrail::new();
    let read = tc("browser_read", serde_json::json!({"session": "bs-0"}));
    for page in 0..6 {
        let severity = g.record_call(&read);
        assert!(
            !matches!(
                severity,
                StallSeverity::Skip { .. } | StallSeverity::Block { .. }
            ),
            "read {page} after a page change must run: {severity:?}"
        );
        g.observe_result(&read, &format!("page state {page}"));
    }
    // Unchanged results are still a stall.
    for _ in 0..3 {
        let _ = g.record_call(&read);
        g.observe_result(&read, "page state 5");
    }
    assert!(matches!(g.record_call(&read), StallSeverity::Skip { .. }));
}
