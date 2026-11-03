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

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

/// How long to wait for an operator before giving up.
///
/// "Without a driver wired in it fails cleanly" only covers a *missing*
/// questioner. A driver that is attached but has nobody behind it — an API-driven
/// or otherwise unattended session — left `ask()` awaiting forever, wedging the
/// turn with no reply and no report. Mirrors `APPROVAL_WAIT_TIMEOUT` on the
/// sub-agent approval path, which had the identical defect.
const ANSWER_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Told to the model on both the no-driver and the timed-out paths, so an
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

        match tokio::time::timeout(ANSWER_WAIT_TIMEOUT, questioner.ask(json!(questions))).await {
            Ok(Ok(answers)) if answers.get("__declined").and_then(Value::as_bool) == Some(true) => {
                ToolResult::ok(
                    "The user declined to answer this question. Do not ask it again; \
                     proceed with a sensible default and state the assumption you made.",
                )
            }
            Ok(Ok(answers)) => ToolResult::ok(render_answers(questions, &answers)),
            Ok(Err(e)) => ToolResult::error(format!("Question was not answered: {e}")),
            Err(_) => ToolResult::error(format!(
                "No answer after {}s — nobody appears to be attached to answer. \
                 {NO_ANSWER_GUIDANCE}",
                ANSWER_WAIT_TIMEOUT.as_secs()
            )),
        }
    }
}

/// Render the operator's answers as readable text for the transcript. `answers`
/// is expected to be an object keyed by question header (or index), each value a
/// string or array of strings.
fn render_answers(questions: &[Value], answers: &Value) -> String {
    let mut out = Vec::new();
    for (i, q) in questions.iter().enumerate() {
        let header = q["header"].as_str().unwrap_or("");
        let key_by_header = answers.get(header);
        let key_by_index = answers.get(i.to_string());
        let ans = key_by_header.or(key_by_index);
        let rendered = match ans {
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
