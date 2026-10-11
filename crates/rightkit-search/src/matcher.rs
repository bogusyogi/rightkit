//! Free-text matching without an index: substring, case-folded, over explicit haystacks.
//!
//! The query's free text is evaluated here so that it can be checked against several
//! sources (a title and a body, say) and combined across them:
//! - Every bare word and every phrase must appear in some haystack (AND across haystacks).
//! - A phrase must appear contiguously in one haystack.
//! - A `(a OR b)` group needs at least one of its terms in some haystack.
//! - A negated term must appear in no haystack.
//! - `%` and `_` are ordinary characters. Nothing here compiles to SQL `LIKE`.
//!
//! Both needles and haystacks are folded with [`crate::fold::fold_with`] (default:
//! [`crate::fold::fold`]); see that module for coverage and limits (no normalisation, length
//! can change).
//!
//! Operator terms (`from:x`) are not evaluated here; the caller applies them with a
//! schema over [`Query::filters_only`].

use crate::fold::{fold_with, folded_len, FoldOptions};
use crate::snippet::Span;
use crate::terms::Query;
use std::fmt;

/// Why a query cannot be evaluated as free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MatchError {
    /// An OR group mixes free text with operator terms, or contains only negated free
    /// text. Such a group has no free-text meaning here; split the query instead.
    MixedOrGroup,
}

impl fmt::Display for MatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MixedOrGroup => {
                write!(f, "an OR group mixes free text with operators or negation")
            }
        }
    }
}

impl std::error::Error for MatchError {}

/// Highlighted excerpt of one text. The spans, concatenated, are a contiguous substring
/// of the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Snippet {
    pub spans: Vec<Span>,
    /// Text was dropped before the excerpt.
    pub cut_start: bool,
    /// Text was dropped after the excerpt.
    pub cut_end: bool,
}

/// Compiled free-text matcher. Build once per query, then test many haystacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMatcher {
    /// The query is malformed (see [`Query::matches_nothing`]); nothing matches.
    nothing: bool,
    /// Folded bare words and phrases; each must appear.
    all: Vec<String>,
    /// Folded OR groups; each needs one term to appear.
    any: Vec<Vec<String>>,
    /// Folded negated terms; none may appear.
    none: Vec<String>,
    /// Folding applied to needles (at build) and haystacks (at match).
    opts: FoldOptions,
}

fn needles(items: &[String], opts: FoldOptions) -> Vec<String> {
    items
        .iter()
        .map(|s| fold_with(s, opts))
        .filter(|s| !s.is_empty())
        .collect()
}

fn contains_any(haystacks: &[String], needle: &str) -> bool {
    haystacks.iter().any(|h| h.contains(needle))
}

impl TextMatcher {
    /// Compile the free text of `q`. Operator terms are ignored.
    ///
    /// # Errors
    /// [`MatchError::MixedOrGroup`] when an OR group holds a bare-text term alongside
    /// operators, or only negated bare text.
    pub fn new(q: &Query) -> Result<Self, MatchError> {
        Self::with_options(q, FoldOptions::default())
    }

    /// As [`TextMatcher::new`], folding needles and haystacks with `opts` (for example
    /// Turkish/Azeri folding).
    ///
    /// # Errors
    /// As [`TextMatcher::new`].
    pub fn with_options(q: &Query, opts: FoldOptions) -> Result<Self, MatchError> {
        if q.op_or_groups.iter().flatten().any(|t| t.op.is_empty()) {
            return Err(MatchError::MixedOrGroup);
        }
        let all = needles(&q.text, opts)
            .into_iter()
            .chain(needles(&q.phrases, opts))
            .collect();
        let any = q
            .or_groups
            .iter()
            .map(|g| needles(g, opts))
            .filter(|g| !g.is_empty())
            .collect();
        Ok(Self {
            nothing: q.matches_nothing(),
            all,
            any,
            none: needles(&q.negated_text, opts),
            opts,
        })
    }

    /// True when the query has no free-text constraint and matches everything.
    /// A malformed query is not empty: it matches nothing.
    pub fn is_empty(&self) -> bool {
        !self.nothing && self.all.is_empty() && self.any.is_empty() && self.none.is_empty()
    }

    /// Whether the free text matches, given the haystacks already at hand (`eager`) and a
    /// way to fetch the rest (`lazy`). `lazy` is called only when `eager` cannot decide.
    ///
    /// Eager haystacks decide early in two cases: a negated term appears in one (no match),
    /// or nothing is negated and every positive requirement is met in them (match).
    pub fn matches_with(&self, eager: &[&str], lazy: impl FnOnce() -> Vec<String>) -> bool {
        if self.nothing {
            return false;
        }
        if self.is_empty() {
            return true;
        }
        let mut haystacks: Vec<String> = eager.iter().map(|s| fold_with(s, self.opts)).collect();
        if self.none.iter().any(|n| contains_any(&haystacks, n)) {
            return false;
        }
        let positives_met = |hs: &[String]| {
            self.all.iter().all(|n| contains_any(hs, n))
                && self
                    .any
                    .iter()
                    .all(|g| g.iter().any(|n| contains_any(hs, n)))
        };
        if self.none.is_empty() && positives_met(&haystacks[..]) {
            return true;
        }
        haystacks.extend(lazy().iter().map(|s| fold_with(s, self.opts)));
        if self.none.iter().any(|n| contains_any(&haystacks, n)) {
            return false;
        }
        positives_met(&haystacks[..])
    }

    /// Excerpt of at most `max_chars` characters around the first positive hit in `text`,
    /// with every positive hit inside the excerpt marked. Returns `None` when nothing
    /// positive occurs in `text`, when the matcher has no positive terms, or when
    /// `max_chars` is zero.
    ///
    /// Highlights are whole source characters. If folding expands a character (`İ`), the
    /// highlight covers that whole character, so a matched span may fold to more than
    /// the needle.
    pub fn snippet(&self, text: &str, max_chars: usize) -> Option<Snippet> {
        if self.nothing || max_chars == 0 {
            return None;
        }
        let positives: Vec<&str> = self
            .all
            .iter()
            .chain(self.any.iter().flatten())
            .map(String::as_str)
            .collect();
        if positives.is_empty() {
            return None;
        }

        // First hit in the whole text, as a folded byte offset, then as a source char.
        let folded = fold_with(text, self.opts);
        let first = positives.iter().filter_map(|n| folded.find(*n)).min()?;
        let mut acc = 0;
        let (hit_char, hit_byte) = text.char_indices().enumerate().find_map(|(ci, (bi, c))| {
            let len = folded_len(c, self.opts);
            if acc + len > first {
                return Some((ci, bi));
            }
            acc += len;
            None
        })?;

        // Window of `max_chars` characters that contains the hit, biased to start before it.
        let total = text.chars().count();
        let start_char = hit_char
            .saturating_sub(max_chars / 2)
            .min(total.saturating_sub(max_chars));
        let end_char = start_char.saturating_add(max_chars).min(total);
        let byte_at = |char_index: usize| {
            text.char_indices()
                .nth(char_index)
                .map_or(text.len(), |(b, _)| b)
        };
        let (win_start, win_end) = (byte_at(start_char), byte_at(end_char));
        debug_assert!(win_start <= hit_byte && hit_byte < win_end);
        let window = &text[win_start..win_end];

        // Folded positions inside the window, mapped back to source byte offsets. Each
        // entry is (folded start, source start) at the start of a source character.
        let win_folded = fold_with(window, self.opts);
        let mut starts: Vec<(usize, usize)> = Vec::new();
        let mut fold_at = 0;
        for (bi, c) in window.char_indices() {
            starts.push((fold_at, bi));
            fold_at += folded_len(c, self.opts);
        }
        starts.push((win_folded.len(), window.len()));

        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for needle in &positives {
            for (fs, m) in win_folded.match_indices(*needle) {
                let fe = fs + m.len();
                // Round out to whole source characters.
                let s = starts.partition_point(|&(f, _)| f <= fs) - 1;
                let e = starts.partition_point(|&(f, _)| f < fe);
                ranges.push((starts[s].1, starts[e].1));
            }
        }
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (s, e) in ranges {
            match merged.last().copied() {
                Some((_, last_end)) if s <= last_end => {
                    if let Some(last) = merged.last_mut() {
                        last.1 = last_end.max(e);
                    }
                }
                _ => merged.push((s, e)),
            }
        }

        let mut spans = Vec::new();
        let mut pos = 0;
        for (s, e) in merged {
            if s > pos {
                spans.push(Span {
                    text: window[pos..s].to_string(),
                    matched: false,
                });
            }
            spans.push(Span {
                text: window[s..e].to_string(),
                matched: true,
            });
            pos = e;
        }
        if pos < window.len() {
            spans.push(Span {
                text: window[pos..].to_string(),
                matched: false,
            });
        }

        Some(Snippet {
            spans,
            cut_start: win_start > 0,
            cut_end: win_end < text.len(),
        })
    }
}
