use super::*;

impl SessionEngine {
    pub(in crate::engine) async fn observe_response_tool_activity(
        &mut self,
        response: &crate::llm::LlmResponse,
        sid: &str,
        iteration: usize,
        mop: &mut crate::mop::MopDetector,
        tool_calls_since_continuation: &mut usize,
    ) {
        // Feed the meltdown detector with this turn's action signature.
        if response.has_tool_calls() {
            *tool_calls_since_continuation += 1;
            let action: String = response
                .tool_calls
                .iter()
                .map(crate::mop::tool_call_shape)
                .collect::<Vec<_>>()
                .join("|");
            if mop.record(crate::mop::signature(&action)) {
                // A loop may request compaction, but it cannot authorize a
                // deterministic lossy rewrite. Use the same active-model,
                // fail-closed path as automatic and manual compaction.
                let compaction = self.compact_context().await;
                let (kind, summary) = match compaction {
                    Ok(outcome) if outcome.compacted => (
                        "mop_context_reset",
                        format!(
                            "agent looping at iteration {iteration}; active-model compaction dropped {} messages",
                            outcome.dropped_count
                        ),
                    ),
                    Ok(_) => (
                        "mop_strategy_reset",
                        format!(
                            "agent looping at iteration {iteration}; no eligible context was compacted"
                        ),
                    ),
                    Err(error) => (
                        "mop_strategy_reset",
                        format!(
                            "agent looping at iteration {iteration}; compaction failed closed ({error})"
                        ),
                    ),
                };
                self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                        id: format!("mop-{iteration}"),
                        kind: kind.into(),
                        subject: sid.to_string(),
                        summary,
                        fields: std::collections::HashMap::new(),
                        created_at: now_iso(),
                    },
                });
                self.transcript
                    .push_user(crate::stall_strategy::STRATEGY_RESET_DIRECTIVE);
                mop.reset();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestArchiveSink;

    #[async_trait::async_trait]
    impl crate::compaction_archive::CompactionArchiveSink for TestArchiveSink {
        async fn archive(
            &self,
            input: crate::compaction_archive::CompactionArchiveInput,
        ) -> Result<
            crate::compaction_archive::CompactionArchiveReceipt,
            crate::compaction_archive::CompactionArchiveError,
        > {
            Ok(crate::compaction_archive::CompactionArchiveReceipt {
                archive_id: "mop-test-archive".into(),
                sha256: "0".repeat(64),
                byte_length: serde_json::to_vec(&input.messages)
                    .map_err(|error| error.to_string())?
                    .len(),
                message_count: input.messages.len(),
                compaction_epoch: 0,
                durable_high_water: 0,
            })
        }
    }

    fn engine_with_summary(
        responses: Vec<crate::llm::LlmResponse>,
    ) -> (SessionEngine, Arc<crate::llm::ScriptedLlm>, Arc<Conduit>) {
        let llm = Arc::new(crate::llm::ScriptedLlm::new(responses));
        let conduit = Arc::new(Conduit::new(32));
        let mut engine = SessionEngine::new(
            llm.clone(),
            Arc::new(ToolRegistry::build_default()),
            conduit.clone(),
            SessionEngineConfig {
                session_id: format!("mop-test-{}", rightkit_agent_engine_protocol::uuid_v7()),
                ..SessionEngineConfig::default()
            },
        );
        engine.set_compaction_archive_sink(Arc::new(TestArchiveSink));
        for index in 0..12 {
            if index % 2 == 0 {
                engine.transcript.push_user(format!(
                    "request {index} {}",
                    "lorem ipsum dolor sit amet ".repeat(80)
                ));
            } else {
                engine.transcript.push_assistant(
                    format!(
                        "answer {index} {}",
                        "lorem ipsum dolor sit amet ".repeat(80)
                    ),
                    None,
                    Vec::new(),
                );
            }
        }
        (engine, llm, conduit)
    }

    fn repeated_tool_response() -> crate::llm::LlmResponse {
        crate::llm::LlmResponse::default().with_tool_call(ToolCall {
            id: "repeat".into(),
            name: "read_file".into(),
            arguments_json: r#"{"path":"src/lib.rs"}"#.into(),
        })
    }

    #[tokio::test]
    async fn mop_compaction_uses_active_model_and_records_evidence() {
        let summary = crate::session_state_summary::SessionStateSummary {
            goal: "active-model MOP summary".into(),
            current_state: "complete".into(),
            ..Default::default()
        }
        .render_markdown();
        let (mut engine, llm, conduit) =
            engine_with_summary(vec![crate::llm::LlmResponse::text(summary)]);
        let response = repeated_tool_response();
        let mut mop = crate::mop::MopDetector::new(8, 2);
        let mut tool_calls = 0;

        engine
            .observe_response_tool_activity(&response, "s1", 1, &mut mop, &mut tool_calls)
            .await;
        engine
            .observe_response_tool_activity(&response, "s1", 2, &mut mop, &mut tool_calls)
            .await;

        assert_eq!(llm.call_count(), 1);
        assert!(engine.transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::Compact { summary, .. }
                if crate::session_state_summary::SessionStateSummary::parse_markdown(summary)
                    .is_some_and(|parsed| parsed.goal == "active-model MOP summary")
        )));
        assert!(conduit.events_after(0).iter().any(|event| matches!(
            event,
            ProtocolEvent::OrchestrationObservationRecorded { observation, .. }
                if observation.kind == "mop_context_reset"
        )));
    }

    #[tokio::test]
    async fn mop_summary_failure_keeps_full_transcript_and_only_pivots_strategy() {
        let (mut engine, llm, conduit) = engine_with_summary(Vec::new());
        let before = engine.transcript.messages().to_vec();
        let response = repeated_tool_response();
        let mut mop = crate::mop::MopDetector::new(8, 2);
        let mut tool_calls = 0;

        engine
            .observe_response_tool_activity(&response, "s1", 1, &mut mop, &mut tool_calls)
            .await;
        engine
            .observe_response_tool_activity(&response, "s1", 2, &mut mop, &mut tool_calls)
            .await;

        // ActiveModelCompactionSummarizer retries up to MAX_COMPACTION_ATTEMPTS
        // (3) times before failing closed (see default_compaction.rs) -- a
        // deliberate resilience mechanism for transient failures, not a bug
        // in the MOP path. With no scripted responses every attempt errors,
        // so all 3 are spent before compact_context() returns Err.
        assert_eq!(llm.call_count(), 3);
        let after = engine.transcript.messages();
        assert_eq!(
            serde_json::to_string(&after[..before.len()]).unwrap(),
            serde_json::to_string(&before).unwrap()
        );
        assert!(!after
            .iter()
            .any(|message| matches!(message, TranscriptMessage::Compact { .. })));
        assert!(
            matches!(after.last(), Some(TranscriptMessage::User { content, .. })
            if content == crate::stall_strategy::STRATEGY_RESET_DIRECTIVE)
        );
        assert!(conduit.events_after(0).iter().any(|event| matches!(
            event,
            ProtocolEvent::OrchestrationObservationRecorded { observation, .. }
                if observation.kind == "mop_strategy_reset"
        )));
    }

    fn paged_tool_response(offset: usize) -> crate::llm::LlmResponse {
        // Mirrors the installed Membrane pagination loop: a long shared
        // id/hash prefix (>64 chars, identical across pages) with only the
        // trailing offset advancing. With the old 64-char truncation these
        // all hashed to the same signature and falsely triggered MOP.
        crate::llm::LlmResponse::default().with_tool_call(ToolCall {
            id: format!("call-{offset}"),
            name: "membrane_memory_read".into(),
            arguments_json: format!(
                r#"{{"id":"host-absorption-20260922/taste_0190b974c08f2311ebae30b7","expectedContentHash":"sha256:c4b05bd3d26c6e02605fa563e86758561d6b434417ac35890bbdd20e47e73862","offset":{offset},"maxChars":1600}}"#
            ),
        })
    }

    #[tokio::test]
    async fn advancing_page_offsets_with_shared_long_prefix_do_not_trigger_mop() {
        let (mut engine, llm, conduit) = engine_with_summary(Vec::new());
        let mut mop = crate::mop::MopDetector::new(8, 2);
        let mut tool_calls = 0;

        for (index, offset) in [0usize, 1600, 3200, 4800, 6400].iter().enumerate() {
            let response = paged_tool_response(*offset);
            engine
                .observe_response_tool_activity(
                    &response,
                    "s1",
                    index + 1,
                    &mut mop,
                    &mut tool_calls,
                )
                .await;
        }

        // No compaction LLM request, no mop observation, no injected directive.
        assert_eq!(llm.call_count(), 0);
        assert!(!conduit.events_after(0).iter().any(|event| matches!(
            event,
            ProtocolEvent::OrchestrationObservationRecorded { observation, .. }
                if observation.kind.starts_with("mop_")
        )));
        assert!(!engine.transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::User { content, .. }
                if content == crate::stall_strategy::STRATEGY_RESET_DIRECTIVE
        )));
    }

    #[tokio::test]
    async fn semantically_identical_calls_still_reach_mop_path() {
        // Reordered JSON keys and distinct transient call ids must
        // canonicalize to the same shape so real repetition is still caught.
        let (mut engine, llm, conduit) = engine_with_summary(Vec::new());
        let mut mop = crate::mop::MopDetector::new(8, 2);
        let mut tool_calls = 0;

        let first = crate::llm::LlmResponse::default().with_tool_call(ToolCall {
            id: "tc-a".into(),
            name: "read_file".into(),
            arguments_json: r#"{"path":"src/lib.rs","line":10}"#.into(),
        });
        let second = crate::llm::LlmResponse::default().with_tool_call(ToolCall {
            id: "tc-b".into(),
            name: "read_file".into(),
            arguments_json: r#"{"line":10,"path":"src/lib.rs"}"#.into(),
        });

        engine
            .observe_response_tool_activity(&first, "s1", 1, &mut mop, &mut tool_calls)
            .await;
        engine
            .observe_response_tool_activity(&second, "s1", 2, &mut mop, &mut tool_calls)
            .await;

        // MOP fired: compaction was attempted via the fail-closed active-model
        // path (3 attempts, no scripted responses) and the reset was observed.
        assert_eq!(llm.call_count(), 3);
        assert!(conduit.events_after(0).iter().any(|event| matches!(
            event,
            ProtocolEvent::OrchestrationObservationRecorded { observation, .. }
                if observation.kind == "mop_strategy_reset"
        )));
        assert!(matches!(
            engine.transcript.messages().last(),
            Some(TranscriptMessage::User { content, .. })
                if content == crate::stall_strategy::STRATEGY_RESET_DIRECTIVE
        ));
    }
}
