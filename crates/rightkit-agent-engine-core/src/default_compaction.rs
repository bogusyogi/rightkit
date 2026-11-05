//! Default LLM-backed compaction summarizer.
//!
//! The engine wires this from `SessionEngine::new()` so long sessions keep a
//! model-written summary of dropped transcript windows by default.
//!
//! ## S:E2 -- compaction summarizer stage isolation (RT02 binding leg)
//!
//! [`WorkflowStageBindingV1`] carries `primary`/`fallbacks` model-route slugs
//! separate from the chat turn's model, but until this seam existed nothing
//! ever consulted them: `build_default_summarizer` simply reused the caller's
//! chat `llm` for every summarization call, so the "compaction has its own
//! pack route" contract was declared and never enforced. Donor pattern: pi
//! `packages/coding-agent/src/core/compaction/{compaction.ts:completeSummarization,
//! branch-summarization.ts}` (retry via `packages/ai/src/utils/retry.ts:retryAssistantCall`) -- a retry-wrapped standalone summary request
//! issued against its own route, never the main chat cache/model.
//!
//! [`CompactionModelRouteResolver`] is the seam the orchestrator injects a
//! real provider-catalog resolver into (see the wiring note below). When
//! present, [`ActiveModelCompactionSummarizer`] resolves `binding.primary`
//! first and walks `binding.fallbacks` in order on resolution or summarizer
//! failure, exactly mirroring pi's retry-wrapped fallback chain. Only when no
//! resolver is injected -- or every slug in the chain fails to resolve -- does
//! it fall back to the passed-in chat `llm`, which keeps every existing
//! caller (including every test in this module) fail-closed-safe rather than
//! erroring outright when the orchestrator has not wired a resolver yet.

use crate::compactor::{
    format_message_for_summarizer, CompactError, CompactionUsage, FallibleSummarizer,
};
use crate::llm::{LlmClient, LlmRequestMetadata};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Resolves a compaction-stage model-route slug (`WorkflowStageBindingV1::primary`
/// or one of its `fallbacks`) to a live [`LlmClient`] bound to that route.
///
/// The orchestrator wires a real implementation over the provider catalog
/// (the same resolver `main_runtime_server`/`api_state_impl_02` use to build
/// the chat-turn `LlmClient`) into `build_default_summarizer`; this crate
/// only defines the seam so `default_compaction.rs` never needs to depend on
/// the provider crate directly.
#[async_trait::async_trait]
pub trait CompactionModelRouteResolver: Send + Sync {
    /// Resolve `slug` to a client for that model route, or an error string if
    /// the slug is unknown, unhealthy, or the route is currently unqualified.
    async fn resolve(&self, slug: &str) -> Result<Arc<dyn LlmClient>, String>;
}

/// Versioned assignment for the compaction workflow stage. It is separate
/// from the chat model binding and carries its own budget, output ceiling,
/// health rule, and fallback chain. The provider resolver validates these
/// slugs against the authoritative live catalog before constructing the
/// `LlmClient` supplied here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkflowStageBindingV1 {
    pub schema_version: u32,
    pub stage: &'static str,
    pub primary: &'static str,
    pub fallbacks: &'static [&'static str],
    pub context_budget_tokens: u64,
    pub max_output_tokens: u32,
    pub health_policy: &'static str,
    pub receipt_required: bool,
}

pub(crate) const fn compaction_stage_binding() -> WorkflowStageBindingV1 {
    WorkflowStageBindingV1 {
        schema_version: 1,
        stage: "compaction",
        primary: "deepseek/deepseek-v4-flash",
        fallbacks: &["stepfun/step-3.7-flash", "openai/gpt-5.6"],
        context_budget_tokens: 120_000,
        max_output_tokens: 20_000,
        health_policy: "qualified_healthy_only",
        receipt_required: true,
    }
}

const DEFAULT_COMPACTION_PROMPT: &str = "\
You are compacting a long coding-assistant transcript so it fits the context window.

<analysis>
First reason through the transcript before writing the summary. Identify the user's
active goal and constraints; decisions made and the reasoning; files and paths
touched and the key changes; tool results that matter (errors, test output,
command results, diffs); verification state; safety constraints; open questions;
and the exact next step the assistant should take. Preserve user instructions
needed for continuity, but clearly distinguish superseded requests from active
work. Use this scratchpad to resolve ordering and importance, but do not include
scratchpad prose in the final answer.
</analysis>

<summary>
Write a dense, factual summary that preserves the analysis above. Drop
pleasantries and redundant detail. Never invent facts.
Output only the summary text, with no XML tags, preamble, or mention that
compaction happened.
</summary>";

/// System prompt for the summarizer: the analysis/summary protocol plus the
/// structured session-state schema ([`SessionStateSummary`]) so every
/// compaction (and later handoff) produces the same sections in the same order.
fn compaction_system_prompt() -> String {
    format!(
        "{DEFAULT_COMPACTION_PROMPT}

{}",
        crate::session_state_summary::SessionStateSummary::prompt_schema()
    )
}

/// Bounded retries keep summary generation isolated from the turn loop.
const MAX_COMPACTION_ATTEMPTS: usize = 3;

struct ActiveModelCompactionSummarizer {
    /// The chat-turn model, kept only as the fail-closed-safe fallback when
    /// no route resolver is injected or every binding slug fails to resolve.
    llm: Arc<dyn LlmClient>,
    /// S:E2 seam: when present, `binding.primary`/`binding.fallbacks` are
    /// actually consulted to route the summarizer call to its own pack
    /// route instead of silently reusing `llm`.
    route_resolver: Option<Arc<dyn CompactionModelRouteResolver>>,
    usage: Mutex<Option<CompactionUsage>>,
    max_output_tokens: AtomicU32,
    binding: WorkflowStageBindingV1,
}

impl ActiveModelCompactionSummarizer {
    /// Ordered candidate slugs for this binding: primary, then each
    /// fallback in order -- mirrors pi's `completeSimpleWithRetries` chain.
    fn candidate_slugs(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.binding.primary).chain(self.binding.fallbacks.iter().copied())
    }

    /// Resolve the client to summarize with for retry attempt `attempt`
    /// (0-indexed): walks the binding's primary/fallback slugs through the
    /// injected resolver in order, advancing one slug per attempt so a
    /// summarizer failure on the primary route retries against the next
    /// fallback rather than hammering the same route -- mirroring pi's
    /// `completeSimpleWithRetries` fallback chain. Falls back to the
    /// chat-turn client once no resolver is wired, the attempt index runs
    /// past the last configured slug, or every remaining slug fails to
    /// resolve.
    async fn resolve_client(&self, attempt: usize) -> Arc<dyn LlmClient> {
        if let Some(resolver) = self.route_resolver.as_ref() {
            for slug in self.candidate_slugs().skip(attempt) {
                match resolver.resolve(slug).await {
                    Ok(client) => return client,
                    Err(_error) => continue,
                }
            }
        }
        self.llm.clone()
    }
}

#[async_trait::async_trait]
impl FallibleSummarizer for ActiveModelCompactionSummarizer {
    async fn try_summarize(
        &self,
        messages: &[rightkit_agent_engine_protocol::TranscriptMessage],
        prior_summary: Option<&str>,
    ) -> Result<String, CompactError> {
        *self.usage.lock().unwrap() = None;
        let mut formatted = String::new();
        if let Some(prior) = prior_summary {
            formatted.push_str("Prior summary:\n");
            formatted.push_str(prior);
            formatted.push_str("\n\nNew messages to integrate:\n");
        }
        for message in messages {
            formatted.push_str(&format_message_for_summarizer(message));
            formatted.push('\n');
        }
        let mut request = vec![
            rightkit_agent_engine_protocol::TranscriptMessage::System {
                id: "compaction-system".into(),
                created_at: rightkit_agent_engine_protocol::now_iso(),
                content: compaction_system_prompt(),
                pinned: false,
            },
            rightkit_agent_engine_protocol::TranscriptMessage::User {
                id: "compaction-input".into(),
                created_at: rightkit_agent_engine_protocol::now_iso(),
                content: formatted,
                trust: None,
                pinned: false,
                attachments: Vec::new(),
            },
        ];
        let max_output_tokens = self.max_output_tokens.load(Ordering::Relaxed);
        let binding_limit = self.binding.max_output_tokens;
        let effective_max_output_tokens = if max_output_tokens == 0 {
            binding_limit
        } else {
            max_output_tokens.min(binding_limit)
        };
        let metadata = LlmRequestMetadata {
            // Standalone stage request: never reuse the chat turn's cache key.
            stable_prefix_key: None,
            max_output_tokens: Some(effective_max_output_tokens),
            ..LlmRequestMetadata::default()
        };
        let mut last_error = None;
        let mut schema_repair_attempted = false;
        for attempt in 0..MAX_COMPACTION_ATTEMPTS {
            // Re-resolve per attempt so a route that fails to resolve (or a
            // summarizer error below) walks to the next fallback slug on
            // retry, matching pi's retry-wrapped fallback chain rather than
            // pinning one client for the whole call.
            let client = self.resolve_client(attempt).await;
            let response = match client.complete_with_metadata(&request, &metadata).await {
                Ok(response) => response,
                Err(error) => {
                    if last_error.is_none() {
                        last_error = Some(CompactError::Summarizer(error));
                    }
                    continue;
                }
            };
            let validation_error = if !response.tool_calls.is_empty() {
                Some("compaction model returned tool calls instead of a summary")
            } else if matches!(
                response.finish_reason.as_deref(),
                Some("length" | "max_tokens" | "max_output_tokens")
            ) {
                Some("compaction summary was truncated by the active model")
            } else if response.text.trim().is_empty() {
                Some("active model returned an empty compaction summary")
            } else {
                None
            };
            if let Some(error) = validation_error {
                last_error = Some(CompactError::Summarizer(error.into()));
                continue;
            }
            if let Err(error) =
                crate::session_state_summary::SessionStateSummary::parse_and_validate(
                    &response.text,
                )
            {
                if schema_repair_attempted {
                    return Err(CompactError::Summarizer(format!(
                        "structured session summary invalid after repair: {error}"
                    )));
                }
                schema_repair_attempted = true;
                request.push(rightkit_agent_engine_protocol::TranscriptMessage::User {
                    id: "compaction-schema-repair".into(),
                    created_at: rightkit_agent_engine_protocol::now_iso(),
                    content: format!("Repair previous output once. Return every required section exactly once in the prescribed order. Validation error: {error}"),
                    trust: None,
                    pinned: false,
                    attachments: Vec::new(),
                });
                continue;
            }
            *self.usage.lock().unwrap() = Some(CompactionUsage {
                prompt_tokens: response.prompt_tokens,
                completion_tokens: response.completion_tokens,
                cache_read_tokens: response.cache_read_tokens,
                cache_creation_tokens: response.cache_creation_tokens,
            });
            return Ok(response.text.trim().to_string());
        }
        Err(last_error.expect("compaction attempts must record an error"))
    }

    fn take_usage(&self) -> Option<CompactionUsage> {
        self.usage.lock().unwrap().take()
    }

    fn set_max_output_tokens(&self, tokens: Option<u32>) {
        self.max_output_tokens
            .store(tokens.unwrap_or(0), Ordering::Relaxed);
    }
}

/// S:E2: default summarizer that actually consults
/// `WorkflowStageBindingV1::primary`/`fallbacks` through `route_resolver`
/// before falling back to the passed-in chat-turn `llm`. This is the
/// constructor the orchestrator's `SessionEngine::new` must call once it
/// injects a real resolver over the provider catalog.
pub(crate) fn build_default_summarizer_with_route_resolver(
    llm: Arc<dyn LlmClient>,
    route_resolver: Option<Arc<dyn CompactionModelRouteResolver>>,
) -> Arc<dyn FallibleSummarizer> {
    Arc::new(ActiveModelCompactionSummarizer {
        llm,
        route_resolver,
        usage: Mutex::new(None),
        max_output_tokens: AtomicU32::new(0),
        binding: compaction_stage_binding(),
    })
}
