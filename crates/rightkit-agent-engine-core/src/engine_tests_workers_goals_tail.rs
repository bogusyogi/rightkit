use super::*;

#[tokio::test]
async fn hard_stopped_loop_emits_stalled_observation() {
    let mk = |id: &str| ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({ "command": "echo hi" }).to_string(),
    };
    // 5 identical shapes -> exact_stop_threshold (5) -> LoopSignal::HardStop.
    // The run ends on the hard-stop (tool_calls cleared), so no trailing text response.
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(mk("c1")),
        LlmResponse::default().with_tool_call(mk("c2")),
        LlmResponse::default().with_tool_call(mk("c3")),
        LlmResponse::default().with_tool_call(mk("c4")),
        LlmResponse::default().with_tool_call(mk("c5")),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        // This test exercises the five-repeat exact-shape stop, not MOP's
        // separate compaction threshold, which otherwise consumes a model call.
        loop_detector_config: LoopDetectorConfig {
            mop_repeat_threshold: 10,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit.clone(), cfg);

    // v1 leaves the return type unchanged: a hard-stop still resolves Ok with the
    // forced-stop text, not an Err.
    let out = engine.submit_user_message("loop please").await.unwrap();
    assert!(
        out.contains("Forced stop"),
        "hard-stop should still return the forced-stop text in v1: {out:?}"
    );

    let stalled = conduit
        .events_after(0)
        .into_iter()
        .find_map(|event| match event {
            ProtocolEvent::OrchestrationObservationRecorded { observation, .. }
                if observation.kind == "stalled" =>
            {
                Some(observation)
            }
            _ => None,
        })
        .expect("a `stalled` orchestration observation must be emitted at the hard-stop");
    assert!(
        stalled.summary.contains("agent stalled"),
        "stalled summary should come from stall_strategy::stalled_reason: {:?}",
        stalled.summary
    );
}

#[tokio::test]
async fn varied_same_tool_calls_do_not_trigger_signature_guardrail() {
    let mut response = LlmResponse::default();
    for index in 0..6 {
        response = response.with_tool_call(ToolCall {
            id: format!("read-{index}"),
            name: "read_file".into(),
            arguments_json: serde_json::json!({ "path": format!("file-{index}.md") }).to_string(),
        });
    }
    let (mut engine, conduit) = engine_with(
        vec![response, LlmResponse::text("varied reads completed")],
        PermissionMode::Bypass,
    );

    let output = engine
        .submit_user_message("read these files")
        .await
        .unwrap();
    assert_eq!(output, "varied reads completed");
    assert!(conduit.events_after(0).iter().any(|event| {
        matches!(
            event,
            ProtocolEvent::AssistantMessageCompleted {
                message: TranscriptMessage::Assistant { content, .. },
                ..
            } if content == &output
        )
    }));
}
