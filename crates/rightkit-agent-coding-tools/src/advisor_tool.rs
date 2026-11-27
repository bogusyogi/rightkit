//! The opt-in `advisor` tool (A225).
//!
//! The working model may ask a stronger model for a verdict on its own work: it calls
//! `advisor {question}`, the host sends the session transcript (bounded) and the question to
//! its configured advisor model and the answer comes back as the tool result. The call is one
//! paid completion the model itself chooses, so the tool exists only when the host's advisor
//! feature is on (the default is off) and the host registers it with a backend; a session without
//! the feature never sees it, and a call without a backend is refused, never answered locally.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

/// Longest question the tool forwards, in characters.
pub const MAX_QUESTION_CHARS: usize = 4_000;
/// Longest transcript the backend is shown, in characters: the head and the tail are kept.
pub const MAX_TRANSCRIPT_CHARS: usize = 120_000;

const CUT_MARKER: &str = "\n[... middle of the transcript omitted ...]\n";

/// `text` cut to `cap` characters, keeping the beginning and the end around a marker.
pub fn bound_transcript(text: &str, cap: usize) -> String {
    let total = text.chars().count();
    if total <= cap {
        return text.to_string();
    }
    let keep = cap.saturating_sub(CUT_MARKER.chars().count());
    let head = keep / 4;
    let tail = keep - head;
    let head_text: String = text.chars().take(head).collect();
    let tail_text: String = text.chars().skip(total - tail).collect();
    format!("{head_text}{CUT_MARKER}{tail_text}")
}

/// The `advisor` tool: one question to a stronger model through the host's
/// [`AdvisorBackend`](crate::context::AdvisorBackend).
pub struct AdvisorTool;

#[async_trait::async_trait]
impl Tool for AdvisorTool {
    fn name(&self) -> &str {
        "advisor"
    }

    fn description(&self) -> &str {
        "Ask a stronger model for a verdict on your work so far. Send one specific question (for example whether a plan or a change is sound). The reviewer sees the conversation so far and your question and answers in text; it takes no action. Each call is one paid request, so use it for a decision that matters, not for routine steps."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["question"],
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The specific question to put to the advisor."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    /// The call leaves the machine and costs money, so it is gated like any other outbound call.
    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::Network)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let question = match args.get("question").and_then(Value::as_str).map(str::trim) {
            Some(text) if !text.is_empty() => text,
            _ => return ToolResult::error("advisor: 'question' is required"),
        };
        let Some((backend, transcript)) = ctx.advisor.as_ref() else {
            return ToolResult::error(
                "advisor is not available: it needs `[features] advisor = true` and `[orchestration] advisor-model` to name a model",
            );
        };
        let question: String = question.chars().take(MAX_QUESTION_CHARS).collect();
        match backend
            .advise(question, bound_transcript(transcript, MAX_TRANSCRIPT_CHARS))
            .await
        {
            Ok(verdict) if verdict.trim().is_empty() => {
                ToolResult::error("advisor: the model returned no answer")
            }
            Ok(verdict) => ToolResult::ok(verdict.trim().to_string()),
            Err(error) => ToolResult::error(format!("advisor failed: {error}")),
        }
    }
}
