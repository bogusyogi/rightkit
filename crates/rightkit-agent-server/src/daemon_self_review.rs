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
