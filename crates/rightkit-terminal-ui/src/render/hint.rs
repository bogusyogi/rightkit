//! Render hints: the only shape a tool's call/result data may take on its
//! way into a [`crate::render::ToolCallBlock`].
//!
//! A render hint is DATA, never renderer code. There is exactly one way to
//! construct either type below, and both constructors route every string
//! through [`super::sanitize::sanitize`] and [`super::truncate::truncate`]
//! before storing it. A call site cannot skip that -- there is no field
//! access that bypasses `new`, so a hostile or buggy tool can put whatever
//! bytes it wants into a hint and none of them reach the terminal unfiltered.

use super::sanitize::sanitize;
use super::truncate::{Truncation, truncate};

/// One-line summaries are kept short: this is a title, not a transcript.
const SUMMARY_CAP: usize = 120;
/// Per-line cap for streamed/result output.
const RESULT_LINE_CAP: usize = 200;
/// Line-count cap for a single result. Past this, later lines are dropped
/// and the block records that it truncated.
const RESULT_MAX_LINES: usize = 50;

/// What a tool call looked like when it started: its name and a short
/// human summary (e.g. `"read src/main.rs"`).
#[derive(Debug, Clone)]
pub struct CallHint {
    pub tool: String,
    pub summary: String,
}

impl CallHint {
    pub fn new(tool: impl Into<String>, summary: impl Into<String>) -> Self {
        let tool = sanitize(&tool.into());
        let (summary, _) = truncate(&sanitize(&summary.into()), SUMMARY_CAP);
        Self { tool, summary }
    }
}

/// A tool's finished (or streaming) output, already sanitized and bounded.
#[derive(Debug, Clone, Default)]
pub struct ResultHint {
    pub lines: Vec<String>,
    /// Set when either the line count or an individual line was capped.
    pub truncation: Option<Truncation>,
}

impl ResultHint {
    /// Builds a result hint from raw, untrusted tool output. Sanitizes
    /// first, then bounds both line count and line length -- a tool that
    /// tries to dump gigabytes of output, or a single multi-megabyte line,
    /// cannot make the block grow without limit.
    pub fn new(raw: &str) -> Self {
        let sanitized = sanitize(raw);
        let mut lines = Vec::new();
        let mut truncation = None;

        for (i, line) in sanitized.lines().enumerate() {
            if i >= RESULT_MAX_LINES {
                truncation.get_or_insert(Truncation {
                    original_len: raw.len(),
                    cap: RESULT_MAX_LINES,
                });
                break;
            }
            let (text, cut) = truncate(line, RESULT_LINE_CAP);
            if cut.is_some() {
                truncation.get_or_insert(Truncation {
                    original_len: raw.len(),
                    cap: RESULT_LINE_CAP,
                });
            }
            lines.push(text);
        }

        Self { lines, truncation }
    }
}
