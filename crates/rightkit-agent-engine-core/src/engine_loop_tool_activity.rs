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
