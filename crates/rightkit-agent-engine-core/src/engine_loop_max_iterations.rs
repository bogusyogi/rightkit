use super::*;
use crate::engine::iteration_budget::{attempt_graceful_summary, GracefulSummaryOutcome};

impl SessionEngine {
    /// Handle budget exhaustion. FEATURE-MATRIX par §#141: instead of
    /// immediately emitting `RunFailed`, attempt ONE more turn with
    /// the model told to summarize and stop calling tools (tool calls
    /// in the response are stripped). Only if that final text-only
    /// turn ALSO fails does the engine fall through to the existing
    /// `RunFailed` path — preserving the abrupt-failure regression
    /// guard while adding the graceful-success path.
    ///
    /// This is the genuinely-reachable code path the FEATURE-MATRIX
    /// row calls for. It is wired into the real turn loop (see
    /// `engine_loop.rs`) and exercised by the graceful-exhaustion
    /// tests in `iteration_budget_tests`.
    pub(in crate::engine) async fn attempt_budget_exhaustion_summary(
        &mut self,
        sid: &str,
        session_span_id: Option<&str>,
    ) -> Result<String, EngineError> {
        let max = self.iteration_budget.max();
        match attempt_graceful_summary(&mut self.transcript, &self.llm).await {
            GracefulSummaryOutcome::Completed(text) => {
                // Graceful success: emit the assistant delta, push the
                // assistant message, and return Ok with the summary as
                // the final output. This is the NEW path added by
                // par §#141 — the model produced a summary, the run
                // ends successfully (not RunFailed).
                self.emit(|seq| ProtocolEvent::AssistantDelta {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    text: text.clone(),
                    is_reasoning: false,
                });
                self.transcript
                    .push_assistant(text.clone(), None, Vec::new());
                self.emit(|seq| ProtocolEvent::AssistantMessageCompleted {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    message: rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                        id: "graceful-summary".into(),
                        created_at: now_iso(),
                        content: text.clone(),
                        reasoning_content: None,
                        tool_calls: vec![],
                        pinned: false,
                        origin: self.llm.message_origin(),
                    },
                    phase: rightkit_agent_engine_protocol::AssistantMessagePhase::Final,
                });
                // CLI-01, ORCH-04: the summary is not a finished result. Say so in a typed
                // event, after the summary so a reader that keeps "the last answer" can mark it.
                self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                        id: format!("turn-limit-{seq}"),
                        kind: crate::TURN_LIMIT_OBSERVATION_KIND.into(),
                        subject: sid.to_string(),
                        summary: format!(
                            "the turn spent its budget of {max} round(s) and ended on a summary of the work so far"
                        ),
                        fields: [("max_iterations".to_string(), max.to_string())]
                            .into_iter()
                            .collect(),
                        created_at: now_iso(),
                    },
                });
                self.record_session_end(
                    session_span_id,
                    "completed",
                    Some("iteration budget graceful summary"),
                );
                Ok(text)
            }
            GracefulSummaryOutcome::Failed { reason } => {
                // Fallback to the existing abrupt-failure path. The
                // summary turn either errored or produced no text —
                // we cannot complete the run gracefully, so we emit
                // RunFailed exactly as the original code did. This
                // preserves the regression guarantee that the
                // abrupt-failure path still exists.
                self.emit(|seq| ProtocolEvent::RunFailed {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    error: format!(
                        "max tool iterations exceeded ({max}); graceful summary failed: {reason}"
                    ),
                    kind: "max_iterations".into(),
                    hint: "The agent reached the iteration budget; the graceful summary \
                           turn also failed."
                        .into(),
                });
                self.record_session_end(
                    session_span_id,
                    "failed",
                    Some("max iterations; graceful summary failed"),
                );
                Err(EngineError::MaxIterations(max))
            }
        }
    }
}
