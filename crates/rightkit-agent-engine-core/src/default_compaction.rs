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

/// Default fail-closed summarizer built from the live session model, with no
/// compaction-stage route resolver wired. Kept for callers that have not
/// (yet) injected a [`CompactionModelRouteResolver`] -- behavior is
/// unchanged from before S:E2: the chat-turn model writes every summary.
///
/// `SessionEngine::new` should migrate to
/// [`build_default_summarizer_with_route_resolver`] once the orchestrator
/// wires a provider-catalog resolver through the hub (see the module-level
/// wiring note); this function remains for compatibility until that lands.
#[cfg(test)]
pub(crate) fn build_default_summarizer(llm: Arc<dyn LlmClient>) -> Arc<dyn FallibleSummarizer> {
    build_default_summarizer_with_route_resolver(llm, None)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmResponse, ScriptedLlm};

    fn structured(goal: &str) -> String {
        crate::session_state_summary::SessionStateSummary {
            goal: goal.into(),
            current_state: "complete".into(),
            ..Default::default()
        }
        .render_markdown()
        .trim()
        .to_string()
    }

    #[tokio::test]
    async fn default_summarizer_is_llm_backed_not_statistical() {
        let llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(structured(
            "MODEL COMPACTION OUTPUT",
        ))]));
        let summarizer = build_default_summarizer(llm);
        let msgs = vec![rightkit_agent_engine_protocol::TranscriptMessage::User {
            id: "u".into(),
            created_at: "t".into(),
            content: "do the thing".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }];

        let out = summarizer.try_summarize(&msgs, None).await.unwrap();

        assert_eq!(out, structured("MODEL COMPACTION OUTPUT"));
        assert!(!out.starts_with("Summary of "));
    }

    #[test]
    fn default_prompt_requires_analysis_and_continuity_without_fake_reservation() {
        assert!(DEFAULT_COMPACTION_PROMPT.contains("<analysis>"));
        assert!(DEFAULT_COMPACTION_PROMPT.contains("</analysis>"));
        assert!(DEFAULT_COMPACTION_PROMPT.contains("<summary>"));
        assert!(DEFAULT_COMPACTION_PROMPT.contains("</summary>"));
        assert!(!DEFAULT_COMPACTION_PROMPT.contains("20000"));
        assert!(DEFAULT_COMPACTION_PROMPT.contains("exact next step"));
        assert!(DEFAULT_COMPACTION_PROMPT.contains("verification state"));
        assert!(DEFAULT_COMPACTION_PROMPT.contains("safety constraints"));
        assert!(DEFAULT_COMPACTION_PROMPT.contains("superseded requests"));
    }

    #[test]
    fn compaction_system_prompt_carries_the_session_state_schema() {
        let prompt = compaction_system_prompt();
        assert!(prompt.starts_with(DEFAULT_COMPACTION_PROMPT));
        for section in crate::session_state_summary::SESSION_STATE_SECTIONS {
            assert!(prompt.contains(&format!("## {section}")), "{section}");
        }
    }

    #[test]
    fn compaction_has_a_versioned_workflow_binding_independent_from_chat_defaults() {
        let binding = compaction_stage_binding();
        assert_eq!(binding.schema_version, 1);
        assert_eq!(binding.stage, "compaction");
        assert!(!binding.fallbacks.is_empty());
        assert!(binding.receipt_required);
        assert_eq!(binding.health_policy, "qualified_healthy_only");
    }

    #[tokio::test]
    async fn truncated_model_summary_fails_closed() {
        let llm = Arc::new(ScriptedLlm::new(vec![
            LlmResponse::text("partial summary").with_finish_reason("length")
        ]));
        let summarizer = build_default_summarizer(llm);
        let error = summarizer
            .try_summarize(
                &[rightkit_agent_engine_protocol::TranscriptMessage::User {
                    id: "u".into(),
                    created_at: "t".into(),
                    content: "continue".into(),
                    trust: None,
                    pinned: false,
                    attachments: Vec::new(),
                }],
                None,
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("truncated"));
    }

    #[tokio::test]
    async fn unusable_summary_is_retried_inside_stage_budget() {
        let llm = Arc::new(ScriptedLlm::new(vec![
            LlmResponse::text("partial").with_finish_reason("length"),
            LlmResponse::text(structured("complete summary")),
        ]));
        let summarizer = build_default_summarizer(llm.clone());
        let message = rightkit_agent_engine_protocol::TranscriptMessage::User {
            id: "u".into(),
            created_at: "t".into(),
            content: "continue".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        };

        assert_eq!(
            summarizer.try_summarize(&[message], None).await.unwrap(),
            structured("complete summary")
        );
        assert_eq!(llm.call_count(), 2);
    }

    #[tokio::test]
    async fn invalid_schema_gets_one_repair_then_validates_before_return() {
        let llm = Arc::new(ScriptedLlm::new(vec![
            LlmResponse::text("missing sections"),
            LlmResponse::text(structured("repaired")),
        ]));
        let summarizer = build_default_summarizer(llm.clone());
        let message = rightkit_agent_engine_protocol::TranscriptMessage::User {
            id: "u".into(),
            created_at: "t".into(),
            content: "continue".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        };
        let output = summarizer.try_summarize(&[message], None).await.unwrap();
        assert_eq!(output, structured("repaired"));
        assert_eq!(
            llm.call_count(),
            2,
            "schema repair is attempted exactly once"
        );
    }

    /// Fake resolver that returns a distinct scripted client per known slug
    /// and records which slugs were asked for, so tests can assert the
    /// summarizer actually consulted `binding.primary`/`fallbacks` rather
    /// than silently reusing the chat-turn client.
    struct FakeRouteResolver {
        routes: std::collections::HashMap<&'static str, Arc<ScriptedLlm>>,
        requested: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl CompactionModelRouteResolver for FakeRouteResolver {
        async fn resolve(&self, slug: &str) -> Result<Arc<dyn LlmClient>, String> {
            self.requested.lock().unwrap().push(slug.to_string());
            self.routes
                .get(slug)
                .cloned()
                .map(|client| client as Arc<dyn LlmClient>)
                .ok_or_else(|| format!("no route configured for {slug}"))
        }
    }

    #[tokio::test]
    async fn bound_summarizer_uses_pack_route_not_chat_model() {
        let binding = compaction_stage_binding();
        let chat_llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(
            "CHAT MODEL MUST NOT BE CALLED",
        )]));
        let pack_route_llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(structured(
            "PACK ROUTE OUTPUT",
        ))]));
        let mut routes = std::collections::HashMap::new();
        routes.insert(binding.primary, pack_route_llm.clone());
        let resolver = Arc::new(FakeRouteResolver {
            routes,
            requested: Mutex::new(Vec::new()),
        });

        let summarizer =
            build_default_summarizer_with_route_resolver(chat_llm.clone(), Some(resolver.clone()));
        let msgs = vec![rightkit_agent_engine_protocol::TranscriptMessage::User {
            id: "u".into(),
            created_at: "t".into(),
            content: "do the thing".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }];

        let out = summarizer.try_summarize(&msgs, None).await.unwrap();

        assert_eq!(out, structured("PACK ROUTE OUTPUT"));
        assert_eq!(pack_route_llm.call_count(), 1);
        assert_eq!(
            chat_llm.call_count(),
            0,
            "chat-turn model must not be called once a pack route resolves"
        );
        assert_eq!(
            resolver.requested.lock().unwrap().as_slice(),
            [binding.primary]
        );
    }

    #[tokio::test]
    async fn bound_summarizer_falls_back_through_binding_chain_on_retry() {
        let binding = compaction_stage_binding();
        assert!(
            !binding.fallbacks.is_empty(),
            "test requires at least one configured fallback slug"
        );
        let first_fallback = binding.fallbacks[0];
        let chat_llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(
            "CHAT MODEL MUST NOT BE CALLED",
        )]));
        // Primary route resolves but its model returns an unusable
        // (truncated) summary, so retry must walk to the first fallback slug
        // rather than hammering the primary route again.
        let primary_llm = Arc::new(ScriptedLlm::new(vec![
            LlmResponse::text("partial").with_finish_reason("length")
        ]));
        let fallback_llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(structured(
            "FALLBACK ROUTE OUTPUT",
        ))]));
        let mut routes = std::collections::HashMap::new();
        routes.insert(binding.primary, primary_llm.clone());
        routes.insert(first_fallback, fallback_llm.clone());
        let resolver = Arc::new(FakeRouteResolver {
            routes,
            requested: Mutex::new(Vec::new()),
        });

        let summarizer =
            build_default_summarizer_with_route_resolver(chat_llm.clone(), Some(resolver.clone()));
        let msgs = vec![rightkit_agent_engine_protocol::TranscriptMessage::User {
            id: "u".into(),
            created_at: "t".into(),
            content: "continue".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }];

        let out = summarizer.try_summarize(&msgs, None).await.unwrap();

        assert_eq!(out, structured("FALLBACK ROUTE OUTPUT"));
        assert_eq!(primary_llm.call_count(), 1);
        assert_eq!(fallback_llm.call_count(), 1);
        assert_eq!(chat_llm.call_count(), 0);
        let requested = resolver.requested.lock().unwrap();
        assert_eq!(requested[0], binding.primary);
        assert_eq!(requested[1], first_fallback);
    }
}
