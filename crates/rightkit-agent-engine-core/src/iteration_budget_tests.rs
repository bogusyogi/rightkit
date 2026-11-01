// Test bodies for IterationBudget. Included from iteration_budget.rs — imports
// are provided by the parent `mod iteration_budget_tests { use super::*; .. }`.

fn read_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "a.rs"}).to_string(),
    }
}

fn mutating_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({"path": "a.rs", "content": "x"}).to_string(),
    }
}

// --- IterationBudget unit tests ----------------------------------

#[test]
fn budget_starts_at_max_and_unexhausted() {
    let b = IterationBudget::new(8);
    assert_eq!(b.max(), 8);
    assert!(!b.is_exhausted());
}

#[test]
fn non_programmatic_charge_costs_one_full_turn() {
    let mut b = IterationBudget::new(2);
    let cost = b.charge(false);
    assert!((cost - 1.0).abs() < f64::EPSILON);
    assert!(!b.is_exhausted());
    b.charge(false);
    assert!(b.is_exhausted());
}

#[test]
fn programmatic_charge_costs_a_fractional_turn() {
    let mut b = IterationBudget::new(1);
    let cost = b.charge(true);
    assert!((cost - 0.25).abs() < f64::EPSILON);
    assert!(!b.is_exhausted());
    b.charge(true);
    b.charge(true);
    b.charge(true);
    assert!(b.is_exhausted());
}

#[test]
fn budget_marks_exhausted_at_zero() {
    let mut b = IterationBudget::new(2);
    b.charge(false);
    assert!(!b.is_exhausted());
    b.charge(false);
    assert!(b.is_exhausted());
}

#[test]
fn fractional_charges_eventually_exhaust() {
    let mut b = IterationBudget::new(1);
    for _ in 0..4 {
        b.charge(true);
    }
    assert!(b.is_exhausted());
}

// --- classify_programmatic tests ---------------------------------

#[test]
fn programmatic_requires_tool_calls() {
    let tools = ToolRegistry::build_default();
    let r = LlmResponse::text("");
    assert!(!classify_programmatic(&r, &tools));
}

#[test]
fn programmatic_rejects_turn_with_user_visible_text() {
    let tools = ToolRegistry::build_default();
    let r = LlmResponse::text("I'll look that up.").with_tool_call(read_call("t1"));
    assert!(!classify_programmatic(&r, &tools));
}

#[test]
fn programmatic_accepts_read_only_tool_chain_with_no_text() {
    let tools = ToolRegistry::build_default();
    let r = LlmResponse::default()
        .with_tool_call(read_call("t1"))
        .with_tool_call(read_call("t2"));
    assert!(classify_programmatic(&r, &tools));
}

#[test]
fn programmatic_rejects_turn_with_any_mutating_call() {
    let tools = ToolRegistry::build_default();
    let r = LlmResponse::default()
        .with_tool_call(read_call("t1"))
        .with_tool_call(mutating_call("t2"));
    assert!(!classify_programmatic(&r, &tools));
}

// --- Graceful summary tests --------------------------------------

#[tokio::test]
async fn graceful_summary_completes_when_text_returned() {
    let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(
        "complete: nothing; blocked: missing file; next: ask user",
    )]));
    let mut transcript = Transcript::new();
    transcript.push_user("do work");
    let outcome = attempt_graceful_summary(&mut transcript, &llm).await;
    match outcome {
        GracefulSummaryOutcome::Completed(text) => {
            assert!(text.contains("complete: nothing"));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert!(transcript.messages().iter().any(|m| matches!(
        m,
        TranscriptMessage::User { content, .. }
            if content.contains("you are out of turns")
    )));
}

#[tokio::test]
async fn graceful_summary_strips_tool_calls_and_uses_text() {
    let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlm::new(vec![LlmResponse {
        text: "here is my summary".to_string(),
        reasoning: None,
        tool_calls: vec![read_call("t1")],
        finish_reason: None,
        prompt_tokens: 0,
        completion_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    }]));
    let mut transcript = Transcript::new();
    let outcome = attempt_graceful_summary(&mut transcript, &llm).await;
    match outcome {
        GracefulSummaryOutcome::Completed(text) => {
            assert!(text.contains("here is my summary"));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn graceful_summary_fails_on_empty_response() {
    let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlm::new(vec![LlmResponse::text("")]));
    let mut transcript = Transcript::new();
    let outcome = attempt_graceful_summary(&mut transcript, &llm).await;
    assert!(matches!(outcome, GracefulSummaryOutcome::Failed { .. }));
}

#[tokio::test]
async fn graceful_summary_fails_on_llm_error() {
    struct FailingLlm;
    #[async_trait::async_trait]
    impl LlmClient for FailingLlm {
        async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
            Err("rate limit".into())
        }
    }
    let llm: Arc<dyn LlmClient> = Arc::new(FailingLlm);
    let mut transcript = Transcript::new();
    let outcome = attempt_graceful_summary(&mut transcript, &llm).await;
    assert!(matches!(outcome, GracefulSummaryOutcome::Failed { .. }));
}

// --- IterationBudget.refund() tests (parity P-3) -----------------

#[test]
fn refund_nets_to_zero_and_records_audit_trail() {
    let mut b = IterationBudget::new(2);
    let cost = b.charge(true); // programmatic = 0.25
    assert!((cost - 0.25).abs() < f64::EPSILON);
    let refunded = b.refund(cost, "no-op programmatic turn");
    assert!((refunded - 0.25).abs() < f64::EPSILON);
    assert!(
        !b.is_exhausted(),
        "refund must clear exhaustion when remaining > 0"
    );
    assert!(
        (b.remaining() - 2.0).abs() < 1e-6,
        "refund must restore full headroom (charged 0.25, refunded 0.25)"
    );
    let trail = b.audit_trail();
    assert_eq!(trail.len(), 2, "audit trail records charge + refund");
    assert!(
        matches!(trail[0], BudgetEvent::Charge { amount, programmatic: true, .. } if (amount - 0.25).abs() < f64::EPSILON),
        "first entry is the charge"
    );
    assert!(
        matches!(&trail[1], BudgetEvent::Refund { amount, remaining_after, reason } if (amount - 0.25).abs() < f64::EPSILON && (*remaining_after - 2.0).abs() < 1e-6 && reason == "no-op programmatic turn"),
        "second entry is the refund with reason"
    );
}

#[test]
fn refund_clears_exhausted_latch_when_lifting_remaining_above_zero() {
    let mut b = IterationBudget::new(2);
    b.charge(false); // 1.0 → remaining = 1.0
    b.charge(false); // 1.0 → remaining = 0.0, exhausted = true
    assert!(b.is_exhausted());
    let refunded = b.refund(0.5, "internal reset");
    assert!((refunded - 0.5).abs() < f64::EPSILON);
    assert!(
        !b.is_exhausted(),
        "refund must clear exhaustion latch when remaining > 0"
    );
}

#[test]
fn refund_clamps_to_ceiling_when_amount_exceeds_headroom() {
    let mut b = IterationBudget::new(2);
    // 0.0 charged — budget already at ceiling.
    let refunded = b.refund(5.0, "excessive refund");
    assert!(refunded == 0.0, "refund beyond ceiling returns 0");
    // Now spend some, refund up to headroom only.
    b.charge(false); // remaining = 1.0, headroom = 1.0
    let refunded = b.refund(5.0, "excessive refund post-charge");
    assert!(
        (refunded - 1.0).abs() < f64::EPSILON,
        "refund must clamp to ceiling headroom (1.0)"
    );
    assert!(
        (b.remaining() - 2.0).abs() < 1e-6,
        "remaining must not exceed ceiling after refund"
    );
}

#[test]
fn refund_with_zero_amount_records_nothing_and_returns_zero() {
    let mut b = IterationBudget::new(2);
    let refunded = b.refund(0.0, "should not record");
    assert!(refunded == 0.0);
    assert_eq!(
        b.audit_trail().len(),
        0,
        "no audit entry for 0-amount refund"
    );
    let refunded = b.refund(-1.0, "negative is a no-op");
    assert!(refunded == 0.0);
    assert_eq!(
        b.audit_trail().len(),
        0,
        "no audit entry for negative refund"
    );
}
