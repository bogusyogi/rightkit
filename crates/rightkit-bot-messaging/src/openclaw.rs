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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_serde_roundtrip() {
        let msg = OpenClawMessage {
            role: "user".into(),
            content: "hello".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: OpenClawMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn usage_serde_roundtrip() {
        let usage = OpenClawUsage {
            input_tokens: 10,
            output_tokens: 20,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let parsed: OpenClawUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, usage);
    }

    #[test]
    fn request_serde_roundtrip() {
        let req = OpenClawRequest {
            model: "claude-3".into(),
            input: vec![OpenClawMessage {
                role: "user".into(),
                content: "what is 2+2?".into(),
            }],
            stream: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: OpenClawRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, req.model);
        assert_eq!(parsed.input.len(), req.input.len());
        assert_eq!(parsed.stream, req.stream);
    }

    #[test]
    fn response_serde_roundtrip() {
        let resp = OpenClawResponse {
            id: "resp-123".into(),
            output: vec![OpenClawMessage {
                role: "assistant".into(),
                content: "4".into(),
            }],
            usage: OpenClawUsage {
                input_tokens: 5,
                output_tokens: 1,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: OpenClawResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, resp.id);
        assert_eq!(parsed.output.len(), resp.output.len());
        assert_eq!(parsed.usage.input_tokens, resp.usage.input_tokens);
    }

    #[tokio::test]
    async fn handle_invokes_run_closure() {
        let req = OpenClawRequest {
            model: "test".into(),
            input: vec![OpenClawMessage {
                role: "user".into(),
                content: "test".into(),
            }],
            stream: false,
        };

        let response = handle_openclaw(req.clone(), |received_req| async move {
            assert_eq!(received_req.model, "test");
            assert_eq!(received_req.input.len(), 1);

            OpenClawResponse {
                id: "test-resp".into(),
                output: vec![OpenClawMessage {
                    role: "assistant".into(),
                    content: "processed".into(),
                }],
                usage: OpenClawUsage {
                    input_tokens: 5,
                    output_tokens: 10,
                },
            }
        })
        .await;

        assert_eq!(response.id, "test-resp");
        assert_eq!(response.output[0].content, "processed");
        assert_eq!(response.usage.output_tokens, 10);
    }

    #[tokio::test]
    async fn handle_with_multiple_messages() {
        let req = OpenClawRequest {
            model: "gpt-test".into(),
            input: vec![
                OpenClawMessage {
                    role: "user".into(),
                    content: "msg1".into(),
                },
                OpenClawMessage {
                    role: "assistant".into(),
                    content: "resp1".into(),
                },
                OpenClawMessage {
                    role: "user".into(),
                    content: "msg2".into(),
                },
            ],
            stream: true,
        };

        let response = handle_openclaw(req.clone(), |received_req| async move {
            assert_eq!(received_req.input.len(), 3);

            OpenClawResponse {
                id: "multi-resp".into(),
                output: vec![OpenClawMessage {
                    role: "assistant".into(),
                    content: "final answer".into(),
                }],
                usage: OpenClawUsage {
                    input_tokens: 50,
                    output_tokens: 25,
                },
            }
        })
        .await;

        assert_eq!(response.id, "multi-resp");
        assert_eq!(response.usage.input_tokens, 50);
    }
}
