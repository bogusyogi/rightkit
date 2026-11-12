//! `brief` — produce a concise briefing summary of provided context text.
//!
//! Deterministic extractive summary: first sentence per paragraph + key lines
//! (lines starting with `-` or `*`). Returns a markdown summary.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

pub struct BriefTool;

#[async_trait::async_trait]
impl Tool for BriefTool {
    fn name(&self) -> &str {
        "brief"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Produce concise briefing summary of context text: first sentence per paragraph + key lines (- or * prefix). Returns markdown summary. Fail: context field missing."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["context"],
            "properties": {
                "context": {
                    "type": "string",
                    "description": "The text/document to summarize"
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let context = match args["context"].as_str() {
            Some(c) => c,
            None => return ToolResult::error("Missing required argument: context"),
        };

        let summary = extract_brief(context);
        ToolResult::ok(summary)
    }
}

/// Extract a brief summary: first sentence per paragraph + key lines.
fn extract_brief(text: &str) -> String {
    let mut summary = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Track code blocks to skip them
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            continue;
        }

        // Skip empty lines
        if trimmed.is_empty() {
            continue;
        }

        // Include key lines (bullets, dashes)
        if trimmed.starts_with('-') || trimmed.starts_with('*') {
            if !summary.is_empty() && !summary.ends_with('\n') {
                summary.push('\n');
            }
            summary.push_str(trimmed);
            summary.push('\n');
            continue;
        }

        // Include headers
        if trimmed.starts_with('#') {
            if !summary.is_empty() && !summary.ends_with('\n') {
                summary.push('\n');
            }
            summary.push_str(trimmed);
            summary.push('\n');
            continue;
        }

        // For regular paragraphs, extract first sentence
        if let Some(first_sentence) = extract_first_sentence(trimmed) {
            if !summary.is_empty() && !summary.ends_with('\n') {
                summary.push('\n');
            }
            summary.push_str(&first_sentence);
            summary.push('\n');
        }
    }

    summary.trim().to_string()
}

/// Extract the first sentence from a line of text.
fn extract_first_sentence(text: &str) -> Option<String> {
    // Simple sentence extraction: look for period, question mark, or exclamation
    let mut sentence = String::new();
    let mut char_count = 0;
    const MAX_SENTENCE_LEN: usize = 200; // Cap sentence length

    for ch in text.chars() {
        if char_count >= MAX_SENTENCE_LEN {
            sentence.push_str("...");
            break;
        }

        sentence.push(ch);
        char_count += 1;

        if ch == '.' || ch == '?' || ch == '!' {
            // Check if this is the end of a sentence (not in an acronym)
            if let Some(next_char) = text.chars().nth(char_count) {
                if next_char == ' ' || next_char == '\n' {
                    break;
                }
            } else {
                break;
            }
        }
    }

    let trimmed = sentence.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
