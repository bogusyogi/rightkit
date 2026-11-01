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
    let matches = content.matches(old_string).count();
    if matches > 1 && !replace_all {
        return Err(format!(
            "{context_label}: old_string is not unique ({matches} matches). Add surrounding context or set replace_all=true."
        ));
    }
    if matches > 0 {
        return Ok(if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        });
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
            return Ok(replace_ranges_preserving_newlines(
                content, &selected, new_string,
            ));
        }
    }

    let hint = nearest_content_diagnostic(content, old_string)
        .map(|hint| format!(" Nearest-content suggestion: {hint}."))
        .unwrap_or_default();
    Err(format!(
        "{context_label}: old_string not found.{hint} The file may have changed; re-read it and copy the exact text."
    ))
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

fn replace_ranges_preserving_newlines(
    content: &str,
    ranges: &[(usize, usize)],
    replacement: &str,
) -> String {
    let mut updated = content.to_string();
    for &range in ranges.iter().rev() {
        let adapted = replacement_for_range(content, range, replacement);
        updated.replace_range(range.0..range.1, &adapted);
    }
    updated
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_crlf_match_preserves_file_line_endings() {
        let content = "alpha\r\nbeta\r\ngamma\r\n";
        let updated = apply_single_edit(content, "alpha\nbeta\n", "one\ntwo\n", false, "file")
            .expect("normalized CRLF match");
        assert_eq!(updated, "one\r\ntwo\r\ngamma\r\n");
    }

    #[test]
    fn normalized_confusable_match_is_applied_only_when_unique() {
        let content = "let label = “trusted”;\n";
        let updated = apply_single_edit(
            content,
            "label = \"trusted\"",
            "label = \"verified\"",
            false,
            "file",
        )
        .expect("unique normalized match");
        assert_eq!(updated, "let label = \"verified\";\n");
    }

    #[test]
    fn ambiguous_normalized_match_fails_closed() {
        let error = apply_single_edit("“x”\n”x“\n", "\"x\"", "y", false, "file")
            .expect_err("normalized ambiguity must fail");
        assert!(error.contains("normalized old_string is not unique"));
    }

    #[test]
    fn missing_match_offers_bounded_nearby_hint_without_editing() {
        let error = apply_single_edit(
            "fn calculate_total(value: u64) {}\n",
            "fn calculate_totals(value: u64) {}",
            "replacement",
            false,
            "file",
        )
        .expect_err("nearby text is a hint, not an edit target");
        assert!(error.contains("Nearest-content suggestion: lines 1-1 (similarity "));
    }

    #[test]
    fn missing_multiline_match_suggests_real_context_without_editing() {
        let content = "before\nfn authenticate(token: &str) {\n    verify(token);\n}\nafter\n";
        let error = apply_single_edit(
            content,
            "fn authentication(token: &str) {\n    verify(token);\n}",
            "replacement",
            false,
            "file",
        )
        .expect_err("suggestion must remain diagnostic-only");

        assert!(error.contains("Nearest-content suggestion: lines 2-4 (similarity "));
        assert!(error.contains("fn authenticate(token: &str)"));
        assert!(!error.contains("replacement\nafter"));
    }

    #[test]
    fn nearest_match_returns_actual_block_without_auto_applying() {
        let content = "before\nfn authenticate(token: &str) {\n    check(token);\n}\nafter\n";
        let error = apply_single_edit(
            content,
            "fn authenticate(token: &str) {\n    verify(token);\n}",
            "replacement",
            false,
            "file",
        )
        .expect_err("near match must remain a diagnostic");

        assert!(error.contains("Nearest-content suggestion: lines 2-4 (similarity "));
        assert!(error.contains("fn authenticate(token: &str)"));
        assert!(error.contains("check(token);"));
        assert!(!error.contains("replacement"));
        assert!(!error.contains("before\nfn authenticate"));
    }

    #[test]
    fn unrelated_content_has_no_nearest_suggestion() {
        let error = apply_single_edit(
            "completely unrelated text\n",
            "fn calculate_total(value: u64) {}",
            "replacement",
            false,
            "file",
        )
        .expect_err("unrelated text must fail without a misleading hint");

        assert!(!error.contains("Nearest-content suggestion"));
    }

    #[test]
    fn nearest_match_diagnostic_carries_range_and_similarity() {
        let content = "before\nfn authenticate(token: &str) {\n    check(token);\n}\nafter\n";
        let matched = nearest_content_match(
            content,
            "fn authenticate(token: &str) {\n    verify(token);\n}",
        )
        .expect("aider-style near match should retain its structured location");
        assert_eq!((matched.start_line, matched.end_line), (2, 4));
        assert!(matched.similarity >= 0.6);

        let error = apply_single_edit(
            content,
            "fn authenticate(token: &str) {\n    verify(token);\n}",
            "replacement",
            false,
            "file",
        )
        .expect_err("near match must remain diagnostic");

        assert!(error.contains("Nearest-content suggestion: lines 2-4 (similarity "));
        assert!(error.contains("fn authenticate(token: &str)"));
        assert!(!error.contains("replacement"));
    }

    // Fixture corpus of near-miss edits. Each case is constructed so no
    // stricter rung can match, asserting the edit lands exactly at the rung
    // named in the test.

    #[test]
    fn trailing_whitespace_drift_matches_on_trailing_rung() {
        // Exact fails: the file line carries trailing spaces the needle omits.
        let content = "let total = price * qty;   \nreturn total;\n";
        let updated = apply_single_edit(
            content,
            "let total = price * qty;\n",
            "let total = price * qty + tax;\n",
            false,
            "file",
        )
        .expect("trailing-whitespace drift must land");
        assert_eq!(updated, "let total = price * qty + tax;\nreturn total;\n");
    }

    #[test]
    fn indentation_drift_matches_on_trim_rung() {
        // Exact and trailing rungs fail: the inner line of the needle carries
        // 4 spaces while the file carries 6.
        let content = "fn f() {\n      return 1;\n}\n";
        let updated = apply_single_edit(
            content,
            "fn f() {\n    return 1;",
            "fn f() {\n    return 2;",
            false,
            "file",
        )
        .expect("both-sides trim drift must land");
        assert_eq!(updated, "fn f() {\n    return 2;\n}\n");
    }

    #[test]
    fn em_dash_drift_matches_on_punctuation_rung() {
        // Whitespace rungs fail: the drift is a typographic dash, not space.
        let content = "range: 1 \u{2014} 10\n";
        let updated = apply_single_edit(content, "range: 1 - 10", "range: 2 - 20", false, "file")
            .expect("unicode punctuation drift must land");
        assert_eq!(updated, "range: 2 - 20\n");
    }

    #[test]
    fn no_break_space_drift_matches_on_punctuation_rung() {
        let content = "a\u{00a0}b\n";
        let updated = apply_single_edit(content, "a b", "c d", false, "file").expect("nbsp drift");
        assert_eq!(updated, "c d\n");
    }

    #[test]
    fn ambiguous_trailing_whitespace_match_fails_closed() {
        // No exact "xy\n" substring; both lines become "xy\n" once trailing
        // whitespace is ignored, so the rung owes an ambiguity error.
        let error = apply_single_edit("xy \nxy\t\nz\n", "xy\n", "w\n", false, "file")
            .expect_err("rung ambiguity must fail");
        assert!(error.contains("whitespace-tolerant old_string is not unique"));
    }

    #[test]
    fn ambiguous_trim_match_fails_closed() {
        // No exact "a\nb" substring (indentation intervenes) and the trailing
        // rung keeps leading indentation, but trimming both sides yields two
        // identical candidates.
        let error = apply_single_edit("  a\n\tb\n  a\n\tb\n", "a\nb", "c\nd", false, "file")
            .expect_err("trim ambiguity must fail");
        assert!(error.contains("trim-tolerant old_string is not unique"));
    }

    #[test]
    fn stricter_rung_wins_over_looser_one() {
        // Under trim both lines are candidates (an ambiguity), but the
        // trailing rung sees exactly one match and must land there instead of
        // escalating to the looser rung. The second line has no newline, so
        // no exact "keep\n" substring exists anywhere.
        let content = "keep \n  keep";
        let updated = apply_single_edit(content, "keep\n", "done\n", false, "file")
            .expect("trailing rung lands before trim ambiguity is visible");
        assert_eq!(updated, "done\n  keep");
    }
}
