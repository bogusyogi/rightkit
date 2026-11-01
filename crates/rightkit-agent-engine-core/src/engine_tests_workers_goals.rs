use super::*;

#[tokio::test]
async fn worker_tool_limit_truncates_over_limit_calls_before_dispatch() {
    let first_response = LlmResponse::default()
        .with_tool_call(ToolCall {
            id: "worker-1".into(),
            name: "task_create".into(),
            arguments_json: serde_json::json!({
                "title": "first worker",
                "kind": "local_agent",
                "assigned_role": "coder"
            })
            .to_string(),
        })
        .with_tool_call(ToolCall {
            id: "record-1".into(),
            name: "task_create".into(),
            arguments_json: serde_json::json!({
                "title": "ordinary tracking record",
                "kind": "record"
            })
            .to_string(),
        })
        .with_tool_call(ToolCall {
            id: "worker-2".into(),
            name: "spawn_subagent".into(),
            arguments_json: serde_json::json!({
                "role": "coder",
                "task": "second worker"
            })
            .to_string(),
        });
    let llm = Arc::new(ScriptedLlm::new(vec![
        first_response,
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_worker_tool_calls: 1,
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit.clone(), cfg);

    let out = e.submit_user_message("fan out").await.unwrap();
    assert_eq!(out, "done");

    let (assistant_content, assistant_calls) = e
        .transcript()
        .messages()
        .iter()
        .find_map(|message| match message {
            TranscriptMessage::Assistant {
                content,
                tool_calls,
                ..
            } if !tool_calls.is_empty() => Some((content.clone(), tool_calls.clone())),
            _ => None,
        })
        .expect("assistant tool-call message");
    assert!(assistant_content.contains("Subagent limit"));
    assert_eq!(
        assistant_calls
            .iter()
            .map(|call| call.id.as_str())
            .collect::<Vec<_>>(),
        vec!["worker-1", "record-1"]
    );

    let dispatched_tool_ids = e
        .transcript()
        .messages()
        .iter()
        .filter_map(|message| match message {
            TranscriptMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(dispatched_tool_ids, vec!["worker-1", "record-1"]);

    let observation = conduit
        .events_after(0)
        .into_iter()
        .find_map(|event| match event {
            ProtocolEvent::OrchestrationObservationRecorded { observation, .. }
                if observation.kind == "worker_tool_limit" =>
            {
                Some(observation)
            }
            _ => None,
        })
        .expect("worker limit observation");
    assert_eq!(
        observation.fields.get("limit").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        observation
            .fields
            .get("totalWorkerCalls")
            .map(String::as_str),
        Some("2")
    );
    assert_eq!(
        observation.fields.get("acceptedCount").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        observation.fields.get("droppedCount").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        observation
            .fields
            .get("droppedToolCallIds")
            .map(String::as_str),
        Some("worker-2")
    );
}

#[tokio::test]
async fn steer_message_injected_before_llm_call() {
    let (mut e, _c) = engine_with(vec![LlmResponse::text("done")], PermissionMode::Bypass);
    let (steer_tx, steer_rx) = tokio::sync::mpsc::channel(8);
    e.set_steer_receiver(steer_rx);
    steer_tx.try_send("steer input".into()).unwrap();
    e.submit_user_message("original").await.unwrap();
    let has_steer = e.transcript().messages().iter().any(
        |m| matches!(m, TranscriptMessage::User { content, .. } if content.contains("steer input")),
    );
    assert!(has_steer);
}

/// A steer that lands while the final answer is streaming is delivered to the
/// model as a follow-up in the same turn, never silently dropped.
struct SteerWhileAnswering {
    tx: tokio::sync::mpsc::Sender<String>,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl LlmClient for SteerWhileAnswering {
    async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            self.tx.try_send("late steer".into()).unwrap();
            Ok(LlmResponse::text("first answer"))
        } else {
            Ok(LlmResponse::text("second answer"))
        }
    }
}

#[tokio::test]
async fn a_steer_that_arrives_during_the_final_answer_is_not_dropped() {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let llm = Arc::new(SteerWhileAnswering {
        tx,
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm.clone(), tools, conduit, cfg);
    e.set_steer_receiver(rx);
    let out = e.submit_user_message("original").await.unwrap();
    assert_eq!(
        out, "second answer",
        "the turn continued to answer the steer"
    );
    assert!(e.transcript().messages().iter().any(
        |m| matches!(m, TranscriptMessage::User { content, .. } if content.contains("late steer")),
    ));
}

struct EmptyThenText {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl LlmClient for EmptyThenText {
    async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Err("provider error: provider returned no visible content or tool calls after reasoning was separated".into())
        } else {
            Ok(LlmResponse::text("recovered"))
        }
    }
}

#[tokio::test]
async fn a_reasoning_only_completion_is_re_asked_instead_of_failing_the_turn() {
    let llm = Arc::new(EmptyThenText {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm.clone(), tools, conduit, cfg);
    assert_eq!(e.submit_user_message("hi").await.unwrap(), "recovered");
    assert_eq!(llm.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn trim_before_last_user_works_via_engine() {
    let (mut e, _c) = engine_with(vec![LlmResponse::text("resp")], PermissionMode::Bypass);
    e.submit_user_message("first").await.unwrap();
    let removed = e.trim_before_last_user();
    assert_eq!(removed, Some("first".into()));
}

#[tokio::test]
async fn poisoned_goal_lock_recovers_and_turn_completes() {
    let (mut engine, _) = engine_with(vec![LlmResponse::text("done")], PermissionMode::Bypass);
    let goal = Arc::new(Mutex::new(Some("ship the fix".to_string())));
    engine.set_goal_arc(goal.clone());
    let _ = std::thread::spawn(move || {
        let _guard = goal.lock().unwrap();
        panic!("inject goal-lock poison");
    })
    .join();

    assert_eq!(
        engine.submit_user_message("continue").await.unwrap(),
        "done"
    );
}

// â”€â”€ F2 goal continuation (turn-end drive) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn goal_state(
    budget_tokens: i64,
    tokens_used: i64,
) -> Arc<Mutex<Option<crate::goal_state::GoalState>>> {
    Arc::new(Mutex::new(Some(crate::goal_state::GoalState {
        id: "g1".into(),
        text: "ship it".into(),
        status: "active".into(),
        budget_usd: 0.0,
        budget_tokens,
        tokens_used,
        cost_usd: 0.0,
        continuation_count: 0,
        no_progress_count: 0,
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
    })))
}

/// Regression: continuation enabled but NO goal_state â‡’ identical to today
/// (returns at the first no-tool response). Guards the shared turn loop.
#[tokio::test]
async fn goalless_session_unchanged_even_with_continuation_enabled() {
    let (mut e, _c) = engine_with(vec![LlmResponse::text("done")], PermissionMode::Bypass);
    e.set_continuation_enabled(true); // no goal_state set
    let out = e.submit_user_message("hi").await.unwrap();
    assert_eq!(out, "done");
}

/// jury Test-A: never-satisfied goal, no tool activity â‡’ `blocked` after
/// exactly 3 no-progress continuations; a terminal GoalProgressUpdated fires.
#[tokio::test]
async fn goal_continuation_blocks_after_three_no_progress() {
    let (mut e, c) = engine_with(
        vec![
            LlmResponse::text("still working"),
            LlmResponse::text("still working"),
            LlmResponse::text("still working"),
        ],
        PermissionMode::Bypass,
    );
    let gs = goal_state(1_000_000, 0); // generous budget so no-progress is the bound
    e.set_goal_state_arc(gs.clone());
    e.set_continuation_enabled(true);
    e.submit_user_message("go").await.unwrap();

    let guard = gs.lock().unwrap();
    let final_gs = guard.as_ref().unwrap();
    assert_eq!(final_gs.status, "blocked");
    assert_eq!(final_gs.no_progress_count, 3);
    assert_eq!(final_gs.continuation_count, 2);

    let blocked = c.events_after(0).into_iter().any(
        |ev| matches!(ev, ProtocolEvent::GoalProgressUpdated { snapshot, .. } if snapshot.status == "blocked"),
    );
    assert!(blocked, "expected a terminal GoalProgressUpdated(blocked)");
}

/// jury Test-B: a goal already at its token ceiling â‡’ `budget-limited` on the
/// first turn-end check; the loop never marks `achieved`.
#[tokio::test]
async fn goal_continuation_stops_budget_limited_at_ceiling() {
    let (mut e, c) = engine_with(vec![LlmResponse::text("working")], PermissionMode::Bypass);
    let gs = goal_state(500, 500); // daemon-fed rollup already at the ceiling
    e.set_goal_state_arc(gs.clone());
    e.set_continuation_enabled(true);
    e.submit_user_message("go").await.unwrap();

    assert_eq!(
        gs.lock().unwrap().as_ref().unwrap().status,
        "budget-limited"
    );
    let limited = c.events_after(0).into_iter().any(|ev| {
            matches!(ev, ProtocolEvent::GoalProgressUpdated { snapshot, .. } if snapshot.status == "budget-limited")
        });
    assert!(limited, "expected GoalProgressUpdated(budget-limited)");
}

#[tokio::test]
async fn goal_progress_reenters_once_then_completed_turn_hits_continuation_bound() {
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(ToolCall {
            id: "progress-1".into(),
            name: "shell".into(),
            arguments_json: serde_json::json!({"command": "echo progress"}).to_string(),
        }),
        LlmResponse::text("progress made"),
        LlmResponse::text("completed"),
    ]));
    let conduit = Arc::new(Conduit::new(256));
    let mut engine = SessionEngine::new(
        llm.clone(),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        },
    );
    let gs = goal_state(1_000_000, 0);
    gs.lock().unwrap().as_mut().unwrap().continuation_count =
        crate::goal_state::GoalState::MAX_CONTINUATIONS - 1;
    engine.set_goal_state_arc(gs.clone());
    engine.set_continuation_enabled(true);

    assert_eq!(engine.submit_user_message("go").await.unwrap(), "completed");
    assert_eq!(
        llm.call_count(),
        3,
        "progress completion must re-enter once"
    );

    let final_goal = gs.lock().unwrap().as_ref().unwrap().clone();
    assert_eq!(
        final_goal.continuation_count,
        crate::goal_state::GoalState::MAX_CONTINUATIONS
    );
    assert_eq!(final_goal.status, "blocked");
    let events = conduit.events_after(0);
    assert!(events.iter().any(|event| matches!(
        event,
        ProtocolEvent::HumanSteeringInjected { injected_by, .. }
            if injected_by == "goal_continuation"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ProtocolEvent::GoalProgressUpdated { snapshot, .. }
            if snapshot.continuation_count == crate::goal_state::GoalState::MAX_CONTINUATIONS
                && snapshot.status == "active"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ProtocolEvent::GoalProgressUpdated { snapshot, .. }
            if snapshot.status == "blocked"
    )));
}

#[tokio::test]
async fn blocking_stop_cap_returns_without_goal_continuation_redrive() {
    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    let responses = (0..20)
        .map(|_| LlmResponse::text("ready"))
        .collect::<Vec<_>>();
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        session_id: "blocking-stop-cap-test".into(),
        permission_mode: PermissionMode::Bypass,
        max_iterations: 20,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    engine.set_hook_bus(rightkit_hooks::agent_hooks::HookBus::new(16));
    engine.set_goal_state_arc(goal_state(1_000_000, 0));
    engine.set_continuation_enabled(true);

    let cmd = if cfg!(windows) {
        r#"if "%HOST_HOOK_SESSION%"=="blocking-stop-cap-test" (echo still unsafe & exit /b 2) else (exit /b 0)"#
    } else {
        r#"if [ "$HOST_HOOK_SESSION" = "blocking-stop-cap-test" ]; then echo still unsafe; exit 2; else exit 0; fi"#
    };
    // Hook env vars are process-wide; share the hook-test lock.
    let _env_lock = super::engine_tests_basic::eff002_hook_env_guard();
    std::env::set_var("RIGHTKIT_HOOK_STOP_BLOCKING", cmd);
    let _guard = EnvGuard("RIGHTKIT_HOOK_STOP_BLOCKING");

    let out = engine.submit_user_message("finish").await.unwrap();

    assert!(out.contains("Loop-safety gate override"));
    assert!(
        engine.transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::User { content, .. }
                if content.contains("[Stop gate blocked completion]")
                    && content.contains("still unsafe")
        )),
        "blocking Stop stdout must survive as the reentry instruction"
    );
    assert!(
        conduit.events_after(0).into_iter().any(|event| matches!(
            event,
            ProtocolEvent::HumanSteeringInjected {
                injected_by,
                content_preview,
                ..
            } if injected_by == "blocking_stop_hook" && content_preview.contains("still unsafe")
        )),
        "blocking Stop stdout must survive in HumanSteeringInjected"
    );
    assert!(
        !engine.transcript.messages().iter().any(|m| matches!(
            m,
            TranscriptMessage::User { content, .. }
                if content.contains("Continue toward the goal")
        )),
        "cap path must return immediately instead of goal-continuation re-driving"
    );
}

// â”€â”€ Auto post-edit diagnostics (wiring + graceful degradation) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[tokio::test]
async fn post_edit_diagnostics_reports_optional_lsp_gap_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();
    rightkit_agent_tools::coding_tools::lsp_tool::write_lsp_workspace_config(
        dir.path(),
        &rightkit_agent_tools::coding_tools::lsp_tool::LspWorkspaceConfig {
            servers: vec![
                rightkit_agent_tools::coding_tools::lsp_tool::LspWorkspaceServer {
                    language: "rust".into(),
                    extensions: vec!["rs".into()],
                    server_path: "rightkit-agent-test-missing-language-server".into(),
                    server_args: Vec::new(),
                },
            ],
        },
    )
    .unwrap();

    let read = ToolCall {
        id: "tc-read".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "a.rs"}).to_string(),
    };
    let edit = ToolCall {
        id: "tc-edit".into(),
        name: "edit_file".into(),
        arguments_json: serde_json::json!({
            "path": "a.rs",
            "old_string": "let x = 1;",
            "new_string": "let x = 2;"
        })
        .to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(read),
        LlmResponse::default().with_tool_call(edit),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: dir.path().to_path_buf(),
        ..Default::default() // auto_post_edit_diagnostics defaults true
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);
    engine.set_run_origin_sink(Arc::new(
        crate::run_origin::InMemoryOriginSink::always_ready(),
    ));
    // The live post-edit fan-out (`ReceiptBinding::Enforced`, FM-179) only
    // considers a mutation verifiable once a host receipt backs it; wire a
    // broker rooted at this test's own tempdir so `record_assurance_receipt`
    // has somewhere to issue one, instead of racing every other test's
    // shared default temp-dir workspace (see `set_assurance_broker`).
    let broker_path = dir
        .path()
        .join(".rightkit")
        .join("sentinel")
        .join("broker.jsonl");
    let broker = crate::assurance_broker::AssuranceBroker::open(broker_path).unwrap();
    engine.set_assurance_broker(Arc::new(Mutex::new(broker)));

    let out = engine.submit_user_message("edit it").await.unwrap();
    // The language server is OPTIONAL verification feedback: its absence is
    // reported honestly but must neither fail the turn closed nor re-enter
    // the loop — the scripted "done" turn completes normally.
    assert!(
        out.contains("done"),
        "optional verifier gap must not block completion: {out:?}; transcript: {:?}",
        engine.transcript.messages()
    );

    // The edit applied: the loop ran through dispatch AND the post-edit hook.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        "fn main() {\n    let x = 2;\n}\n"
    );
    // The optional gap stays visible in the injected report, marked optional,
    // and is never converted into a clean result.
    let injected = engine.transcript.messages().iter().any(|m| {
        matches!(m, TranscriptMessage::User { content, .. }
            if content.to_ascii_lowercase().contains("optional") && content.contains("via lsp"))
    });
    assert!(
        injected,
        "missing optional LSP must surface as a reported gap"
    );
    let fail_closed = engine.transcript.messages().iter().any(|m| {
        matches!(m, TranscriptMessage::User { content, .. } if content.contains("failing closed"))
    });
    assert!(
        !fail_closed,
        "optional infrastructure gaps never fail closed"
    );
}
#[tokio::test]
async fn required_verification_blocks_completion_but_allows_diagnostic_tools() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".rightkit")).unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn value() { let x = 1; }\n").unwrap();
    std::fs::write(dir.path().join(".rightkit/verification.json"), r#"{"version":1,"commands":[{"id":"required-check","category":"test","argv":["host-missing-required-checker"],"extensions":["rs"]}]}"#).unwrap();
    let read = |id: &str| ToolCall {
        id: id.into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path":"a.rs"}).to_string(),
    };
    let edit = ToolCall {
        id: "edit".into(),
        name: "edit_file".into(),
        arguments_json:
            serde_json::json!({"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"})
                .to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(read("before")),
        LlmResponse::default().with_tool_call(edit),
        LlmResponse::default().with_tool_call(read("diagnose-one")),
        LlmResponse::text("unchecked success claim"),
        LlmResponse::default().with_tool_call(read("diagnose-two")),
        LlmResponse::text("unchecked success claim"),
        LlmResponse::text("unchecked success claim"),
    ]));
    let cfg = SessionEngineConfig {
        workspace_root: dir.path().to_path_buf(),
        workspace_trust: WorkspaceTrust::Trusted,
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(
        llm,
        Arc::new(ToolRegistry::build_default()),
        Arc::new(Conduit::new(256)),
        cfg,
    );
    engine.set_run_origin_sink(Arc::new(
        crate::run_origin::InMemoryOriginSink::always_ready(),
    ));
    let broker = crate::assurance_broker::AssuranceBroker::open(
        dir.path().join(".rightkit/sentinel/broker.jsonl"),
    )
    .unwrap();
    engine.set_assurance_broker(Arc::new(Mutex::new(broker)));
    let out = engine.submit_user_message("edit & verify").await.unwrap();
    assert!(
        out.contains("required verification still could not confirm"),
        "{out}"
    );
    assert!(!out.contains("unchecked success claim"));
    assert!(std::fs::read_to_string(dir.path().join("a.rs"))
        .unwrap()
        .contains("let x = 2;"));
    for id in ["diagnose-one", "diagnose-two"] {
        assert!(
            engine
                .transcript
                .messages()
                .iter()
                .any(|message| matches!(message,
            TranscriptMessage::Tool {tool_call_id, ..} if tool_call_id == id)),
            "diagnostic tool {id} did not run"
        );
    }
}

// Manual smoke (not automated â€” needs a real server): with rust-analyzer on
// PATH, edit a .rs file to introduce a type error; confirm the next turn's
// input contains an "[auto-diagnostics]" block naming the error.

#[tokio::test]
async fn detected_loop_injects_strategy_reset_directive() {
    let mk = |id: &str| ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({ "command": "echo hi" }).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(mk("c1")),
        LlmResponse::default().with_tool_call(mk("c2")),
        LlmResponse::default().with_tool_call(mk("c3")), // 3rd identical shape -> Warn
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine.submit_user_message("loop please").await.unwrap();
    assert_eq!(out, "done");
    assert!(
        engine.transcript.messages().iter().any(|m| matches!(
            m, TranscriptMessage::User { content, .. }
            if content.contains("fundamentally different approach")
        )),
        "the strategy-reset directive should be injected once the loop detector warns"
    );
}

#[path = "engine_tests_workers_goals_tail.rs"]
mod engine_tests_workers_goals_tail;
