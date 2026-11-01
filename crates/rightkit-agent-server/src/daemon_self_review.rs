use rightkit_agent_engine_core::llm::{LlmClient, LlmRequestMetadata};
use rightkit_agent_engine_core::self_review::{
    global_self_review_fork, stable_self_review_proposal_id, RestrictedSelfReviewFork,
    SelfReviewInput,
};
use rightkit_agent_engine_protocol::{
    now_iso, ProtocolEvent, SelfReviewProposalDraft, SelfReviewProposalKind, TranscriptMessage,
};
use std::sync::Arc;

pub struct DaemonSelfReviewFork {
    llm: Arc<dyn LlmClient>,
    conduit: Arc<rightkit_agent_engine_core::conduit::Conduit>,
    learning_sink: Option<Arc<dyn RestrictedSelfReviewFork>>,
}

impl DaemonSelfReviewFork {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        conduit: Arc<rightkit_agent_engine_core::conduit::Conduit>,
    ) -> Self {
        Self::with_learning_sink(llm, conduit, global_self_review_fork())
    }

    fn with_learning_sink(
        llm: Arc<dyn LlmClient>,
        conduit: Arc<rightkit_agent_engine_core::conduit::Conduit>,
        learning_sink: Option<Arc<dyn RestrictedSelfReviewFork>>,
    ) -> Self {
        Self {
            llm,
            conduit,
            learning_sink,
        }
    }
}

impl RestrictedSelfReviewFork for DaemonSelfReviewFork {
    fn review_and_propose(&self, input: SelfReviewInput) {
        if !input.is_governed() {
            return;
        }
        let llm = self.llm.clone();
        let conduit = self.conduit.clone();
        let learning_sink = self.learning_sink.clone();
        let session_id = input.session_id.clone();
        tokio::spawn(async move {
            // Build a transcript for the self-review
            let mut messages = vec![];

            // Add the stable system prompt
            if let Ok(stable_prompt) = String::from_utf8(input.stable_system_prompt_bytes.clone()) {
                messages.push(TranscriptMessage::System {
                    id: "self-review-system".into(),
                    created_at: now_iso(),
                    content: stable_prompt,
                    pinned: true,
                });
            }

            // Add self-review instruction
            messages.push(TranscriptMessage::User {
                id: "self-review-instruction".into(),
                created_at: now_iso(),
                content: "Review your last response and the conversation history. Look for:\n1. Any mistakes or inaccuracies\n2. Opportunities to improve future work\n3. Lessons learned that could be stored as memory\n4. Suggestions for workflow improvements\n\nIf you have a proposal, describe it clearly. If not, just say 'No proposal'. Keep your response concise and focused.".into(),
                trust: None,
                pinned: false,
                attachments: Vec::new(),
            });

            // Add the transcript
            messages.extend(input.transcript.clone());

            // Call the LLM
            let result = llm
                .complete_with_metadata(
                    &messages,
                    &LlmRequestMetadata {
                        stable_prefix_key: input.stable_system_cache_key.clone(),
                        ..Default::default()
                    },
                )
                .await;

            match result {
                Ok(response)
                    if !response.text.trim().is_empty()
                        && response.text.trim() != "No proposal" =>
                {
                    let summary = response
                        .text
                        .lines()
                        .find_map(|line| {
                            let trimmed = line.trim();
                            let lower = trimmed.to_ascii_lowercase();
                            let marked = lower
                                .strip_prefix("remember:")
                                .or_else(|| lower.strip_prefix("learned:"))
                                .or_else(|| lower.strip_prefix("correction:"))?;
                            (marked.trim().len() >= 8).then(|| {
                                trimmed
                                    .split_once(':')
                                    .map(|(_, rest)| rest.trim())
                                    .unwrap_or(trimmed)
                                    .to_string()
                            })
                        })
                        .unwrap_or_else(|| response.text.trim().to_string());
                    let id = stable_self_review_proposal_id(&session_id, &summary);
                    let proposal = SelfReviewProposalDraft {
                        id,
                        kind: SelfReviewProposalKind::Memory,
                        summary: summary.clone(),
                        evidence: response.text.clone(),
                    };

                    // The daemon adapter owns async provider work, while the
                    // learning sink owns synchronous durable quarantine. Feed
                    // it a marked review line so this event and stored row
                    // share the same deterministic identity.
                    if let Some(sink) = learning_sink {
                        let mut learning_input = input;
                        learning_input.assistant_text = format!("Remember: {summary}");
                        sink.review_and_propose(learning_input);
                    }

                    // Emit the event
                    conduit.publish(ProtocolEvent::SelfReviewProposalReceived {
                        session_id: session_id.clone(),
                        seq: conduit.next_seq(),
                        created_at: now_iso(),
                        proposal,
                    });
                }
                Ok(_) => {
                    // No proposal, do nothing
                }
                Err(e) => {
                    eprintln!("Self review error: {}", e);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learning::LearningHarness;
    use rightkit_agent_engine_core::conduit::ConduitReceive;
    use rightkit_agent_engine_core::llm::{LlmResponse, ScriptedLlm};
    use rightkit_agent_engine_core::self_review::SELF_REVIEW_ALLOWED_TOOLS;

    fn governed_input() -> SelfReviewInput {
        SelfReviewInput {
            session_id: "live-review".into(),
            model: "haiku".into(),
            allowed_tools: SELF_REVIEW_ALLOWED_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            shell_allowed: false,
            stable_system_prompt_bytes: b"stable\n".to_vec(),
            stable_system_cache_key: Some("cache:stable".into()),
            transcript: Vec::new(),
            assistant_text: "visible answer".into(),
        }
    }

    struct MetadataRecordingLlm {
        metadata: std::sync::Mutex<Vec<LlmRequestMetadata>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for MetadataRecordingLlm {
        async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
            Ok(LlmResponse::text(
                "Remember: preserve review proposals until human approval.",
            ))
        }

        async fn complete_with_metadata(
            &self,
            messages: &[TranscriptMessage],
            metadata: &LlmRequestMetadata,
        ) -> Result<LlmResponse, String> {
            self.metadata.lock().unwrap().push(metadata.clone());
            self.complete(messages).await
        }
    }

    #[tokio::test]
    async fn live_fork_emits_event_and_persists_candidate_quarantine() {
        let harness = LearningHarness::new();
        let llm = Arc::new(MetadataRecordingLlm {
            metadata: std::sync::Mutex::new(Vec::new()),
        });
        let llm_for_assertions = llm.clone();
        let conduit = Arc::new(rightkit_agent_engine_core::conduit::Conduit::new(32));
        let mut subscription = conduit.subscribe();
        let fork =
            DaemonSelfReviewFork::with_learning_sink(llm, conduit, Some(Arc::new(harness.clone())));

        fork.review_and_propose(governed_input());

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), subscription.recv())
            .await
            .expect("background review should publish a live event");
        let proposal = match event {
            ConduitReceive::Event(ProtocolEvent::SelfReviewProposalReceived {
                session_id,
                proposal,
                ..
            }) => {
                assert_eq!(session_id, "live-review");
                proposal
            }
            other => panic!("unexpected self-review event: {other:?}"),
        };

        let stored = harness
            .proposal(&proposal.id)
            .expect("live proposal should be written to learning quarantine");
        assert_eq!(stored.status, rightkit_adapt::ProposalStatus::Candidate);
        assert_eq!(stored.score, 0.0);
        assert!(harness.lessons().is_empty());
        assert_eq!(
            llm_for_assertions.metadata.lock().unwrap()[0].stable_prefix_key,
            Some("cache:stable".into()),
            "live fork must reuse parent stable cache key"
        );
    }

    #[tokio::test]
    async fn live_fork_rejects_shell_or_extra_tool_authority_before_spawn() {
        let llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(
            "Remember: should never run.",
        )]));
        let conduit = Arc::new(rightkit_agent_engine_core::conduit::Conduit::new(32));
        let fork = DaemonSelfReviewFork::with_learning_sink(llm.clone(), conduit.clone(), None);
        let mut input = governed_input();
        input.shell_allowed = true;
        fork.review_and_propose(input);
        tokio::task::yield_now().await;
        assert_eq!(llm.call_count(), 0);
        assert!(conduit.events_after(0).is_empty());

        let mut input = governed_input();
        input.allowed_tools.push("shell".into());
        fork.review_and_propose(input);
        tokio::task::yield_now().await;
        assert_eq!(llm.call_count(), 0);
        assert!(conduit.events_after(0).is_empty());
    }
}
