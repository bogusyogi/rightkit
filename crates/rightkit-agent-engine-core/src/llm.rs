//! The LLM client abstraction.
//!
//! The agent loop depends only on this trait, so it can run against a real provider
//! (the `host_providers` clients, wrapped by the live fallback chain in `host-app/src/provider_client.rs`) or a scripted stub in tests.

use async_trait::async_trait;
use futures::Stream;
use rightkit_agent_coding_tools::ToolRegistry;
use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};
use std::pin::Pin;
use std::sync::Arc;

/// Provider-neutral identity for one model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRequestIdentity {
    pub session_id: String,
    pub task_id: String,
    pub request_id: String,
    pub generation: u64,
}

impl LlmRequestIdentity {
    pub fn new(
        session_id: impl Into<String>,
        task_id: impl Into<String>,
        request_id: impl Into<String>,
        generation: u64,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            task_id: task_id.into(),
            request_id: request_id.into(),
            generation,
        }
    }
}

/// Stable request metadata that provider adapters may consume without placing
/// volatile values in the transcript itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmRequestMetadata {
    pub request_identity: Option<LlmRequestIdentity>,
    pub stable_prefix_key: Option<String>,
    /// Per-call provider output ceiling. Used by compaction so its active-model
    /// summary cannot consume an unbounded/default generation allowance.
    pub max_output_tokens: Option<u32>,
    /// HCTX-002: generation of host-state projections promoted at the request boundary.
    pub provider_boundary_generation: Option<u64>,
    /// HCTX-002: fingerprint of the prompt/context/world-state projection used.
    pub context_fingerprint: Option<String>,
    /// Receives one notice per provider retry/backoff for this request.
    pub retry_notifier: rightkit_agent_engine_protocol::RetryNotifier,
}

/// A single model turn's output.
#[derive(Debug, Clone, Default)]
pub struct LlmResponse {
    /// Assistant prose (may be empty when the turn is purely tool calls).
    pub text: String,
    /// Optional chain-of-thought / reasoning text.
    pub reasoning: Option<String>,
    /// Tool calls the model wants to make this turn.
    pub tool_calls: Vec<ToolCall>,
    /// Provider finish reason, when available (for example `stop`, `tool_calls`,
    /// `content_filter`, or provider-specific safety strings).
    pub finish_reason: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
}

/// One observable item from a provider response stream.
///
/// `Response` keeps the compatibility seam lossless for existing clients;
/// real providers emit text, usage, and fully assembled tool calls separately
/// so dispatch can begin before generation ends.
#[derive(Debug, Clone)]
pub enum LlmStreamEvent {
    Response(LlmResponse),
    Text(String),
    Reasoning(String),
    ToolCall(ToolCall),
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    },
    /// Itemized detail for the preceding `Usage` event of the same response.
    /// `reasoning_tokens` is a subset of `completion_tokens`.
    UsageDetail {
        reasoning_tokens: u32,
    },
    /// A text-only output-limit stop. Consumers must persist accumulated
    /// output before acknowledging; provider continuation waits on this gate.
    OutputLimitCheckpoint {
        finish_reason: String,
        acknowledge: std::sync::Arc<tokio::sync::Notify>,
    },
    Finished(Option<String>),
}

pub type LlmResponseStream =
    Pin<Box<dyn Stream<Item = Result<LlmStreamEvent, String>> + Send + 'static>>;

impl LlmResponse {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: s.into(),
            ..Default::default()
        }
    }

    pub fn with_tool_call(mut self, call: ToolCall) -> Self {
        self.tool_calls.push(call);
        self
    }

    pub fn with_tokens(mut self, prompt: u32, completion: u32) -> Self {
        self.prompt_tokens = prompt;
        self.completion_tokens = completion;
        self
    }

    pub fn with_finish_reason(mut self, reason: impl Into<String>) -> Self {
        self.finish_reason = Some(reason.into());
        self
    }

    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

/// The contract the engine uses to talk to a model.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Produce the next assistant turn given the current transcript.
    async fn complete(&self, messages: &[TranscriptMessage]) -> Result<LlmResponse, String>;

    /// Metadata-aware request seam. Test doubles and non-provider clients keep
    /// the old method; provider clients override this to put stable cache keys
    /// onto supported wire requests.
    async fn complete_with_metadata(
        &self,
        messages: &[TranscriptMessage],
        _metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponse, String> {
        self.complete(messages).await
    }

    /// Stream a response. Default implementation preserves every existing
    /// test double and non-streaming client; the engine falls back to the
    /// interruptible completed-turn seam when this returns its sentinel error.
    async fn stream_with_metadata(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponseStream, String> {
        let _ = (messages, metadata);
        Err("streaming seam unavailable".into())
    }

    /// Operational image request budget for the active provider route. Unknown
    /// clients receive the smallest conservative profile.
    fn image_budget_policy(&self) -> crate::image_eviction::ImageBudgetPolicy {
        crate::image_eviction::ImageBudgetPolicy::conservative_unknown()
    }

    /// Count the request body used for multimodal eviction. Provider adapters
    /// should override this with their exact wire serializer.
    fn image_request_accounting(
        &self,
        messages: &[TranscriptMessage],
        _metadata: &LlmRequestMetadata,
    ) -> crate::image_eviction::ImageRequestAccounting {
        crate::image_eviction::ImageRequestAccounting {
            body_bytes: serde_json::to_vec(messages).map_or(usize::MAX, |body| body.len()),
            basis: crate::image_eviction::ImageAccountingBasis::TranscriptJsonFallback,
        }
    }

    /// Replace the canonical execution/provider tool registry at a named
    /// provider-turn boundary. Non-provider test doubles remain unchanged.
    fn replace_tool_registry(&self, _tools: Arc<ToolRegistry>) {}

    /// CR-33: the session's ONE promotion-state owner. Provider clients that
    /// accumulate `PromotedToolState` across turns (the same state they
    /// serialize onto the wire) must override this to return a clone of
    /// their live, accumulated snapshot — never a fresh
    /// `PromotedToolState::new()`. The engine's `CacheTracker` digest is
    /// built from whatever this returns, so a promotion recorded on the
    /// wire always produces the matching digest transition/reason here;
    /// recomputing from a new state object would silently desynchronize the
    /// two (a promotion changes the wire prefix with no corresponding
    /// CacheTracker transition, defeating invariant F). Non-provider clients
    /// (tests, stubs) never promote anything, so the empty default is exact
    /// for them, not just a placeholder.
    fn promoted_tool_state_snapshot(
        &self,
    ) -> rightkit_agent_coding_tools::registry::PromotedToolState {
        rightkit_agent_coding_tools::registry::PromotedToolState::new()
    }

    /// Identity of the model route the NEXT request will use (model id, or an
    /// opaque route fingerprint). The prompt-cache break detector compares it
    /// across requests so a live model switch is classified as an expected
    /// break instead of an unexplained one. Clients whose model cannot change
    /// mid-session return `None`.
    fn active_route_label(&self) -> Option<String> {
        None
    }
}

/// A scripted LLM for tests: returns a queued sequence of responses, one per call.
pub struct ScriptedLlm {
    responses: std::sync::Mutex<std::collections::VecDeque<LlmResponse>>,
    pub calls: std::sync::atomic::AtomicUsize,
}

impl ScriptedLlm {
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into_iter().collect()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmClient for ScriptedLlm {
    async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "ScriptedLlm: no more scripted responses".to_string())
    }
}
