// Integration tests for the iteration-budget refund + graceful exhaustion
// (FEATURE-MATRIX par §#141).
//
// Co-located with the other `engine_tests_*` files and pulled in via
// `#[path] mod` from `engine_tests.rs`. These tests exercise the full
// `SessionEngine` turn loop end-to-end, not just the unit-level helpers
// in `iteration_budget.rs`.

use super::*;

// ── Helpers ───────────────────────────────────────────────────────────

fn read_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "a.rs"}).to_string(),
    }
}

fn read_call_at(id: &str, path: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": path}).to_string(),
    }
}

fn shell_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "echo hi"}).to_string(),
    }
}

fn make_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({"path": "out.rs", "content": "x"}).to_string(),
    }
}

// ── Test 1: programmatic turn does not decrement the budget ──────────

#[tokio::test]
async fn programmatic_turn_charges_fractional_budget() {
    // 1 full turn of budget. Five programmatic turns at 0.25 each = 1.25,
    // which exhausts the budget on the 5th turn. The run should still
    // get 4 programmatic turns in before exhaustion (remaining goes
    // 0.75, 0.50, 0.25, 0.00). Each of those 4 turns is a read-only
    // tool call with no user-visible text.
    let responses = vec![
        LlmResponse::default().with_tool_call(read_call_at("p1", "one.rs")),
        LlmResponse::default().with_tool_call(read_call_at("p2", "two.rs")),
        LlmResponse::default().with_tool_call(read_call_at("p3", "three.rs")),
        LlmResponse::default().with_tool_call(read_call_at("p4", "four.rs")),
        // Budget exhausted on turn 5. Graceful summary turn.
        LlmResponse::text("summary from graceful turn"),
    ];
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 1, // tiny budget so 4 programmatic turns exhaust it
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine.submit_user_message("read four files").await.unwrap();

    // The graceful summary should succeed and the run should end with
    // the summary text as the final output.
    assert_eq!(out, "summary from graceful turn");
}

// ── Test 2: non-programmatic turn decrements the budget normally ─────

#[tokio::test]
async fn non_programmatic_turn_charges_full_budget() {
    // 2-turn budget. One mutating turn (write_file) costs 1.0, leaving
    // 1.0. A second mutating turn costs 1.0, exhausting the budget.
    let responses = vec![
        LlmResponse::default().with_tool_call(make_call("m1")),
        LlmResponse::default().with_tool_call(make_call("m2")),
        // Budget exhausted on turn 3. Graceful summary.
        LlmResponse::text("done"),
    ];
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 2,
        // This test isolates iteration accounting. Post-edit verification is
        // covered by the dedicated diagnostics integration test.
        auto_post_edit_diagnostics: false,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine.submit_user_message("write two files").await.unwrap();
    assert_eq!(out, "done");
}

// ── Test 2b: no-op empty turn is refunded, not charged ──────────────

#[tokio::test]
async fn noop_empty_turn_is_refunded_not_charged() {
    // A no-op turn — the model returns neither tool calls nor user-visible
    // text — makes no progress and mutates nothing (parity P-3). Its charge
    // must be refunded so it does not burn the user's turn budget. The empty
    // turn ends the run (Finished with empty text); the budget must show the
    // charge was returned to full.
    let responses = vec![LlmResponse::default()];
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 3,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let _ = engine.submit_user_message("do nothing").await;

    // Charged 1.0 for the (non-programmatic) empty turn, then refunded 1.0.
    assert_eq!(engine.iteration_budget_remaining(), 3.0);
}

// ── Test 3: mixed turns — refund for programmatic sub-calls ─────────

#[tokio::test]
async fn mixed_turns_refund_programmatic_chains() {
    // 2-turn budget. Turn 1: read_file (programmatic, 0.25). Turn 2:
    // shell (non-programmatic, 1.0). Remaining: 2.0 - 0.25 - 1.0 = 0.75.
    // Turn 3: read_file (programmatic, 0.25). Remaining: 0.50.
    // Turn 4: read_file (programmatic, 0.25). Remaining: 0.25.
    // Turn 5: text "done" (non-programmatic, 1.0). Remaining: 0.0,
    // exhausted. Graceful summary fires.
    let responses = vec![
        LlmResponse::default().with_tool_call(read_call("p1")),
        LlmResponse::default().with_tool_call(shell_call("s1")),
        LlmResponse::default().with_tool_call(read_call("p2")),
        LlmResponse::default().with_tool_call(read_call("p3")),
        // Turn 5: a mutating tool call (not text) so the budget is
        // charged to 0 and the run doesn't exit early via
        // `handle_assistant_completion`. Remaining: 2.0 - 0.25 - 1.0
        // - 0.25 - 0.25 - 1.0 = clamped to 0.0, exhausted.
        LlmResponse::default().with_tool_call(shell_call("s5")),
        LlmResponse::text("summary from graceful turn"),
    ];
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 2,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine.submit_user_message("mix it up").await.unwrap();
    assert_eq!(out, "summary from graceful turn");
}

// ── Test 4: graceful summary on budget exhaustion ───────────────────

#[tokio::test]
async fn budget_exhaustion_triggers_graceful_summary_not_immediate_failure() {
    // 1-turn budget. Turn 1: shell call (non-programmatic, 1.0).
    // Remaining: 0.0, exhausted. Graceful summary fires.
    let responses = vec![
        LlmResponse::default().with_tool_call(shell_call("s1")),
        LlmResponse::text("here is my final summary"),
    ];
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 1,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit.clone(), cfg);

    let out = engine.submit_user_message("loop").await.unwrap();
    assert_eq!(out, "here is my final summary");

    // The terminal assistant message carries completed output for this
    // response, including graceful budget summaries.
    let events = conduit.events_after(0);
    let has_completed_summary = events.iter().any(|event| {
        matches!(event, ProtocolEvent::AssistantMessageCompleted {
            message: rightkit_agent_engine_protocol::TranscriptMessage::Assistant { content, .. }, ..
        } if content == "here is my final summary")
    });
    assert!(
        has_completed_summary,
        "graceful summary should publish its completed assistant message"
    );
    // And NO RunFailed should have been emitted (the graceful path
    // succeeded).
    let has_run_failed = events
        .iter()
        .any(|e| matches!(e, ProtocolEvent::RunFailed { .. }));
    assert!(!has_run_failed, "graceful success must not emit RunFailed");
}

// ── Test 5: graceful summary failure falls through to RunFailed ─────

#[tokio::test]
async fn graceful_summary_failure_falls_through_to_run_failed() {
    // 1-turn budget. Turn 1: shell call (non-programmatic, 1.0).
    // Remaining: 0.0, exhausted. Graceful summary turn is scripted to
    // produce empty text → summary fails → RunFailed.
    let responses = vec![
        LlmResponse::default().with_tool_call(shell_call("s1")),
        LlmResponse::text(""), // empty summary → graceful fails
    ];
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 1,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit.clone(), cfg);

    let result = engine.submit_user_message("loop").await;
    assert!(matches!(result, Err(EngineError::MaxIterations(1))));

    // RunFailed MUST fire when graceful fails (regression guard).
    let events = conduit.events_after(0);
    let has_run_failed = events
        .iter()
        .any(|e| matches!(e, ProtocolEvent::RunFailed { kind, .. } if kind == "max_iterations"));
    assert!(
        has_run_failed,
        "graceful summary failure must fall through to RunFailed"
    );
}

// ── Test 6: subagent iteration budget is independent of parent ──────

/// Recording spawner that captures the `max_iterations` option the
/// engine passes to the subagent. The parent's remaining iteration
/// budget must NOT influence this value.
struct RecordingSpawner {
    captured: std::sync::Mutex<Option<u32>>,
}

impl RecordingSpawner {
    fn new() -> Self {
        Self {
            captured: std::sync::Mutex::new(None),
        }
    }
    fn captured(&self) -> Option<u32> {
        *self.captured.lock().unwrap()
    }
}

impl rightkit_agent_tools::coding_tools::SubagentSpawner for RecordingSpawner {
    fn spawn(
        &self,
        _role: &str,
        _task: &str,
        _parent_session_id: &str,
        options: rightkit_agent_tools::coding_tools::SubagentSpawnOptions,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        *self.captured.lock().unwrap() = options.max_iterations;
        Box::pin(async { Ok("subagent done".to_string()) })
    }
}

#[tokio::test]
async fn subagent_iteration_budget_is_independent_of_parent_remaining() {
    // Parent has max_iterations = 1 (i.e. 1 full turn remaining before
    // the spawn). The model calls spawn_subagent with maxTurns: 10.
    // The subagent must receive max_iterations: Some(10), NOT
    // Some(1) — the parent's remaining budget does not constrain the
    // subagent's ceiling.
    let spawn = Arc::new(RecordingSpawner::new());
    let spawn_call = ToolCall {
        id: "tc-spawn".into(),
        name: "spawn_subagent".into(),
        arguments_json: serde_json::json!({
            "role": "coder",
            "task": "do work",
            "maxTurns": 10
        })
        .to_string(),
    };
    let responses = vec![
        LlmResponse::default().with_tool_call(spawn_call),
        LlmResponse::text("parent done"),
    ];
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 1,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);
    engine.set_spawner(spawn.clone());

    let out = engine.submit_user_message("delegate").await.unwrap();
    assert_eq!(out, "parent done");

    // The subagent received max_iterations: Some(10), proving the
    // parent's remaining budget (1 turn) did not constrain the
    // subagent's ceiling.
    assert_eq!(
        spawn.captured(),
        Some(10),
        "subagent must receive its own configured maxTurns, not the parent's remaining budget"
    );
}
