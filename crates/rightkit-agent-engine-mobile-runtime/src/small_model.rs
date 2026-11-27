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
    parse_small_model_output, ConfidenceBucket, SmallModelBudget, SmallModelClient,
    SmallModelError, SmallModelInput, SmallModelOutput, SmallModelTask,
    DEFAULT_CALL_TOKEN_ESTIMATE,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// The assist transport of one session: a plain client for free-text tasks and
/// the provider route to build a schema-bound client from for structured ones.
pub struct MobileSmallModelClient {
    config: ProviderConfig,
    plain: Arc<dyn LlmClient>,
}

/// Build the assist client for `config`. A bad route fails here, not on the
/// first call.
pub fn client_for(config: &ProviderConfig) -> Result<Arc<dyn SmallModelClient>, RuntimeError> {
    // The assist client sends no tool schemas. It also rejects unexpected tool
    // calls before returning a typed output.
    Ok(Arc::new(MobileSmallModelClient {
        config: config.clone(),
        plain: llm::assist_client_for(config)?,
    }))
}

/// ENG-84: JSON Schema for each task's answer; `None` for free-text tasks, and
/// for every task unless the host turned on
/// [`crate::host_options::set_small_model_structured_output`].
fn response_schema(task: &SmallModelTask) -> Option<Value> {
    if !crate::host_options::small_model_structured_output() {
        return None;
    }
    match task {
        SmallModelTask::SessionFollowUp => Some(json!({
            "type": "object",
            "properties": {"followUp": {"type": "string"}},
            "required": ["followUp"],
            "additionalProperties": false
        })),
        SmallModelTask::Classify { labels } => Some(json!({
            "type": "object",
            "properties": {"label": {"type": "string", "enum": labels}},
            "required": ["label"],
            "additionalProperties": false
        })),
        SmallModelTask::ProgressAudit => Some(json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["ON_TRACK", "BLOCKED"]},
                "summary": {"type": "string"}
            },
            "required": ["status", "summary"],
            "additionalProperties": false
        })),
        SmallModelTask::Recap | SmallModelTask::DiffNarration => None,
    }
}

/// Parse a structured answer; `None` when the provider ignored the schema and
/// returned plain text, so the caller falls back to `parse_small_model_output`.
fn parse_structured(task: &SmallModelTask, text: &str) -> Option<SmallModelOutput> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let object = value.as_object()?;
    match task {
        SmallModelTask::SessionFollowUp => {
            let follow_up = object.get("followUp")?.as_str()?.trim();
            Some(SmallModelOutput::SessionFollowUp {
                suggestion: follow_up.to_string(),
            })
        }
        SmallModelTask::Classify { labels } => {
            let label = object.get("label")?.as_str()?.trim();
            let matched = labels
                .iter()
                .find(|candidate| candidate.eq_ignore_ascii_case(label))?;
            Some(SmallModelOutput::Classify {
                label: matched.clone(),
                confidence_bucket: ConfidenceBucket::High,
            })
        }
        SmallModelTask::ProgressAudit => {
            let status = object.get("status")?.as_str()?.trim();
            let on_track = if status.eq_ignore_ascii_case("ON_TRACK") {
                true
            } else if status.eq_ignore_ascii_case("BLOCKED") {
                false
            } else {
                return None;
            };
            let summary = object.get("summary")?.as_str()?.trim().to_string();
            Some(SmallModelOutput::ProgressAudit {
                on_track,
                summary,
                blockers: Vec::new(),
            })
        }
        SmallModelTask::Recap | SmallModelTask::DiffNarration => None,
    }
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
        let schema = response_schema(&task);
        let llm = match &schema {
            Some(schema) => {
                llm::assist_client_with_response_format(&self.config, Some(schema.clone()))
                    .map_err(|error| SmallModelError::ProviderError(error.to_string()))?
            }
            None => self.plain.clone(),
        };
        let response = llm
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
        if schema.is_some() {
            if let Some(output) = parse_structured(&task, &response.text) {
                return Ok(output);
            }
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
