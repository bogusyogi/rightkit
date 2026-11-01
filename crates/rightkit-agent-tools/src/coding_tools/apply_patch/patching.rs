// `apply_patch` — apply a custom patch format to files.
//
// Custom patch format (NOT unified diff):
// ```text
// *** path/to/file
// --- old content line
// +++ new content line
// context line (no prefix) must match exactly
// ```

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct ApplyPatchTool;

#[derive(Debug, Clone)]
enum PatchLine {
    /// `*** path` — sets the target file
    FilePath(String),
    /// `--- content` — line to remove
    Remove(String),
    /// `+++ content` — line to add
    Add(String),
    /// context line (no prefix) — must match existing content
    Context(String),
}

fn parse_patch(patch: &str) -> Vec<PatchLine> {
    patch
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix("*** ") {
                PatchLine::FilePath(rest.to_string())
            } else if let Some(rest) = line.strip_prefix("--- ") {
                PatchLine::Remove(rest.to_string())
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                PatchLine::Add(rest.to_string())
            } else if line == "---" {
                PatchLine::Remove(String::new())
            } else if line == "+++" {
                PatchLine::Add(String::new())
            } else {
                PatchLine::Context(line.to_string())
            }
        })
        .collect()
}

/// Compute a simple edit distance (Levenshtein) between two strings.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let m = a_bytes.len();
    let n = b_bytes.len();
    let mut dp = vec![vec![0; n + 1]; m + 1];

    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_bytes[i - 1] == b_bytes[j - 1] {
                0
            } else {
                1
            };
            dp[i][j] = std::cmp::min(
                std::cmp::min(dp[i - 1][j] + 1, dp[i][j - 1] + 1),
                dp[i - 1][j - 1] + cost,
            );
        }
    }
    dp[m][n]
}

/// Find the line in `lines` that is closest to `target` by edit distance.
/// Returns `(line_index, distance)`.
fn nearest_match_line(lines: &[&str], target: &str) -> Option<(usize, usize)> {
    if lines.is_empty() {
        return None;
    }
    lines
        .iter()
        .enumerate()
        .map(|(idx, l)| (idx, edit_distance(target, l)))
        .min_by_key(|(_, dist)| *dist)
        .map(|(idx, dist)| (idx, dist))
}

/// Map common typographic punctuation to ASCII equivalents so patches
/// authored with plain ASCII still land on files carrying smart quotes,
/// dashes, or odd spaces. Mirrors the most permissive pass of `git apply`
/// leniency (see `repos/cli/codex/codex-rs/apply-patch/src/seek_sequence.rs`).
fn normalize_punct_char(ch: char) -> char {
    crate::coding_tools::edit_helpers::normalize_char(ch)
}

fn normalized_line(line: &str) -> String {
    line.trim().chars().map(normalize_punct_char).collect()
}

/// Outcome of seeking a target line through the graduated rung ladder.
///
/// CR-37: a rung that lands more than one candidate must never guess by
/// falling back to the first forward occurrence — it must report the
/// ambiguity so the caller can fail closed, exactly as `edit_helpers.rs`
/// does for `edit_file`/`multi_edit`.
enum SeekOutcome {
    /// Exactly one match, at this absolute index into `orig_lines`.
    Found(usize),
    /// More than one match at a single rung: the rung's label plus the
    /// absolute (0-based) indices of every candidate line, so the caller can
    /// name them in the error.
    Ambiguous {
        rung_label: &'static str,
        line_indices: Vec<usize>,
    },
    /// Zero matches at every rung.
    NotFound,
}

/// Seek the line at or after `start` matching `target`, trying rungs with
/// decreasing strictness: exact, trailing-whitespace-insensitive,
/// both-sides-trim-insensitive, then Unicode-punctuation-normalized. At each
/// rung every candidate in `lines[start..]` is collected (not just the
/// first): zero matches falls through to the next rung, exactly one match
/// wins outright, and more than one match is reported as ambiguous rather
/// than guessed — the strictest rung that produces any match is always
/// authoritative, but "any match" means "exactly one", never "the first of
/// several". Duplicate lines that are genuinely intended can still be
/// disambiguated with hashline anchors, which bypass this ladder entirely.
fn seek_line(lines: &[&str], target: &str, start: usize) -> SeekOutcome {
    if start > lines.len() {
        return SeekOutcome::NotFound;
    }
    let collect = |predicate: &dyn Fn(&&str) -> bool| -> Vec<usize> {
        lines[start..]
            .iter()
            .enumerate()
            .filter(|(_, line)| predicate(line))
            .map(|(offset, _)| start + offset)
            .collect()
    };
    let resolve = |matches: Vec<usize>, rung_label: &'static str| -> Option<SeekOutcome> {
        match matches.len() {
            0 => None,
            1 => Some(SeekOutcome::Found(matches[0])),
            _ => Some(SeekOutcome::Ambiguous {
                rung_label,
                line_indices: matches,
            }),
        }
    };

    // Exact match first.
    if let Some(outcome) = resolve(collect(&|line| *line == target), "exact") {
        return outcome;
    }
    // Then trailing-whitespace-insensitive.
    if let Some(outcome) = resolve(
        collect(&|line| line.trim_end() == target.trim_end()),
        "trailing-whitespace-insensitive",
    ) {
        return outcome;
    }
    // Then both-sides-trim-insensitive.
    if let Some(outcome) = resolve(
        collect(&|line| line.trim() == target.trim()),
        "both-sides-trim-insensitive",
    ) {
        return outcome;
    }
    // Finally, Unicode-punctuation-normalized (with trim).
    let normalized_target = normalized_line(target);
    match resolve(
        collect(&|line| normalized_line(line) == normalized_target),
        "Unicode-punctuation-normalized",
    ) {
        Some(outcome) => outcome,
        None => SeekOutcome::NotFound,
    }
}

/// Format a typed ambiguity error naming every candidate line, so the caller
/// can add context rather than have the tool guess (CR-37, plan P1-9 DoD
/// item 4: "Ambiguous multi-match still fails rather than guessing.").
fn format_ambiguous_line_error(
    kind: &str,
    pos: usize,
    expected: &str,
    rung_label: &str,
    line_indices: &[usize],
) -> String {
    let candidates: Vec<String> = line_indices
        .iter()
        .map(|idx| format!("line {}", idx + 1))
        .collect();
    format!(
        "{kind} is ambiguous starting at line {}: expected: {:?}\n  {} candidates match under the {} rung: {}. Add surrounding context (or a hashline anchor) so only one location matches.",
        pos + 1,
        expected,
        line_indices.len(),
        rung_label,
        candidates.join(", ")
    )
}

/// Format context around a failed line match.
fn format_context(lines: &[&str], match_idx: usize, context_radius: usize) -> String {
    let start = match_idx.saturating_sub(context_radius);
    let end = std::cmp::min(match_idx + context_radius + 1, lines.len());
    let mut context = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let actual_line_num = start + i + 1; // 1-based
        let marker = if start + i == match_idx {
            ">>> "
        } else {
            "    "
        };
        context.push_str(&format!("{}{}: {}\n", marker, actual_line_num, line));
    }
    context
}

/// Apply a sequence of PatchLines to the given file content.
/// Returns the modified content or an error describing the mismatch.
fn apply_to_content(original: &str, patch_lines: &[PatchLine]) -> Result<String, String> {
    let orig_lines: Vec<&str> = original.lines().collect();
    let mut output: Vec<String> = Vec::new();
    let mut pos = 0usize; // current position in orig_lines

    let mut i = 0;
    while i < patch_lines.len() {
        match &patch_lines[i] {
            PatchLine::FilePath(_) => {
                i += 1;
            }
            PatchLine::Context(ctx_line) => {
                if let Some(anchor) =
                    crate::coding_tools::read_file::resolve_hashline_line(ctx_line, original)?
                {
                    let target = anchor.line_number - 1;
                    if target < pos {
                        return Err(format!(
                            "Context hashline anchor at line {} is before the current patch position line {}. Keep patch lines in file order.",
                            anchor.line_number,
                            pos + 1
                        ));
                    }
                    for l in &orig_lines[pos..=target] {
                        output.push(l.to_string());
                    }
                    pos = target + 1;
                } else {
                    // Find the context line in the original starting from pos,
                    // through the graduated rung ladder.
                    let found = seek_line(&orig_lines, ctx_line.as_str(), pos);
                    match found {
                        SeekOutcome::Found(target) => {
                            let offset = target - pos;
                            // Copy everything up to and including the context line
                            for l in &orig_lines[pos..pos + offset + 1] {
                                output.push(l.to_string());
                            }
                            pos += offset + 1;
                        }
                        SeekOutcome::Ambiguous {
                            rung_label,
                            line_indices,
                        } => {
                            return Err(format_ambiguous_line_error(
                                "Context line",
                                pos,
                                ctx_line.as_str(),
                                rung_label,
                                &line_indices,
                            ));
                        }
                        SeekOutcome::NotFound => {
                            let expected_content = ctx_line.as_str();
                            let (nearest_idx, nearest_dist) =
                                nearest_match_line(&orig_lines[pos..], expected_content)
                                    .map(|(idx, dist)| (pos + idx, dist))
                                    .unwrap_or((pos, usize::MAX));

                            let mut error_msg =
                                format!("Context line not found starting at line {}: ", pos + 1);
                            error_msg.push_str(&format!("expected: {:?}", expected_content));
                            error_msg.push('\n');

                            if nearest_dist < 20 {
                                error_msg.push_str(&format!(
                                    "  Nearest match at line {}: (distance: {})\n",
                                    nearest_idx + 1,
                                    nearest_dist
                                ));
                                error_msg.push_str(&format!(
                                    "  Context:\n{}",
                                    format_context(&orig_lines, nearest_idx, 2)
                                ));
                            }

                            return Err(error_msg);
                        }
                    }
                }
                i += 1;
            }
            PatchLine::Remove(remove_line) => {
                if let Some(anchor) =
                    crate::coding_tools::read_file::resolve_hashline_line(remove_line, original)?
                {
                    let target = anchor.line_number - 1;
                    if target < pos {
                        return Err(format!(
                            "Remove hashline anchor at line {} is before the current patch position line {}. Keep patch lines in file order.",
                            anchor.line_number,
                            pos + 1
                        ));
                    }
                    // Copy everything before the anchored removed line.
                    for l in &orig_lines[pos..target] {
                        output.push(l.to_string());
                    }
                    pos = target + 1;
                } else {
                    // Find and remove this line through the graduated rung
                    // ladder.
                    let found = seek_line(&orig_lines, remove_line.as_str(), pos);
                    match found {
                        SeekOutcome::Found(target) => {
                            let offset = target - pos;
                            // Copy everything before the removed line
                            for l in &orig_lines[pos..pos + offset] {
                                output.push(l.to_string());
                            }
                            pos += offset + 1; // skip the removed line
                        }
                        SeekOutcome::Ambiguous {
                            rung_label,
                            line_indices,
                        } => {
                            return Err(format_ambiguous_line_error(
                                "Line to remove",
                                pos,
                                remove_line.as_str(),
                                rung_label,
                                &line_indices,
                            ));
                        }
                        SeekOutcome::NotFound => {
                            let expected_content = remove_line.as_str();
                            let (nearest_idx, nearest_dist) =
                                nearest_match_line(&orig_lines[pos..], expected_content)
                                    .map(|(idx, dist)| (pos + idx, dist))
                                    .unwrap_or((pos, usize::MAX));

                            let mut error_msg =
                                format!("Line to remove not found starting at line {}: ", pos + 1);
                            error_msg.push_str(&format!("expected: {:?}", expected_content));
                            error_msg.push('\n');

                            if nearest_dist < 20 {
                                error_msg.push_str(&format!(
                                    "  Nearest match at line {}: (distance: {})\n",
                                    nearest_idx + 1,
                                    nearest_dist
                                ));
                                error_msg.push_str(&format!(
                                    "  Context:\n{}",
                                    format_context(&orig_lines, nearest_idx, 2)
                                ));
                            }

                            return Err(error_msg);
                        }
                    }
                }
                i += 1;
            }
            PatchLine::Add(add_line) => {
                output.push(add_line.clone());
                i += 1;
            }
        }
    }

    // Copy remaining lines
    for l in &orig_lines[pos..] {
        output.push(l.to_string());
    }

    // Preserve trailing newline
    let mut result = output.join("\n");
    if original.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}
