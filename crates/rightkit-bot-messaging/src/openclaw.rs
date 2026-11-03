//! OpenAI-compatible surface — Responses API shape.
//!
//! Defines request/response types for an OpenAI-compatible endpoint that adapts to the
//! agentic Responses API pattern (multi-turn agent execution). The actual agent execution
//! is injected via a run-closure to avoid crate cycles with the engine.

use serde::{Deserialize, Serialize};

/// A message in the OpenClaw request/response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenClawMessage {
    /// The role of the speaker: "user", "assistant", etc.
    pub role: String,
    /// The content of the message.
    pub content: String,
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenClawUsage {
    /// Number of input tokens consumed.
    pub input_tokens: u32,
    /// Number of output tokens produced.
    pub output_tokens: u32,
}

/// An OpenClaw request matching the Responses API shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawRequest {
    /// The model identifier.
    pub model: String,
    /// Conversation history and user input.
    pub input: Vec<OpenClawMessage>,
    /// Whether to stream the response.
    pub stream: bool,
}

/// An OpenClaw response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawResponse {
    /// Unique response identifier.
    pub id: String,
    /// The assistant's output messages.
    pub output: Vec<OpenClawMessage>,
    /// Token usage for this response.
    pub usage: OpenClawUsage,
}

/// Handle an OpenClaw request by invoking an injected run-closure.
///
/// The run-closure receives the request and returns a future that completes with the response.
/// This design avoids circular dependencies between the remote crate and the engine.
///
/// WebSocket execution mode: When used in a WebSocket context with streaming enabled,
/// the latency is determined by the backend's real-time execution. The daemon typically
/// streams deltas incrementally, which the relay can forward as they arrive. Non-streaming
/// mode waits for the full response before returning.
pub async fn handle_openclaw<F, Fut>(req: OpenClawRequest, run: F) -> OpenClawResponse
where
    F: Fn(OpenClawRequest) -> Fut,
    Fut: std::future::Future<Output = OpenClawResponse>,
{
    run(req).await
}
