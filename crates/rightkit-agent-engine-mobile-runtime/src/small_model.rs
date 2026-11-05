//! Tool-free small-model transport for mobile orchestration assists.
//!
//! It shares a checked session provider route, but builds an isolated client
//! with no usable tool context. Recaps, goal audits, & diff walkthroughs thus
//! cannot dispatch tools or mutate a workspace.

use crate::{llm, ProviderConfig, RuntimeError};
use async_trait::async_trait;
use rightkit_agent_engine_core::llm::{LlmClient, LlmRequestMetadata};
use rightkit_agent_engine_protocol::{now_iso, TranscriptMessage};
use rightkit_agent_orchestration::coding::small_model_utility::{
    parse_small_model_output, SmallModelBudget, SmallModelClient, SmallModelError, SmallModelInput,
    SmallModelOutput, SmallModelTask, DEFAULT_CALL_TOKEN_ESTIMATE,
};
use std::sync::Arc;

pub struct MobileSmallModelClient {
    llm: Arc<dyn LlmClient>,
}

pub fn client_for(config: &ProviderConfig) -> Result<Arc<dyn SmallModelClient>, RuntimeError> {
    // The assist client sends no schemas. It also rejects unexpected tool
    // calls before returning a typed output.
    Ok(Arc::new(MobileSmallModelClient {
        llm: llm::assist_client_for(config)?,
    }))
}

#[async_trait]
impl SmallModelClient for MobileSmallModelClient {
    async fn run(
        &self,
        task: SmallModelTask,
        input: &SmallModelInput,
        budget: &SmallModelBudget,
    ) -> Result<SmallModelOutput, SmallModelError> {
        if !budget.can_afford(DEFAULT_CALL_TOKEN_ESTIMATE) {
            return Err(SmallModelError::BudgetExceeded);
        }
        let response = self
            .llm
            .complete_with_metadata(
                &[
                    TranscriptMessage::System {
                        id: "mobile-small-model-system".to_string(),
                        created_at: now_iso(),
                        content: instruction(&task),
                        pinned: true,
                    },
                    TranscriptMessage::User {
                        id: "mobile-small-model-input".to_string(),
                        created_at: now_iso(),
                        content: input.content.clone(),
                        trust: None,
                        pinned: false,
                        attachments: Vec::new(),
                    },
                ],
                &LlmRequestMetadata {
                    max_output_tokens: Some(DEFAULT_CALL_TOKEN_ESTIMATE),
                    ..Default::default()
                },
            )
            .await
            .map_err(SmallModelError::ProviderError)?;
        if !response.tool_calls.is_empty() {
            return Err(SmallModelError::ProviderError(
                "small-model assist returned an unexpected tool call".to_string(),
            ));
        }
        Ok(parse_small_model_output(task, response.text))
    }
}

fn instruction(task: &SmallModelTask) -> String {
    match task {
        SmallModelTask::Recap => "Summarize supplied session content in concise plain language. Do not call tools or claim work not present in input.".to_string(),
        SmallModelTask::ProgressAudit => "Audit supplied goal progress only. First line must be ON_TRACK or BLOCKED. Follow with concise summary, then each concrete blocker on its own '- ' line. Do not call tools or perform work.".to_string(),
        SmallModelTask::DiffNarration => "Explain supplied unified-diff hunk in concise plain language. Do not call tools, propose unrelated changes, or claim verification.".to_string(),
        SmallModelTask::SessionFollowUp => "Return exactly one concise, user-controlled next instruction based only on supplied recap and pinned project knowledge. Do not call tools, execute work, or invent facts.".to_string(),
        SmallModelTask::Classify { labels } => format!("Return exactly one label from this list: {}. Do not call tools.", labels.join(", ")),
    }
}
