// `apply_patch` — apply a custom patch format to files.
//
// Custom patch format (NOT unified diff):
// ```text
// *** path/to/file
// --- old content line
// +++ new content line
// context line (no prefix) must match exactly
// ```

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
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
    let newer = crate::tool_options::apply_patch_newer();
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
            } else if let Some(escaped) = line.strip_prefix(' ').filter(|rest| {
                newer
                    && (rest.starts_with("---")
                        || rest.starts_with("+++")
                        || rest.starts_with("*** "))
            }) {
                // A single leading space escapes a context line that would otherwise read as a
                // marker (markdown/YAML `---`).
                PatchLine::Context(escaped.to_string())
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
    if lines.is_empty() || target.len() > MAX_DISTANCE_LINE_BYTES {
        return None;
    }
    // `edit_distance` allocates a table of both lengths multiplied, so a very long line of the
    // file (a minified bundle) is not a candidate: the hint is only for near misses.
    lines
        .iter()
        .enumerate()
        .take(MAX_DISTANCE_LINES)
        .filter(|(_, l)| l.len() <= MAX_DISTANCE_LINE_BYTES)
        .map(|(idx, l)| (idx, edit_distance(target, l)))
        .min_by_key(|(_, dist)| *dist)
        .map(|(idx, dist)| (idx, dist))
}

/// Longest line, in bytes, that [`nearest_match_line`] compares (the table it fills grows with the
/// product of two lengths).
const MAX_DISTANCE_LINE_BYTES: usize = 1024;

/// Most lines [`nearest_match_line`] looks at.
const MAX_DISTANCE_LINES: usize = 20_000;

/// Map common typographic punctuation to ASCII equivalents so patches
/// authored with plain ASCII still land on files carrying smart quotes,
/// dashes, or odd spaces. Mirrors the most permissive pass of `git apply`
/// leniency (see `repos/cli/codex/codex-rs/apply-patch/src/seek_sequence.rs`).
fn normalize_punct_char(ch: char) -> char {
    crate::edit_helpers::normalize_char(ch)
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
    if crate::tool_options::apply_patch_newer() {
        return apply_to_content_newer(original, patch_lines);
    }
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
                if let Some(anchor) = crate::read_file::resolve_hashline_line(ctx_line, original)? {
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
                    crate::read_file::resolve_hashline_line(remove_line, original)?
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

/// A Codex patch translated to the native format, with the paths its `Add File` sections name (as
/// written, which is how the native headers spell them).
struct TranslatedPatch {
    native: String,
    added_files: Vec<String>,
}

/// Translate a Codex patch envelope (`*** Begin Patch` ... `*** End Patch`) into the native
/// format, so models trained on the Codex form are accepted. `None` means the patch is not an
/// envelope and is parsed natively. `@@` lines are hunk markers or seek hints, never content: the
/// native parser already seeks the first old line forward, so they are dropped. Delete and move
/// have no native representation and are refused with a pointer to the right tool.
fn translate_codex_envelope(patch: &str) -> Option<Result<TranslatedPatch, String>> {
    let patch = patch.trim_start();
    if !patch.starts_with("*** Begin Patch") {
        return None;
    }
    let marker_like =
        |text: &str| text.starts_with("---") || text.starts_with("+++") || text.starts_with("*** ");
    let mut out = String::new();
    let mut added_files: Vec<String> = Vec::new();
    let mut file: Option<bool> = None; // Some(true) while inside an Add File section
    for (index, line) in patch.lines().enumerate().skip(1) {
        if let Some(marker) = line.strip_prefix("*** ").map(str::trim) {
            if marker == "End Patch" {
                break;
            }
            if marker == "End of File" {
                continue;
            }
            if let Some(path) = marker.strip_prefix("Update File:") {
                out.push_str(&format!("*** {}\n", path.trim()));
                file = Some(false);
            } else if let Some(path) = marker.strip_prefix("Add File:") {
                out.push_str(&format!("*** {}\n", path.trim()));
                added_files.push(path.trim().to_string());
                file = Some(true);
            } else if marker.starts_with("Delete File:") || marker.starts_with("Move to:") {
                return Some(Err(format!(
                    "Patch line {}: '*** {marker}' is not supported by apply_patch; delete or move files with the file delete or move tool, and use '*** Update File:' / '*** Add File:' here.",
                    index + 1
                )));
            } else {
                return Some(Err(format!(
                    "Patch line {}: unknown patch envelope header '*** {marker}'. Use '*** Update File: path' or '*** Add File: path'.",
                    index + 1
                )));
            }
            continue;
        }
        if line.starts_with("@@") {
            continue;
        }
        let Some(adding) = file else {
            if line.trim().is_empty() {
                continue;
            }
            return Some(Err(format!(
                "Patch line {} appears before the first '*** Update File:' or '*** Add File:' header.",
                index + 1
            )));
        };
        let (prefix, text) = if adding {
            ("+++", line.strip_prefix('+').unwrap_or(line))
        } else {
            match line.chars().next() {
                Some('+') => ("+++", &line[1..]),
                Some('-') => ("---", &line[1..]),
                Some(' ') => ("", &line[1..]),
                _ => ("", line),
            }
        };
        match prefix {
            "" if marker_like(text) => out.push_str(&format!(" {text}\n")),
            "" => out.push_str(&format!("{text}\n")),
            _ if text.is_empty() => out.push_str(&format!("{prefix}\n")),
            _ => out.push_str(&format!("{prefix} {text}\n")),
        }
    }
    Some(Ok(TranslatedPatch {
        native: out,
        added_files,
    }))
}

/// Rung labels, strictest first, and the per-line comparison each one uses.
const SEEK_RUNGS: [&str; 4] = [
    "exact",
    "trailing-whitespace-insensitive",
    "both-sides-trim-insensitive",
    "Unicode-punctuation-normalized",
];

fn line_matches_at_rung(rung: usize, line: &str, target: &str) -> bool {
    match rung {
        0 => line == target,
        1 => line.trim_end() == target.trim_end(),
        2 => line.trim() == target.trim(),
        _ => normalized_line(line) == normalized_line(target),
    }
}

/// Seek the position at or after `start` where every line of `block` matches the file's lines
/// contiguously and in order, trying rungs from strictest to loosest. At each rung zero matches
/// falls through, exactly one wins, and more than one is reported as ambiguous rather than
/// guessed. Duplicate blocks that are genuinely intended can still be disambiguated with hashline
/// anchors, which bypass this ladder entirely.
fn seek_block(lines: &[&str], block: &[&str], start: usize) -> SeekOutcome {
    if block.is_empty() || start > lines.len() || lines.len() - start < block.len() {
        return SeekOutcome::NotFound;
    }
    for (rung, rung_label) in SEEK_RUNGS.iter().enumerate() {
        let matches: Vec<usize> = (start..=lines.len() - block.len())
            .filter(|&at| {
                block
                    .iter()
                    .enumerate()
                    .all(|(offset, target)| line_matches_at_rung(rung, lines[at + offset], target))
            })
            .collect();
        match matches.len() {
            0 => {}
            1 => return SeekOutcome::Found(matches[0]),
            _ => {
                return SeekOutcome::Ambiguous {
                    rung_label,
                    line_indices: matches,
                }
            }
        }
    }
    SeekOutcome::NotFound
}

/// Find the original line index a context or removal line at `patch_lines[i]` refers to. The first
/// old line of a patch locates itself with the whole run of consecutive old lines that follows it,
/// which must match contiguously at one position. Every later old line must be the very next
/// original line, or the whole remaining run must match contiguously at exactly one later
/// position, so a single drifted line cannot jump to a look-alike elsewhere.
fn locate_old_line(
    original: &str,
    orig_lines: &[&str],
    patch_lines: &[PatchLine],
    i: usize,
    pos: usize,
    adjacent: bool,
    kind: &str,
) -> Result<usize, String> {
    let text = match &patch_lines[i] {
        PatchLine::Context(text) | PatchLine::Remove(text) => text.as_str(),
        _ => return Err("internal error: not an old line".to_string()),
    };
    if adjacent {
        let hit = orig_lines.get(pos).is_some_and(|line| {
            (0..SEEK_RUNGS.len()).any(|rung| line_matches_at_rung(rung, line, text))
        });
        if hit {
            return Ok(pos);
        }
    }
    let mut block: Vec<&str> = Vec::new();
    for line in &patch_lines[i..] {
        let (PatchLine::Context(t) | PatchLine::Remove(t)) = line else {
            break;
        };
        if !matches!(
            crate::read_file::resolve_hashline_line(t, original),
            Ok(None)
        ) {
            break;
        }
        block.push(t.as_str());
    }
    match seek_block(orig_lines, &block, pos) {
        SeekOutcome::Found(target) => Ok(target),
        SeekOutcome::Ambiguous {
            rung_label,
            line_indices,
        } => Err(format_ambiguous_line_error(
            kind,
            pos,
            text,
            rung_label,
            &line_indices,
        )),
        SeekOutcome::NotFound => {
            let (nearest_idx, nearest_dist) =
                nearest_match_line(&orig_lines[pos.min(orig_lines.len())..], text)
                    .map(|(idx, dist)| (pos + idx, dist))
                    .unwrap_or((pos, usize::MAX));
            let mut error_msg = format!("{kind} not found starting at line {}: ", pos + 1);
            error_msg.push_str(&format!("expected: {:?}", text));
            if block.len() > 1 {
                error_msg.push_str(&format!(
                    " (as one block of {} consecutive lines)",
                    block.len()
                ));
            }
            error_msg.push('\n');
            if nearest_dist < 20 {
                error_msg.push_str(&format!(
                    "  Nearest match at line {}: (distance: {})\n",
                    nearest_idx + 1,
                    nearest_dist
                ));
                error_msg.push_str(&format!(
                    "  Context:\n{}",
                    format_context(orig_lines, nearest_idx, 2)
                ));
            }
            Err(error_msg)
        }
    }
}

/// True when every old line of the patch (context or removed) is a whole line of `original` as
/// written, or a HashLine anchor: the patch then located its place by exact text, like an exact
/// `edit_file` match. Any line that only fits after the whitespace or punctuation tolerance means
/// the model had not seen the text as it is.
fn patch_old_lines_are_exact(original: &str, patch_lines: &[PatchLine]) -> bool {
    let present: std::collections::HashSet<&str> = original
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    patch_lines.iter().all(|line| match line {
        PatchLine::Context(text) | PatchLine::Remove(text) => {
            matches!(
                crate::read_file::resolve_hashline_line(text, original),
                Ok(Some(_))
            ) || present.contains(text.as_str())
        }
        _ => true,
    })
}

/// [`apply_to_content`] under host option `apply_patch_newer`: a run of old lines is located as one
/// contiguous block ([`locate_old_line`]) and every original line keeps its own line ending (a
/// one-line patch to a CRLF or mixed file changes one line; added lines take the file's first
/// ending). Line numbering matches hashline (`\n` only).
fn apply_to_content_newer(original: &str, patch_lines: &[PatchLine]) -> Result<String, String> {
    let orig = crate::edit_helpers::split_line_endings(original);
    let preferred = crate::edit_helpers::preferred_line_ending(&orig);
    let orig_lines: Vec<&str> = orig.iter().map(|(text, _)| *text).collect();
    // (text, terminator, added by the patch)
    let mut output: Vec<(String, Option<crate::edit_helpers::LineEnding>, bool)> = Vec::new();
    // Current position in orig_lines, and whether an old line has been located (later old lines
    // must then be adjacent).
    let mut pos = 0usize;
    let mut adjacent = false;

    for (i, patch_line) in patch_lines.iter().enumerate() {
        match patch_line {
            PatchLine::FilePath(_) => {}
            PatchLine::Context(ctx_line) => {
                if let Some(anchor) = crate::read_file::resolve_hashline_line(ctx_line, original)? {
                    let target = anchor.line_number - 1;
                    if target < pos {
                        return Err(format!(
                            "Context hashline anchor at line {} is before the current patch position line {}. Keep patch lines in file order.",
                            anchor.line_number,
                            pos + 1
                        ));
                    }
                    for (text, ending) in &orig[pos..=target] {
                        output.push((text.to_string(), *ending, false));
                    }
                    pos = target + 1;
                } else {
                    let target = locate_old_line(
                        original,
                        &orig_lines,
                        patch_lines,
                        i,
                        pos,
                        adjacent,
                        "Context line",
                    )?;
                    // Copy everything up to and including the context line
                    for (text, ending) in &orig[pos..=target] {
                        output.push((text.to_string(), *ending, false));
                    }
                    pos = target + 1;
                }
                adjacent = true;
            }
            PatchLine::Remove(remove_line) => {
                if let Some(anchor) =
                    crate::read_file::resolve_hashline_line(remove_line, original)?
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
                    for (text, ending) in &orig[pos..target] {
                        output.push((text.to_string(), *ending, false));
                    }
                    pos = target + 1;
                } else {
                    let target = locate_old_line(
                        original,
                        &orig_lines,
                        patch_lines,
                        i,
                        pos,
                        adjacent,
                        "Line to remove",
                    )?;
                    // Copy everything before the removed line
                    for (text, ending) in &orig[pos..target] {
                        output.push((text.to_string(), *ending, false));
                    }
                    pos = target + 1; // skip the removed line
                }
                adjacent = true;
            }
            PatchLine::Add(add_line) => {
                output.push((add_line.clone(), Some(preferred), true));
            }
        }
    }

    // Copy remaining lines
    for (text, ending) in &orig[pos..] {
        output.push((text.to_string(), *ending, false));
    }

    // Copied lines keep their own terminator, even when they end up last. A line followed by more
    // output always gets one (an unterminated original last line gains the preferred ending). An
    // added line that ends up last is terminated only when the original was empty or ended with a
    // terminator, so a deliberate missing trailing newline survives. Empty output is "".
    let original_terminated = orig.last().is_none_or(|(_, ending)| ending.is_some());
    let last = output.len().saturating_sub(1);
    let mut result = String::with_capacity(original.len());
    for (idx, (text, ending, added)) in output.iter().enumerate() {
        result.push_str(text);
        let ending = if idx < last {
            Some(ending.unwrap_or(preferred))
        } else if *added {
            original_terminated.then_some(preferred)
        } else {
            *ending
        };
        if let Some(ending) = ending {
            result.push_str(ending.as_str());
        }
    }
    Ok(result)
}
