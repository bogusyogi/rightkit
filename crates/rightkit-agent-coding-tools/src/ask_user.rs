//! `ask_user` — ask the operator a structured multiple-choice question
//! (Claude-Code `AskUserQuestion` parity).
//!
//! Binary tool-approval already exists, but the agent had no way to ask the
//! operator a *substantive* question — "which auth strategy?", "ship to staging
//! or prod?" — and get a structured answer back mid-run. This tool blocks the
//! turn, surfaces 1-4 questions (each with 2-4 labelled options) to whatever
//! driver is attached (desktop UI, web, Telegram), and returns the chosen
//! answers. Without a driver wired in it fails cleanly so a headless run isn't
//! left hanging.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

/// Told to the model on the no-driver and no-answer paths, so an
/// unattended run keeps going with a stated assumption instead of stalling.
const NO_ANSWER_GUIDANCE: &str =
    "Proceed with a sensible default and state the assumption you made.";

pub struct AskUserTool;

#[async_trait::async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn is_read_only(&self) -> bool {
        // Communication, not a workspace effect: never permission-gated. Runs in
        // the read batch and simply awaits the operator's answer.
        true
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        // Asking the operator is itself the consent surface; it must never be
        // preceded by a second "Approve request" card (D17: question card).
        Some(rightkit_agent_policy::CapabilityClass::ReadFile)
    }

    fn description(&self) -> &str {
        "Block and ask operator a structured question (1-4 questions, 2-4 options each; multiSelect for multi-choice). Use only for operator decisions, not facts you can find. Fail: headless/no questioner driver."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["questions"],
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "required": ["question", "header", "options"],
                        "properties": {
                            "question": { "type": "string" },
                            "header": { "type": "string", "description": "Short chip label (max ~12 chars)" },
                            "multiSelect": { "type": "boolean", "description": "Allow multiple selections (default false)" },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 4,
                                "items": {
                                    "type": "object",
                                    "required": ["label"],
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": { "type": "string" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let questions = match args["questions"].as_array() {
            Some(q) if !q.is_empty() => q,
            _ => {
                return ToolResult::error("`questions` must be a non-empty array (1-4 questions).")
            }
        };
        if questions.len() > 4 {
            return ToolResult::error("Too many questions (max 4).");
        }
        for (i, q) in questions.iter().enumerate() {
            if q["question"].as_str().unwrap_or("").trim().is_empty() {
                return ToolResult::error(format!("questions[{i}].question is required."));
            }
            match q["options"].as_array() {
                Some(opts) if (2..=4).contains(&opts.len()) => {
                    for (j, o) in opts.iter().enumerate() {
                        if o["label"].as_str().unwrap_or("").trim().is_empty() {
                            return ToolResult::error(format!(
                                "questions[{i}].options[{j}].label is required."
                            ));
                        }
                    }
                }
                _ => {
                    return ToolResult::error(format!(
                        "questions[{i}].options must have 2-4 entries."
                    ))
                }
            }
        }

        let questioner = match &ctx.questioner {
            Some(q) => q,
            None => {
                return ToolResult::error(format!(
                    "No operator is available to answer (headless run). {NO_ANSWER_GUIDANCE}"
                ))
            }
        };

        // Contract rule 1: no timeout. The wait ends on an answer, a client
        // detach or a cancelled turn (the questioner returns `Err`), never on a timer.
        match questioner.ask(json!(questions)).await {
            // Headless R8 / client-api R36: the host declines for an unattended turn with
            // `{declined: true, reason: "unattended"}`; the model is told nobody could answer.
            Ok(answers) if is_unattended_decline(&answers) => ToolResult::ok(format!(
                "The question was declined because no human is attached to answer it (unattended). \
                 Do not ask again. {NO_ANSWER_GUIDANCE}"
            )),
            Ok(answers) if is_declined(&answers) => ToolResult::ok(
                "The user declined to answer; no option was chosen. Do not ask these questions \
                 again. Proceed with a sensible default and state the assumption you made.",
            ),
            Ok(answers) => ToolResult::ok(render_answers(questions, &answers)),
            Err(e) => ToolResult::error(format!(
                "Question was not answered: {e}. {NO_ANSWER_GUIDANCE}"
            )),
        }
    }
}

/// `{"__declined": true}`: the marker for a declined question (at its index)
/// or, at the top level, for the whole call.
fn is_declined(value: &Value) -> bool {
    value.get("__declined").and_then(Value::as_bool) == Some(true)
        || value.get("declined").and_then(Value::as_bool) == Some(true)
}

/// A whole-call decline made because no human could answer.
fn is_unattended_decline(value: &Value) -> bool {
    is_declined(value) && value.get("reason").and_then(Value::as_str) == Some("unattended")
}

/// Render the operator's answers as readable text for the transcript.
/// `answers` is an object keyed by question index (R40: never by header, which
/// two questions can share); each value is a string, an array of strings, or
/// the per-question declined marker.
fn render_answers(questions: &[Value], answers: &Value) -> String {
    let mut out = Vec::new();
    for (i, q) in questions.iter().enumerate() {
        let header = q["header"].as_str().unwrap_or("");
        let rendered = match answers.get(i.to_string()) {
            Some(value) if is_declined(value) => "declined".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            Some(other) => other.to_string(),
            None => "(no answer)".to_string(),
        };
        let label = if header.is_empty() {
            q["question"].as_str().unwrap_or("Q")
        } else {
            header
        };
        out.push(format!("{label}: {rendered}"));
    }
    out.join("\n")
}
