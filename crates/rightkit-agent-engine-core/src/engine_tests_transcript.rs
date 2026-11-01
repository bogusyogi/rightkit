use super::*;
use rightkit_agent_tools::coding_tools::{Tool, ToolContext, ToolResult};
use std::sync::atomic::{AtomicUsize, Ordering};

struct ImageReadFixture;

struct CountingReadFixture {
    executions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for CountingReadFixture {
    fn name(&self) -> &str {
        "count_read"
    }

    fn description(&self) -> &str {
        "stateful read-only fallback probe"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"value": {"type": "string"}}})
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        self.executions.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("counted")
    }
}

#[async_trait::async_trait]
impl Tool for ImageReadFixture {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "test image reader"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::ok(
            "Image file: pixel.png\nmime: image/png\ndata_url: data:image/png;base64,AAAA",
        )
    }
}

#[tokio::test]
async fn view_image_injection_waits_until_read_file_tool_result_is_paired() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("pixel.png"), png_1x1_bytes()).unwrap();
    let call = ToolCall {
        id: "tc-image".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "pixel.png"}).to_string(),
    };
    let llm = Arc::new(CapturingLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("saw image"),
    ]));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ImageReadFixture));
    let tools = Arc::new(registry);
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        workspace_root: tmp.path().to_path_buf(),
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm.clone(), tools, conduit, cfg);

    let out = engine
        .submit_user_message("inspect the image")
        .await
        .unwrap();
    assert_eq!(out, "saw image");

    let calls = llm.calls();
    assert_eq!(calls.len(), 2);
    assert!(
        !calls[0].iter().any(is_structured_view_image_user_message),
        "image data must not be injected before the tool result exists"
    );
    let second = &calls[1];
    let assistant_index = second
        .iter()
        .position(|message| {
            matches!(
                message,
                TranscriptMessage::Assistant { tool_calls, .. }
                    if tool_calls.iter().any(|call| call.id == "tc-image")
            )
        })
        .expect("assistant tool-call turn should be sent back to the provider");
    let tool_index = second
        .iter()
        .position(|message| {
            matches!(
                message,
                TranscriptMessage::Tool { tool_call_id, .. } if tool_call_id == "tc-image"
            )
        })
        .expect("tool result should be paired before image injection");
    let view_index = second
        .iter()
        .position(is_structured_view_image_user_message)
        .expect("structured view_image user message should be injected");

    assert!(assistant_index < tool_index);
    assert!(tool_index < view_index);
    match &second[view_index] {
        TranscriptMessage::User {
            content, pinned, ..
        } => {
            assert!(*pinned);
            assert!(content.contains("data:image/png;base64,"));
        }
        _ => panic!("expected user image injection"),
    }
}

#[test]
fn view_image_clear_signal_prevents_injecting_earlier_images() {
    let mut transcript = Transcript::new();
    let call = ToolCall {
        id: "tc-image".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "pixel.png"}).to_string(),
    };
    transcript.push_assistant("", None, vec![call]);
    transcript.push_tool_result(
        "tc-image",
        "Image file: pixel.png\nmime: image/png\ndata_url: data:image/png;base64,AAAA",
        false,
    );
    transcript.push_user("[view_image:clear]");

    assert!(!inject_pending_view_images(&mut transcript));
    assert!(!transcript
        .messages()
        .iter()
        .any(is_structured_view_image_user_message));
}

#[test]
fn repairs_dangling_tool_calls_with_synthetic_errors_in_call_order() {
    let mut transcript = Transcript::new();
    let call_a = ToolCall {
        id: "tc-a".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path":"a"}).to_string(),
    };
    let call_b = ToolCall {
        id: "tc-b".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command":"echo b"}).to_string(),
    };
    transcript.push_assistant("", None, vec![call_a.clone(), call_b.clone()]);
    transcript.push_tool_result("tc-b", "b complete", false);
    transcript.push_user("continue");

    let inserted = repair_dangling_tool_calls(&mut transcript);

    assert_eq!(inserted, 1);
    let messages = transcript.messages();
    assert!(matches!(messages[0], TranscriptMessage::Assistant { .. }));
    assert!(matches!(
        &messages[1],
        TranscriptMessage::Tool {
            tool_call_id,
            is_error: true,
            summary,
            ..
        } if tool_call_id == "tc-a" && summary.contains("did not complete")
    ));
    assert!(matches!(
        &messages[2],
        TranscriptMessage::Tool {
            tool_call_id,
            summary,
            is_error: false,
            ..
        } if tool_call_id == "tc-b" && summary == "b complete"
    ));
    assert!(matches!(messages[3], TranscriptMessage::User { .. }));
}

#[tokio::test]
async fn resumed_session_repairs_dangling_tool_calls_before_next_llm_call() {
    let call = ToolCall {
        id: "tc-resume".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command":"cargo test"}).to_string(),
    };
    let (mut e, _c) = engine_with(
        vec![LlmResponse::text("resumed safely")],
        PermissionMode::Bypass,
    );
    e.seed_transcript(vec![TranscriptMessage::Assistant {
        id: "ast-old".into(),
        created_at: "2026-06-08T00:00:00Z".into(),
        content: String::new(),
        reasoning_content: None,
        tool_calls: vec![call],
        pinned: false,
    }]);

    let out = e.submit_user_message("continue").await.unwrap();

    assert_eq!(out, "resumed safely");
    let repaired = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                tool_call_id,
                is_error: true,
                summary,
                ..
            } if tool_call_id == "tc-resume" && summary.contains("did not complete")
        )
    });
    assert!(repaired, "{:#?}", e.transcript().messages());
}

#[tokio::test]
async fn safety_finish_strips_tool_calls_and_completes_with_stop_note() {
    let call = ToolCall {
        id: "tc-safety".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command":"echo should-not-run"}).to_string(),
    };
    let (mut e, _c) = engine_with(
        vec![LlmResponse::text("I cannot continue")
            .with_tool_call(call)
            .with_finish_reason("content_filter")],
        PermissionMode::Bypass,
    );

    let out = e.submit_user_message("unsafe request").await.unwrap();

    assert!(out.contains("I cannot continue"));
    assert!(out.contains("Stopped by provider safety policy"));
    assert!(!e
        .transcript()
        .messages()
        .iter()
        .any(|m| matches!(m, TranscriptMessage::Tool { .. })));
    let assistant = e.transcript().messages().iter().find_map(|m| match m {
        TranscriptMessage::Assistant {
            content,
            tool_calls,
            ..
        } => Some((content, tool_calls)),
        _ => None,
    });
    let Some((content, tool_calls)) = assistant else {
        panic!("expected assistant message");
    };
    assert!(tool_calls.is_empty());
    assert!(content.contains("No requested tool calls were run"));
}

#[tokio::test]
async fn non_safety_tool_calls_finish_reason_still_dispatches_tools() {
    let call = ToolCall {
        id: "tc-normal".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command":"echo hi"}).to_string(),
    };
    let (mut e, _c) = engine_with(
        vec![
            LlmResponse::default()
                .with_tool_call(call)
                .with_finish_reason("tool_calls"),
            LlmResponse::text("done"),
        ],
        PermissionMode::Bypass,
    );

    let out = e.submit_user_message("run normal tool").await.unwrap();

    assert_eq!(out, "done");
    assert!(e.transcript().messages().iter().any(
        |m| matches!(m, TranscriptMessage::Tool { tool_call_id, .. } if tool_call_id == "tc-normal")
    ));
}

fn repeated_shell_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command":"echo loop"}).to_string(),
    }
}

#[tokio::test]
async fn repeated_tool_shape_warning_is_injected_at_next_llm_boundary() {
    let llm = Arc::new(CapturingLlm::new(vec![
        LlmResponse::default().with_tool_call(repeated_shell_call("tc-loop-1")),
        LlmResponse::default().with_tool_call(repeated_shell_call("tc-loop-2")),
        LlmResponse::default().with_tool_call(repeated_shell_call("tc-loop-3")),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 8,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm.clone(), tools, conduit, cfg);

    let out = engine.submit_user_message("repeat a tool").await.unwrap();

    assert_eq!(out, "done");
    let calls = llm.calls();
    assert_eq!(calls.len(), 4);
    assert!(calls[3].iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::User { content, .. }
                if content.contains("[Loop warning]")
                    && content.contains("exact repeated tool-call shape")
        )
    }));
}

#[tokio::test]
async fn repeated_tool_shape_hard_stop_strips_tool_calls_before_dispatch() {
    let llm = Arc::new(CapturingLlm::new(vec![
        LlmResponse::default().with_tool_call(repeated_shell_call("tc-hard-1")),
        LlmResponse::default().with_tool_call(repeated_shell_call("tc-hard-2")),
        LlmResponse::default().with_tool_call(repeated_shell_call("tc-hard-3")),
        LlmResponse::default().with_tool_call(repeated_shell_call("tc-hard-4")),
        LlmResponse::default().with_tool_call(repeated_shell_call("tc-hard-5")),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        max_iterations: 8,
        // MOP's independent lower repeat threshold otherwise consumes a
        // model response before the exact-shape hard-stop regression does.
        loop_detector_config: LoopDetectorConfig {
            mop_repeat_threshold: 10,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine
        .submit_user_message("repeat until stopped")
        .await
        .unwrap();

    assert!(out.contains("Forced stop"));
    assert!(out.contains("loop detector halted tool execution"));
    assert!(!engine.transcript().messages().iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::Tool { tool_call_id, .. } if tool_call_id == "tc-hard-5"
        )
    }));
    let final_assistant = engine
        .transcript()
        .messages()
        .iter()
        .rev()
        .find_map(|message| match message {
            TranscriptMessage::Assistant {
                content,
                tool_calls,
                ..
            } => Some((content, tool_calls)),
            _ => None,
        });
    let Some((content, tool_calls)) = final_assistant else {
        panic!("expected final assistant message");
    };
    assert!(content.contains("Forced stop"));
    assert!(tool_calls.is_empty());
}

// ── EFF-009: incomplete-generation gate corpus ─────────────────────────────
//
// Provider-family truncation corpus: for every incomplete/malformed
// destructive call, prove zero approval, zero `ToolCallStarted`, zero
// filesystem mutation, and zero process launch. A trailing control case
// proves the gate does not block a normal complete generation.

fn destructive_write_call(id: &str, path: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({"path": path, "content": "payload"}).to_string(),
    }
}

fn destructive_shell_call(id: &str, marker: &std::path::Path) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "shell".into(),
        arguments_json: serde_json::json!({
            "command": format!("touch {}", marker.display())
        })
        .to_string(),
    }
}

/// Assert the zero-effect invariant for one gated turn: no approval event,
/// no `ToolCallStarted`, no `Tool` transcript message, and the caller's
/// filesystem probe closure reports nothing happened either.
fn assert_zero_dispatch_effects(engine: &SessionEngine, conduit: &Conduit) {
    let events = conduit.events_after(0);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ProtocolEvent::ToolCallStarted { .. })),
        "expected zero ToolCallStarted, got: {events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ProtocolEvent::ToolApprovalRequested { .. })),
        "expected zero ToolApprovalRequested, got: {events:#?}"
    );
    assert!(
        !engine
            .transcript()
            .messages()
            .iter()
            .any(|m| matches!(m, TranscriptMessage::Tool { .. })),
        "expected zero Tool transcript messages, got: {:#?}",
        engine.transcript().messages()
    );
}

fn gated_engine(
    tmp: &tempfile::TempDir,
    responses: Vec<LlmResponse>,
) -> (SessionEngine, Arc<Conduit>) {
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        workspace_root: tmp.path().to_path_buf(),
        permission_mode: PermissionMode::Bypass,
        // These tests script exactly one write and one final answer; the
        // completion-time verification recheck (receipt-bound, no assurance
        // broker here) would legitimately ask the model once more.
        auto_post_edit_diagnostics: false,
        ..Default::default()
    };
    let engine = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    (engine, conduit)
}

#[tokio::test]
async fn eff009_max_tokens_finish_reason_blocks_destructive_write() {
    let tmp = tempfile::tempdir().unwrap();
    let call = destructive_write_call("tc-eff009-1", "out.txt");
    let (mut e, c) = gated_engine(
        &tmp,
        vec![LlmResponse::default()
            .with_tool_call(call)
            .with_finish_reason("length")],
    );

    let out = e.submit_user_message("write the file").await.unwrap();

    assert!(
        out.contains("incomplete"),
        "expected incompleteness note in output, got: {out}"
    );
    assert!(!tmp.path().join("out.txt").exists());
    assert_zero_dispatch_effects(&e, &c);
}

#[tokio::test]
async fn eff009_provider_family_truncation_corpus_blocks_destructive_calls() {
    // OpenAI-compatible clients expose `length`/`max_tokens`; other adapters
    // normalize equivalent output ceilings as `max_output_tokens`,
    // `token_limit`, or `output_limit`. Every family must fail closed before
    // approval, ToolCallStarted, or the workspace mutation.
    for (index, reason) in [
        "length",
        "max_tokens",
        "max_output_tokens",
        "token_limit",
        "output_limit",
    ]
    .into_iter()
    .enumerate()
    {
        let tmp = tempfile::tempdir().unwrap();
        let path = format!("truncated-{index}.txt");
        let call = destructive_write_call(&format!("tc-eff009-family-{index}"), &path);
        let (mut e, c) = gated_engine(
            &tmp,
            vec![LlmResponse::default()
                .with_tool_call(call)
                .with_finish_reason(reason)],
        );

        let out = e.submit_user_message("write the file").await.unwrap();

        assert!(out.contains("incomplete"), "reason={reason}: {out}");
        assert!(!tmp.path().join(path).exists(), "reason={reason}");
        assert_zero_dispatch_effects(&e, &c);
    }
}

#[tokio::test]
async fn eff009_stream_truncated_mid_tool_call_blocks_dispatch() {
    // A stream cut off mid-tool-call: the provider's own termination signal
    // (max_tokens) plus a JSON fragment that never closed. Malformed AND
    // incomplete; either reason alone must be enough to gate it.
    let tmp = tempfile::tempdir().unwrap();
    let mut call = destructive_write_call("tc-eff009-2", "out2.txt");
    call.arguments_json = r#"{"path":"out2.txt","content":"unterminated"#.to_string();
    let (mut e, c) = gated_engine(
        &tmp,
        vec![LlmResponse::default()
            .with_tool_call(call)
            .with_finish_reason("max_tokens")],
    );

    let out = e.submit_user_message("write the file").await.unwrap();

    assert!(!out.trim().is_empty());
    assert!(!tmp.path().join("out2.txt").exists());
    assert_zero_dispatch_effects(&e, &c);
}

struct TruncatedStreamingProbe {
    call: ToolCall,
}

#[async_trait::async_trait]
impl LlmClient for TruncatedStreamingProbe {
    async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        Ok(LlmResponse::text("unexpected non-streaming fallback"))
    }

    async fn stream_with_metadata(
        &self,
        _messages: &[TranscriptMessage],
        _metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponseStream, String> {
        Ok(Box::pin(futures::stream::iter([
            Ok(LlmStreamEvent::ToolCall(self.call.clone())),
            Ok(LlmStreamEvent::Finished(Some("length".into()))),
        ])))
    }
}

#[tokio::test]
async fn eff009_stream_complete_json_with_length_reason_never_starts_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("stream-truncated.txt");
    let call = destructive_write_call("tc-eff009-stream", target.to_str().unwrap());
    let conduit = Arc::new(Conduit::new(256));
    let mut e = SessionEngine::new(
        Arc::new(TruncatedStreamingProbe { call }),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            workspace_root: tmp.path().to_path_buf(),
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        },
    );

    let out = e.submit_user_message("write the file").await.unwrap();

    assert!(
        out.contains("incomplete"),
        "expected truncation note: {out}"
    );
    assert!(
        !target.exists(),
        "truncated stream must not mutate workspace"
    );
    assert_zero_dispatch_effects(&e, &conduit);
}

#[tokio::test]
async fn eff009_stream_readonly_call_also_waits_for_complete_generation() {
    // EFF-009 is generation-level, not merely mutation-level: even a call
    // that would qualify for the streaming read-only fast path must not start
    // before the provider confirms a complete generation.
    let tmp = tempfile::tempdir().unwrap();
    let call = ToolCall {
        id: "tc-eff009-readonly".into(),
        name: "sleep".into(),
        arguments_json: serde_json::json!({"milliseconds": 1}).to_string(),
    };
    let conduit = Arc::new(Conduit::new(256));
    let mut e = SessionEngine::new(
        Arc::new(TruncatedStreamingProbe { call }),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            workspace_root: tmp.path().to_path_buf(),
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        },
    );

    let out = e.submit_user_message("sleep briefly").await.unwrap();

    assert!(
        out.contains("incomplete"),
        "expected truncation note: {out}"
    );
    assert_zero_dispatch_effects(&e, &conduit);
}

#[tokio::test]
async fn eff009_syntactically_valid_json_is_not_enough_when_provider_marks_incomplete() {
    // The key EFF-009 assertion: a destructive shell call whose JSON parses
    // perfectly fine must still be withheld once the provider itself says
    // the generation was cut off.
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("should-not-exist.marker");
    let call = destructive_shell_call("tc-eff009-3", &marker);
    let (mut e, c) = gated_engine(
        &tmp,
        vec![LlmResponse::default()
            .with_tool_call(call)
            .with_finish_reason("output_limit")],
    );

    e.submit_user_message("run the command").await.unwrap();

    assert!(
        !marker.exists(),
        "process must not have launched: marker file exists"
    );
    assert_zero_dispatch_effects(&e, &c);
}

#[tokio::test]
async fn eff009_malformed_tool_call_blocks_dispatch_even_with_complete_finish_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("should-not-exist-2.marker");
    let mut call = destructive_shell_call("tc-eff009-4", &marker);
    call.name = String::new(); // malformed: empty tool name
    let (mut e, c) = gated_engine(
        &tmp,
        vec![LlmResponse::default()
            .with_tool_call(call)
            .with_finish_reason("stop")],
    );

    e.submit_user_message("run the command").await.unwrap();

    assert!(!marker.exists());
    assert_zero_dispatch_effects(&e, &c);
}

#[tokio::test]
async fn eff009_control_complete_generation_still_dispatches() {
    // Control: a normal, complete generation must NOT be blocked by the gate.
    let tmp = tempfile::tempdir().unwrap();
    let call = destructive_write_call("tc-eff009-control", "control.txt");
    let (mut e, c) = gated_engine(
        &tmp,
        vec![
            LlmResponse::default()
                .with_tool_call(call)
                .with_finish_reason("stop"),
            LlmResponse::text("done"),
        ],
    );

    let out = e.submit_user_message("write the file").await.unwrap();

    assert_eq!(out, "done");
    assert!(tmp.path().join("control.txt").exists());
    let events = c.events_after(0);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProtocolEvent::ToolCallStarted { .. })),
        "expected the control case to dispatch: {events:#?}"
    );
    assert!(e
        .transcript()
        .messages()
        .iter()
        .any(|m| matches!(m, TranscriptMessage::Tool { tool_call_id, .. } if tool_call_id == "tc-eff009-control")));
}

#[tokio::test]
async fn eff001_real_dispatch_records_canonical_production_admission_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let call = destructive_write_call("tc-eff001-real-trace", "trace.txt");
    let (mut engine, _conduit) = gated_engine(
        &tmp,
        vec![
            LlmResponse::default()
                .with_tool_call(call)
                .with_finish_reason("stop"),
            LlmResponse::text("done"),
        ],
    );

    assert_eq!(
        engine.submit_user_message("write the trace").await.unwrap(),
        "done"
    );
    let trace = crate::tool_dispatch_plumbing::production_admission_trace();
    assert_eq!(
        trace.stages_for("tc-eff001-real-trace"),
        crate::tool_dispatch_plumbing::EffectAdmissionStage::canonical_order(),
        "real dispatch must populate the shared sink in canonical order"
    );
}

#[tokio::test]
async fn eff001_readonly_loop_batch_uses_canonical_dispatch_seam() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("batch-input.txt");
    std::fs::write(&input, "batch payload").unwrap();
    let call = ToolCall {
        id: "tc-eff001-readonly-batch".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": input}).to_string(),
    };
    let (mut engine, _conduit) = gated_engine(&tmp, vec![]);
    let mut results = std::collections::HashMap::new();
    let calls = [&call];

    engine
        .dispatch_concurrent_batch_bounded(&calls, &mut results)
        .await;

    assert!(results.get(&call.id).is_some_and(|result| !result.is_error));
    let trace = crate::tool_dispatch_plumbing::production_admission_trace();
    assert_eq!(
        trace.stages_for(&call.id),
        crate::tool_dispatch_plumbing::EffectAdmissionStage::canonical_order(),
        "readonly batch ingress must use canonical dispatch stages"
    );
}

#[tokio::test]
async fn eff001_readonly_transform_falls_back_to_serial_authority() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("serial-input.txt");
    std::fs::write(&input, "serial payload").unwrap();
    let call = ToolCall {
        id: "tc-eff001-readonly-transform".into(),
        name: "read_file".into(),
        arguments_json: format!("{} ", serde_json::json!({"path": input})),
    };
    let (mut engine, _conduit) = gated_engine(&tmp, vec![]);
    let mut results = std::collections::HashMap::new();
    let calls = [&call];

    engine
        .dispatch_concurrent_batch_bounded(&calls, &mut results)
        .await;

    assert!(results.get(&call.id).is_some_and(|result| !result.is_error));
    assert_eq!(
        crate::tool_dispatch_plumbing::production_admission_trace().stages_for(&call.id),
        crate::tool_dispatch_plumbing::EffectAdmissionStage::canonical_order(),
        "transformed readonly calls must use serial governed admission"
    );
}

#[tokio::test]
async fn eff001_serial_fallback_executes_stateful_tool_once() {
    let tmp = tempfile::tempdir().unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CountingReadFixture {
        executions: executions.clone(),
    }));
    let conduit = Arc::new(Conduit::new(256));
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(registry),
        conduit,
        SessionEngineConfig {
            workspace_root: tmp.path().to_path_buf(),
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        },
    );
    let call = ToolCall {
        id: "tc-eff001-count-once".into(),
        name: "count_read".into(),
        arguments_json: r#"{"value":"probe"} "#.into(),
    };
    let mut results = std::collections::HashMap::new();
    let calls = [&call];

    engine
        .dispatch_concurrent_batch_bounded(&calls, &mut results)
        .await;

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(results.get(&call.id).is_some_and(|result| !result.is_error));
}

#[tokio::test]
async fn eff001_ineligible_batch_candidate_records_typed_denial() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut engine, conduit) = gated_engine(&tmp, vec![]);
    let call = destructive_write_call("tc-eff001-denied-batch", "must-not-exist.txt");
    let mut results = std::collections::HashMap::new();
    let calls = [&call];

    engine
        .dispatch_concurrent_batch_bounded(&calls, &mut results)
        .await;

    assert!(results.get(&call.id).is_some_and(|result| result.is_error));
    assert!(!tmp.path().join("must-not-exist.txt").exists());
    assert!(engine
        .transcript()
        .messages()
        .iter()
        .any(|message| matches!(
            message,
            TranscriptMessage::Tool { tool_call_id, is_error: true, .. }
                if tool_call_id == &call.id
        )));
    assert!(!conduit
        .events_after(0)
        .iter()
        .any(|event| matches!(event, ProtocolEvent::ToolCallStarted { .. })));
}

/// EFF-002 end-to-end: the pre-execute digest recheck must actually block a
/// payload that changed after approval.
///
/// The digest function's own unit tests only prove collision resistance; none
/// of them would fail if the recheck block in `execute_single_tool_call` were
/// deleted. This drives the real execute path with a seeded approval digest
/// that does not match the payload about to run, and asserts the effect never
/// happens.
#[tokio::test]
async fn eff002_execute_refuses_a_payload_that_changed_after_approval() {
    let workspace = tempfile::tempdir().expect("workspace");
    let target = workspace.path().join("eff002_must_not_exist.txt");

    let call = ToolCall {
        id: "tc-eff002".into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({
            "path": target.to_string_lossy(),
            "content": "written despite a stale approval",
        })
        .to_string(),
    };

    let llm = Arc::new(CapturingLlm::new(vec![
        LlmResponse::default().with_tool_call(call.clone()),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: workspace.path().to_path_buf(),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    // The operator approved some OTHER payload; this is the mismatch the
    // recheck exists to catch.
    engine.seed_approved_effect_digest(&call.id, "sha256:approved-something-else");

    let mut turn_results = std::collections::HashMap::new();
    let result = engine
        .execute_single_tool_call("s1", &call, None, true, &mut turn_results)
        .await;

    assert!(
        result.is_none(),
        "a payload that no longer matches its approval must not execute"
    );
    assert!(
        !target.exists(),
        "the effect must not have landed: a stale approval cannot authorize it"
    );
    let recorded = turn_results
        .get(&call.id)
        .expect("a refusal must be recorded for the model to see");
    assert!(
        recorded.is_error,
        "the refusal must be surfaced as an error, not a silent skip"
    );
    assert!(
        recorded.content.contains("Approval no longer matches"),
        "the refusal must say why; got: {}",
        recorded.content
    );
    assert_eq!(
        engine.approved_effect_digest_count(),
        0,
        "the grant must be consumed, so it cannot authorize a later attempt"
    );
}

/// Control: a matching digest still executes, so the gate does not block
/// legitimate approved work.
#[tokio::test]
async fn eff002_execute_proceeds_when_the_payload_still_matches_its_approval() {
    let workspace = tempfile::tempdir().expect("workspace");
    let target = workspace.path().join("eff002_expected.txt");

    let call = ToolCall {
        id: "tc-eff002-ok".into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({
            "path": target.to_string_lossy(),
            "content": "approved and unchanged",
        })
        .to_string(),
    };

    let llm = Arc::new(CapturingLlm::new(vec![LlmResponse::text("done")]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: workspace.path().to_path_buf(),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    // Compute the digest exactly as the approval path does.
    let (args, _) = crate::tool_dispatch_plumbing::parse_tool_arguments_attributed(
        &call.arguments_json,
        None,
        Some(&call.name),
        None,
    );
    let digest = crate::tool_dispatch_plumbing::effect_payload_digest(&call.name, &args);
    engine.seed_approved_effect_digest(&call.id, &digest);

    let mut turn_results = std::collections::HashMap::new();
    let result = engine
        .execute_single_tool_call("s1", &call, None, true, &mut turn_results)
        .await;

    assert!(
        result.is_some(),
        "an unchanged approved payload must execute"
    );
    assert!(
        target.exists(),
        "the approved effect must actually land when the digest matches"
    );
}

/// Build an engine and a `write_file` call for the approval-digest tests.
fn eff_digest_fixture(
    id: &str,
    workspace: &std::path::Path,
    name: &str,
) -> (SessionEngine, ToolCall, std::path::PathBuf) {
    let target = workspace.join(name);
    let call = ToolCall {
        id: id.into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({
            "path": target.to_string_lossy(),
            "content": "payload",
        })
        .to_string(),
    };
    let llm = Arc::new(CapturingLlm::new(vec![LlmResponse::text("done")]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: workspace.to_path_buf(),
        ..Default::default()
    };
    (SessionEngine::new(llm, tools, conduit, cfg), call, target)
}

/// An approval-required grant with no digest must be denied, not executed.
#[tokio::test]
async fn eff002_approval_required_grant_without_digest_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (mut engine, call, target) =
        eff_digest_fixture("tc-eff002-missing", workspace.path(), "missing.txt");
    engine.seed_approval_required(&call.id);

    let mut turn_results = std::collections::HashMap::new();
    let result = engine
        .execute_single_tool_call("s1", &call, None, true, &mut turn_results)
        .await;

    assert!(result.is_none(), "missing digest must deny execution");
    assert!(!target.exists(), "the effect must not land");
    let recorded = turn_results.get(&call.id).expect("refusal recorded");
    assert!(recorded.is_error);
    assert!(
        recorded.content.contains("no payload digest"),
        "got: {}",
        recorded.content
    );
}

/// A grant is single-use: replaying a consumed approval-required grant denies.
#[tokio::test]
async fn eff002_replayed_approval_grant_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (mut engine, call, target) =
        eff_digest_fixture("tc-eff002-replay", workspace.path(), "replay.txt");
    let (args, _) = crate::tool_dispatch_plumbing::parse_tool_arguments_attributed(
        &call.arguments_json,
        None,
        Some(&call.name),
        None,
    );
    let digest = crate::tool_dispatch_plumbing::effect_payload_digest(&call.name, &args);
    engine.seed_approval_required(&call.id);
    engine.seed_approved_effect_digest(&call.id, &digest);

    let mut turn_results = std::collections::HashMap::new();
    let first = engine
        .execute_single_tool_call("s1", &call, None, true, &mut turn_results)
        .await;
    assert!(first.is_some(), "the matching grant executes once");
    assert!(target.exists());
    std::fs::remove_file(&target).expect("reset effect");

    let mut replay_results = std::collections::HashMap::new();
    let replay = engine
        .execute_single_tool_call("s1", &call, None, true, &mut replay_results)
        .await;
    assert!(replay.is_none(), "a consumed grant must not run again");
    assert!(!target.exists(), "the replay must not re-apply the effect");
}

/// A mutated payload under an approval-required grant is denied (existing
/// mismatch check still applies when the marker is present).
#[tokio::test]
async fn eff002_approval_required_grant_with_mutated_digest_is_denied() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (mut engine, call, target) =
        eff_digest_fixture("tc-eff002-mutated", workspace.path(), "mutated.txt");
    engine.seed_approval_required(&call.id);
    engine.seed_approved_effect_digest(&call.id, "sha256:some-other-payload");

    let mut turn_results = std::collections::HashMap::new();
    let result = engine
        .execute_single_tool_call("s1", &call, None, true, &mut turn_results)
        .await;
    assert!(result.is_none());
    assert!(!target.exists());
}

/// Routine policy-authorized execution (no approval prompt, no digest, no
/// approval-required marker) still runs.
#[tokio::test]
async fn eff002_policy_authorized_call_without_approval_still_runs() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (mut engine, call, target) =
        eff_digest_fixture("tc-eff002-policy", workspace.path(), "policy.txt");

    let mut turn_results = std::collections::HashMap::new();
    let result = engine
        .execute_single_tool_call("s1", &call, None, true, &mut turn_results)
        .await;
    assert!(result.is_some(), "policy-authorized call must execute");
    assert!(target.exists());
}

/// Issue #56: a consumed grant's marker is pruned at turn end, while the
/// replay is still denied before then.
#[tokio::test]
async fn eff002_consumed_marker_is_pruned_after_turn_but_replay_denied_before() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (mut engine, call, target) =
        eff_digest_fixture("tc-eff002-prune", workspace.path(), "prune.txt");
    let (args, _) = crate::tool_dispatch_plumbing::parse_tool_arguments_attributed(
        &call.arguments_json,
        None,
        Some(&call.name),
        None,
    );
    let digest = crate::tool_dispatch_plumbing::effect_payload_digest(&call.name, &args);
    engine.seed_approval_required(&call.id);
    engine.seed_approved_effect_digest(&call.id, &digest);

    let mut turn_results = std::collections::HashMap::new();
    assert!(engine
        .execute_single_tool_call("s1", &call, None, true, &mut turn_results)
        .await
        .is_some());
    assert_eq!(
        engine.approval_required_marker_count(),
        1,
        "kept for replay denial"
    );
    std::fs::remove_file(&target).expect("reset effect");
    let mut replay_results = std::collections::HashMap::new();
    assert!(engine
        .execute_single_tool_call("s1", &call, None, true, &mut replay_results)
        .await
        .is_none());

    engine.prune_resolved_approval_markers();
    assert_eq!(engine.approval_required_marker_count(), 0);
}

/// A marker whose grant is still live (digest outstanding) must survive pruning.
#[tokio::test]
async fn eff002_prune_keeps_marker_of_live_grant() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (engine, call, _target) =
        eff_digest_fixture("tc-eff002-live", workspace.path(), "live.txt");
    engine.seed_approval_required(&call.id);
    engine.seed_approved_effect_digest(&call.id, "sha256:live");
    engine.prune_resolved_approval_markers();
    assert_eq!(engine.approval_required_marker_count(), 1);
    assert_eq!(engine.approved_effect_digest_count(), 1);
}

/// Deny/timeout clears both the marker and any digest for the call id.
#[tokio::test]
async fn eff002_denied_request_clears_marker_and_digest() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (engine, call, _target) =
        eff_digest_fixture("tc-eff002-deny", workspace.path(), "deny.txt");
    engine.seed_approval_required(&call.id);
    engine.seed_approved_effect_digest(&call.id, "sha256:stale");
    engine.clear_unapproved_request(&call.id);
    assert_eq!(engine.approval_required_marker_count(), 0);
    assert_eq!(engine.approved_effect_digest_count(), 0);
}
