//! Shared helpers for edit_file and multi_edit.
//!
//! Matching runs a graduated ladder modelled on `git apply`'s leniency:
//! 1. exact match,
//! 2. trailing-whitespace-insensitive,
//! 3. both-sides-trim-insensitive,
//! 4. Unicode-punctuation-normalized (dashes, quotes, odd spaces).
//!
//! Each rung either matches uniquely (or `replace_all`) or fails closed —
//! leniency never turns an ambiguous edit into a guess.

/// Largest file `edit_file` and `multi_edit` will load with host option `edit_file_newer`: the
/// tolerant rungs hold several copies of the text.
const MAX_EDIT_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// With host option `edit_file_newer`, a refusal message when `path` is too large to edit in
/// memory, else `None`.
pub(crate) fn oversize_refusal(path: &std::path::Path) -> Option<String> {
    if !crate::tool_options::edit_file_newer() {
        return None;
    }
    oversize_file_refusal(path)
}

/// [`oversize_refusal`] without the option check, for a tool whose own option turns it on.
pub(crate) fn oversize_file_refusal(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::metadata(path).ok()?.len();
    (bytes > MAX_EDIT_FILE_BYTES).then(|| {
        format!(
            "'{}' is {bytes} bytes; this edit tool refuses files over 10 MB. Change it with a shell command (sed, python) instead.",
            path.display()
        )
    })
}

/// Apply a single find-and-replace to `content`.
///
/// Returns `Ok(new_content)` on success, `Err(message)` when the match is
/// missing or ambiguous.
pub fn apply_single_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    context_label: &str,
) -> Result<String, String> {
    apply_single_edit_counted(content, old_string, new_string, replace_all, context_label)
        .map(|(updated, _, _)| updated)
}

/// [`apply_single_edit`], also returning how many places were replaced and, when there was no
/// exact match, the label of the tolerant rung that matched (`None` for an exact match).
pub fn apply_single_edit_counted(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    context_label: &str,
) -> Result<(String, usize, Option<&'static str>), String> {
    let newer = crate::tool_options::edit_file_newer();
    let exact: Vec<(usize, usize)> = content
        .match_indices(old_string)
        .map(|(start, found)| (start, start + found.len()))
        .collect();
    let matches = exact.len();
    if matches > 1 && !replace_all {
        return Err(format!(
            "{context_label}: old_string is not unique ({matches} matches). Add surrounding context or set replace_all=true."
        ));
    }
    if matches > 0 {
        if newer {
            let selected = if replace_all { &exact[..] } else { &exact[..1] };
            let updated = replace_ranges_preserving_newlines(content, selected, new_string);
            return Ok((updated, selected.len(), None));
        }
        return Ok((
            if replace_all {
                content.replace(old_string, new_string)
            } else {
                content.replacen(old_string, new_string, 1)
            },
            matches,
            None,
        ));
    }

    // Graduated leniency: the first rung that lands any match wins, so a
    // stricter rung always takes precedence over a looser one.
    for (rung, label) in [
        (MatchRung::TrailingWhitespace, "whitespace-tolerant"),
        (MatchRung::Trim, "trim-tolerant"),
        (MatchRung::Punctuation, "normalized"),
    ] {
        let rung_matches = rung_match_ranges(content, old_string, rung);
        if rung_matches.len() > 1 && !replace_all {
            return Err(format!(
                "{context_label}: {label} old_string is not unique ({} matches). Add surrounding context or set replace_all=true.",
                rung_matches.len()
            ));
        }
        if !rung_matches.is_empty() {
            let selected = if replace_all {
                rung_matches
            } else {
                rung_matches.into_iter().take(1).collect()
            };
            let applied = selected.len();
            // The matched range starts at the first non-blank character, so the file's own indent
            // stays in place; drop the replacement's copy of the indent the caller typed before
            // old_string, to avoid doubling it. An indent the replacement adds beyond it is kept.
            let replacement = if newer && rung.trims_leading() {
                let indent_len = old_string
                    .find(|c: char| c != ' ' && c != '\t')
                    .unwrap_or(old_string.len());
                if indent_len > 0 {
                    new_string
                        .strip_prefix(&old_string[..indent_len])
                        .unwrap_or_else(|| new_string.trim_start_matches([' ', '\t']))
                } else {
                    new_string
                }
            } else {
                new_string
            };
            let updated = replace_ranges_preserving_newlines(content, &selected, replacement);
            return Ok((updated, applied, Some(label)));
        }
    }

    // A retried edit often misses because its first attempt already landed: say so, so the model
    // moves on instead of re-reading and re-editing.
    if newer && looks_already_applied(content, old_string, new_string) {
        return Err(format!(
            "{context_label}: old_string not found, and the new text is already in the file. This edit looks already applied; do not repeat it. Re-read the file only if you need to change it further."
        ));
    }

    let hint = nearest_content_diagnostic(content, old_string)
        .map(|hint| format!(" Nearest-content suggestion: {hint}."))
        .unwrap_or_default();
    Err(format!(
        "{context_label}: old_string not found.{hint} The file may have changed; re-read it and copy the exact text."
    ))
}

/// True when the replacement text is in the file exactly once and is long enough to be this edit's
/// own text and not a coincidence.
fn looks_already_applied(content: &str, old_string: &str, new_string: &str) -> bool {
    let wanted = new_string.trim();
    wanted.len() >= 12 && !old_string.contains(wanted) && content.matches(wanted).count() == 1
}

/// One tolerance level of the matching ladder, strictest to loosest.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchRung {
    /// Ignore trailing whitespace on each line.
    TrailingWhitespace,
    /// Ignore leading and trailing whitespace on each line.
    Trim,
    /// Trim plus typographic punctuation mapped to ASCII equivalents.
    Punctuation,
}

impl MatchRung {
    fn trims_leading(self) -> bool {
        matches!(self, MatchRung::Trim | MatchRung::Punctuation)
    }

    fn maps_punctuation(self) -> bool {
        matches!(self, MatchRung::Punctuation)
    }
}

pub(crate) fn normalize_char(ch: char) -> char {
    match ch {
        '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
        | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200a}' | '\u{202f}' | '\u{205f}'
        | '\u{3000}' => ' ',
        '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{02bc}' => '\'',
        '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        other => other,
    }
}

/// Transform `input` under a rung, keeping the byte offsets of every retained
/// character so match ranges map back onto the original text. Line separators
/// (`\n`) are always retained so lines never merge across the transform.
fn rung_with_offsets(input: &str, rung: MatchRung) -> (Vec<char>, Vec<usize>, Vec<usize>) {
    let indexed: Vec<(usize, char)> = input.char_indices().collect();
    let mut chars = Vec::new();
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut cursor = 0usize;
    while cursor <= indexed.len() {
        let line_begin = cursor;
        while cursor < indexed.len() && indexed[cursor].1 != '\n' {
            cursor += 1;
        }
        let line_end = cursor;
        let mut lo = line_begin;
        let mut hi = line_end;
        if rung.trims_leading() {
            while lo < hi && indexed[lo].1.is_whitespace() {
                lo += 1;
            }
        }
        while hi > lo && indexed[hi - 1].1.is_whitespace() {
            hi -= 1;
        }
        for (byte, ch) in &indexed[lo..hi] {
            let mapped = if rung.maps_punctuation() {
                normalize_char(*ch)
            } else {
                *ch
            };
            chars.push(mapped);
            starts.push(*byte);
            ends.push(byte + ch.len_utf8());
        }
        if cursor < indexed.len() {
            let (byte, _) = indexed[cursor];
            chars.push('\n');
            starts.push(byte);
            ends.push(byte + 1);
            cursor += 1;
        } else {
            break;
        }
    }
    (chars, starts, ends)
}

fn rung_chars(input: &str, rung: MatchRung) -> Vec<char> {
    rung_with_offsets(input, rung).0
}

/// All byte ranges of `content` that match `needle` under `rung`,
/// non-overlapping, mapped back to original offsets.
fn rung_match_ranges(content: &str, needle: &str, rung: MatchRung) -> Vec<(usize, usize)> {
    let (haystack, starts, ends) = rung_with_offsets(content, rung);
    let needle = rung_chars(needle, rung);
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut index = 0usize;
    while index + needle.len() <= haystack.len() {
        if haystack[index..index + needle.len()] == needle {
            ranges.push((starts[index], ends[index + needle.len() - 1]));
            index += needle.len();
        } else {
            index += 1;
        }
    }
    ranges
}

fn replacement_for_range(content: &str, range: (usize, usize), replacement: &str) -> String {
    if content[range.0..range.1].contains("\r\n") {
        replacement.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        replacement.replace("\r\n", "\n")
    }
}

/// A range spanning lines uses its own newline style; a range inside one line uses that line's
/// ending (or the previous line's, on an unterminated last line).
fn range_uses_crlf(content: &str, range: (usize, usize)) -> bool {
    let matched = &content[range.0..range.1];
    if matched.contains('\n') {
        return matched.contains("\r\n");
    }
    match content[range.1..].find('\n') {
        Some(offset) => content[..range.1 + offset].ends_with('\r'),
        None => content[..range.0]
            .rfind('\n')
            .is_some_and(|newline| content[..newline].ends_with('\r')),
    }
}

/// Every line ends in `\r\n` (and there is at least one line ending).
fn file_is_uniformly_crlf(content: &str) -> bool {
    let newlines = content.matches('\n').count();
    newlines > 0 && content.matches("\r\n").count() == newlines
}

/// Line-ending rule of host option `edit_file_newer`: when the matched text uses CRLF
/// ([`range_uses_crlf`]) or the whole file is CRLF, every newline in `replacement` becomes `\r\n`;
/// otherwise `replacement` lands verbatim, so a deliberate `\r\n` written into an LF region is
/// kept.
fn replacement_for_range_newer(
    content: &str,
    range: (usize, usize),
    replacement: &str,
    file_crlf: bool,
) -> String {
    if file_crlf || range_uses_crlf(content, range) {
        replacement.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        replacement.to_string()
    }
}

fn replace_ranges_preserving_newlines(
    content: &str,
    ranges: &[(usize, usize)],
    replacement: &str,
) -> String {
    let newer = crate::tool_options::edit_file_newer();
    let file_crlf = newer && file_is_uniformly_crlf(content);
    let mut updated = content.to_string();
    for &range in ranges.iter().rev() {
        let adapted = if newer {
            replacement_for_range_newer(content, range, replacement, file_crlf)
        } else {
            replacement_for_range(content, range, replacement)
        };
        updated.replace_range(range.0..range.1, &adapted);
    }
    updated
}

/// A line terminator found in existing file text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// Split `content` into `(text, terminator)` pairs so line-based editors can re-emit every
/// untouched line byte for byte. Only `\n` ends a line (a `\r` right before it makes the line
/// `CrLf` and is excluded from the text); a bare `\r` is not a terminator, so line indices match
/// the hashline numbering. An unterminated tail keeps its raw text with no terminator.
pub(crate) fn split_line_endings(content: &str) -> Vec<(&str, Option<LineEnding>)> {
    content
        .split_inclusive('\n')
        .map(|line| match line.strip_suffix('\n') {
            Some(body) => match body.strip_suffix('\r') {
                Some(text) => (text, Some(LineEnding::CrLf)),
                None => (body, Some(LineEnding::Lf)),
            },
            None => (line, None),
        })
        .collect()
}

/// The ending new lines should use: the first terminator in the file, LF when it has none.
pub(crate) fn preferred_line_ending(lines: &[(&str, Option<LineEnding>)]) -> LineEnding {
    lines
        .iter()
        .find_map(|(_, ending)| *ending)
        .unwrap_or(LineEnding::Lf)
}

/// Find a bounded near-match for an unsuccessful edit without changing content.
///
/// This mirrors aider's diagnostic-only sliding-window suggestion: a candidate
/// must reach a 0.6 similarity threshold, then returns nearby real file lines
/// so callers can retry with exact text. It is deliberately not part of any
/// matching rung, so it can never widen an edit's mutation authority.
#[allow(dead_code)]
pub(crate) fn nearest_content_suggestion(content: &str, old_string: &str) -> Option<String> {
    nearest_content_match(content, old_string).map(|candidate| candidate.suggestion)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NearestContentMatch {
    pub start_line: usize,
    pub end_line: usize,
    pub similarity: f32,
    pub suggestion: String,
}

pub(crate) fn nearest_content_diagnostic(content: &str, old_string: &str) -> Option<String> {
    nearest_content_match(content, old_string).map(|candidate| {
        format!(
            "lines {}-{} (similarity {:.2}): {}",
            candidate.start_line, candidate.end_line, candidate.similarity, candidate.suggestion
        )
    })
}

fn nearest_content_match(content: &str, old_string: &str) -> Option<NearestContentMatch> {
    const MAX_SCAN_LINES: usize = 10_000;
    const MAX_TARGET_LINES: usize = 32;
    const MAX_LINE_CHARS: usize = 160;
    const CONTEXT_LINES: usize = 5;
    const SIMILARITY_THRESHOLD: f32 = 0.6;

    let target = old_string
        .lines()
        .take(MAX_TARGET_LINES)
        .map(|line| normalized_line(line, MAX_LINE_CHARS))
        .collect::<Vec<_>>();
    if target.is_empty() || target.iter().map(Vec::len).sum::<usize>() < 4 {
        return None;
    }

    let lines = content.lines().take(MAX_SCAN_LINES).collect::<Vec<_>>();
    if lines.len() < target.len() {
        return None;
    }

    let mut best: Option<(usize, usize, f32)> = None;
    for start in 0..=lines.len() - target.len() {
        let expected = target.as_slice();
        let candidate = lines[start..start + target.len()]
            .iter()
            .map(|line| normalized_line(line, MAX_LINE_CHARS))
            .collect::<Vec<_>>();
        let similarity = sequence_similarity(expected, &candidate);
        if similarity < SIMILARITY_THRESHOLD {
            continue;
        }
        let boundary_matches = (if candidate.first() == target.first() {
            1
        } else {
            0
        }) + (if candidate.last() == target.last() {
            1
        } else {
            0
        });
        let better = best
            .as_ref()
            .is_none_or(|(_, best_boundaries, best_similarity)| {
                boundary_matches > *best_boundaries
                    || (boundary_matches == *best_boundaries && similarity > *best_similarity)
            });
        if better {
            best = Some((start, boundary_matches, similarity));
        }
    }

    best.map(|(start, _, similarity)| {
        let candidate = lines[start..start + target.len()]
            .iter()
            .map(|line| line.chars().take(MAX_LINE_CHARS).collect::<String>())
            .collect::<Vec<_>>();
        let first_matches = normalized_line(&candidate[0], MAX_LINE_CHARS) == target[0];
        let last_matches = normalized_line(
            candidate.last().expect("target is non-empty"),
            MAX_LINE_CHARS,
        ) == *target.last().expect("target is non-empty");
        let suggestion = if first_matches && last_matches {
            candidate.join("\n")
        } else {
            let excerpt_start = start.saturating_sub(CONTEXT_LINES);
            let excerpt_end = (start + target.len() + CONTEXT_LINES).min(lines.len());
            let excerpt = lines[excerpt_start..excerpt_end]
                .iter()
                .map(|line| line.chars().take(MAX_LINE_CHARS).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            format!("line {}:\n{excerpt}", start + 1)
        };
        NearestContentMatch {
            start_line: start + 1,
            end_line: start + target.len(),
            similarity,
            suggestion,
        }
    })
}

fn normalized_line(line: &str, max_chars: usize) -> Vec<char> {
    rung_chars(line, MatchRung::Punctuation)
        .into_iter()
        .take(max_chars)
        .collect()
}

fn sequence_similarity(expected: &[Vec<char>], actual: &[Vec<char>]) -> f32 {
    if expected.is_empty() && actual.is_empty() {
        return 1.0;
    }
    expected
        .iter()
        .zip(actual)
        .map(|(left, right)| line_similarity(left, right))
        .sum::<f32>()
        / expected.len().max(actual.len()) as f32
}

fn line_similarity(expected: &[char], actual: &[char]) -> f32 {
    let longest = expected.len().max(actual.len());
    if longest == 0 {
        return 1.0;
    }
    let mut row = vec![0usize; actual.len() + 1];
    for expected_char in expected {
        let mut diagonal = 0usize;
        for (index, actual_char) in actual.iter().enumerate() {
            let previous = row[index + 1];
            row[index + 1] = if expected_char == actual_char {
                diagonal + 1
            } else {
                row[index + 1].max(row[index])
            };
            diagonal = previous;
        }
    }
    2.0 * row[actual.len()] as f32 / (expected.len() + actual.len()) as f32
}

/// Whether an edit only inserts whole lines around the text it matched: the replacement is the
/// matched text with lines added before it or after it, so every existing character survives and
/// the addition does not start or end inside a line of the existing text. Used with host option
/// `edit_test_file_additions` to let an unattended agent ADD to an existing test file (a new test
/// case next to the old ones) while a change or removal of existing test text still asks.
/// HashLine-anchored blocks and `replace_all` are never insert-only. An inserted line can still
/// change what a test does (an early `return`, a skip decorator); the rule narrows the protection
/// to rewrites, it does not prove the addition is honest.
pub fn edit_only_inserts(old_string: &str, new_string: &str, replace_all: bool) -> bool {
    if replace_all || old_string.trim().is_empty() || new_string.len() <= old_string.len() {
        return false;
    }
    let after = new_string.strip_prefix(old_string);
    let before = new_string.strip_suffix(old_string);
    after.is_some_and(|rest| rest.starts_with('\n') || rest.starts_with("\r\n"))
        || before.is_some_and(|rest| rest.ends_with('\n'))
}

/// [`edit_only_inserts`] for a `multi_edit` call's `edits` array: true only when every edit is
/// insert-only and there is at least one.
pub fn edits_only_insert(edits: &[serde_json::Value]) -> bool {
    !edits.is_empty()
        && edits.iter().all(|edit| {
            match (edit["old_string"].as_str(), edit["new_string"].as_str()) {
                (Some(old), Some(new)) => {
                    edit_only_inserts(old, new, edit["replace_all"].as_bool().unwrap_or(false))
                }
                _ => false,
            }
        })
}
