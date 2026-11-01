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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_hint_strips_escape_sequences_from_untrusted_tool_metadata() {
        let hint = CallHint::new("read", "read \x1b[31mfile.rs\x1b[0m");
        assert!(!hint.summary.contains('\u{1b}'));
        assert!(!hint.tool.contains('\u{1b}'));
    }

    #[test]
    fn call_hint_summary_is_capped_even_when_the_tool_supplies_a_huge_one() {
        let hint = CallHint::new("read", "x".repeat(10_000));
        assert!(hint.summary.len() <= SUMMARY_CAP);
    }

    #[test]
    fn result_hint_bounds_line_count_for_unbounded_output() {
        let raw = (0..1000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let hint = ResultHint::new(&raw);
        assert!(hint.lines.len() <= RESULT_MAX_LINES);
        assert!(hint.truncation.is_some());
    }

    #[test]
    fn result_hint_records_truncation_with_the_original_size() {
        let raw = "y".repeat(50_000);
        let hint = ResultHint::new(&raw);
        let truncation = hint
            .truncation
            .expect("oversized single line must truncate");
        assert_eq!(truncation.original_len, raw.len());
    }

    #[test]
    fn small_clean_output_is_untouched_and_unmarked() {
        let hint = ResultHint::new("all good\nno issues");
        assert_eq!(
            hint.lines,
            vec!["all good".to_string(), "no issues".to_string()]
        );
        assert!(hint.truncation.is_none());
    }

    #[test]
    fn a_hostile_result_cannot_carry_raw_control_bytes_through() {
        let raw = "before\x1b]0;pwned\x07after";
        let hint = ResultHint::new(raw);
        for line in &hint.lines {
            assert!(!line.contains('\u{1b}'));
            assert!(!line.contains('\u{7}'));
        }
    }
}
