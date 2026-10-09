//! Word-boundary alignment check (used by qualifiers).

use crate::segment::{normalize_words, Cue};
use crate::words::TimedWord;

/// Default tolerance between a cue edge and the nearest word boundary.
pub const ALIGNMENT_TOLERANCE_MS: u64 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlignmentIssue {
    StartNotOnWord { cue: usize, start_ms: u64 },
    EndNotOnWord { cue: usize, end_ms: u64 },
    Overlap { cue: usize },
    Empty { cue: usize },
}

/// Checks that every cue edge lies on a word boundary (within `tolerance_ms`),
/// that cues are non-empty and do not overlap. Empty result means aligned.
pub fn check_alignment(
    cues: &[Cue],
    words: &[TimedWord],
    tolerance_ms: u64,
) -> Vec<AlignmentIssue> {
    let words = normalize_words(words);
    let near =
        |t: u64, f: fn(&TimedWord) -> u64| words.iter().any(|w| f(w).abs_diff(t) <= tolerance_ms);
    let mut issues = Vec::new();
    for (i, c) in cues.iter().enumerate() {
        if c.end_ms <= c.start_ms {
            issues.push(AlignmentIssue::Empty { cue: i });
        }
        if !near(c.start_ms, |w| w.start_ms) {
            issues.push(AlignmentIssue::StartNotOnWord {
                cue: i,
                start_ms: c.start_ms,
            });
        }
        if !near(c.end_ms, |w| w.end_ms) {
            issues.push(AlignmentIssue::EndNotOnWord {
                cue: i,
                end_ms: c.end_ms,
            });
        }
        if i > 0 && cues[i - 1].end_ms > c.start_ms {
            issues.push(AlignmentIssue::Overlap { cue: i });
        }
    }
    issues
}
