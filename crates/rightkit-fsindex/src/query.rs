// Fuzzy and one-typo name scoring adapted from fsearch
// (https://github.com/noahdunnagan/fsearch, src/query.rs), Copyright (c) 2026
// Noah Dunnagan, MIT License; see THIRD-PARTY-NOTICES. Changes: character-mask
// prefilters removed, token parsing reduced to a small query language, scoring
// runs per distinct interned name.

//! Name query: whitespace-separated tokens, all must match.
//!
//! Plain words are fuzzy (fzf-style subsequence, words of five or more
//! letters also forgive one typo at a word start). `'word` matches exactly
//! (substring), `^word` a prefix, `word$` a suffix, `!word` excludes names
//! containing it.

use crate::error::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Fuzzy,
    Exact,
    Prefix,
    Suffix,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub text: Vec<u8>,
    pub mode: Mode,
    pub negate: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub tokens: Vec<Token>,
}

const TYPO_MIN_LEN: usize = 5;
const TYPO_COST: i32 = 60;

fn takes_typos(text: &[u8], mode: Mode) -> bool {
    mode == Mode::Fuzzy && text.len() >= TYPO_MIN_LEN
}

impl Query {
    pub fn parse(s: &str) -> Result<Query> {
        let mut q = Query::default();
        for w in s.split_whitespace() {
            let (mut t, mut negate, mut mode) = (w, false, Mode::Fuzzy);
            if let Some(r) = t.strip_prefix('!') {
                (t, negate, mode) = (r, true, Mode::Exact);
            }
            if let Some(r) = t.strip_prefix('\'') {
                (t, mode) = (r, Mode::Exact);
            } else if let Some(r) = t.strip_prefix('^') {
                (t, mode) = (r, Mode::Prefix);
            } else if let Some(r) = t.strip_suffix('$') {
                (t, mode) = (r, Mode::Suffix);
            }
            if t.is_empty() {
                continue;
            }
            if t.len() > 128 {
                return Err(Error::InvalidQuery("token longer than 128 bytes".into()));
            }
            q.tokens.push(Token {
                text: t.bytes().map(|b| b.to_ascii_lowercase()).collect(),
                mode,
                negate,
            });
        }
        if q.tokens.iter().all(|t| t.negate) {
            return Err(Error::InvalidQuery(
                "query needs at least one positive token".into(),
            ));
        }
        Ok(q)
    }

    /// Sum of positive token scores, or None if any positive token misses or a negated one hits.
    pub fn score_name(&self, name: &[u8]) -> Option<i32> {
        let mut total = 0;
        for t in &self.tokens {
            let s = token_score(name, t);
            if t.negate {
                if s.is_some() {
                    return None;
                }
            } else {
                total += s?;
            }
        }
        Some(total)
    }
}

#[inline(always)]
pub(crate) fn fold(b: u8) -> u8 {
    b | (((b.wrapping_sub(b'A') < 26) as u8) << 5)
}

fn find_ci(name: &[u8], q: &[u8]) -> Option<usize> {
    if q.len() > name.len() {
        return None;
    }
    (0..=name.len() - q.len()).find(|&i| {
        name[i..i + q.len()]
            .iter()
            .zip(q)
            .all(|(&a, &b)| fold(a) == b)
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Lower,
    Upper,
    Digit,
    Delim,
    Other,
}

#[inline(always)]
fn class(b: u8) -> Class {
    match b {
        b'a'..=b'z' => Class::Lower,
        b'A'..=b'Z' => Class::Upper,
        b'0'..=b'9' => Class::Digit,
        b' ' | b'_' | b'-' | b'.' | b'/' | b'(' | b')' | b'[' | b']' | b',' | b'+' | b'@' => {
            Class::Delim
        }
        _ => Class::Other,
    }
}

const SCORE_MATCH: i32 = 16;
const GAP_START: i32 = -3;
const GAP_EXT: i32 = -1;
const BONUS_BOUNDARY: i32 = 8;
const BONUS_CAMEL: i32 = 7;
const BONUS_CONSEC: i32 = 4;

#[inline(always)]
fn bonus(prev: Class, cur: Class) -> i32 {
    match (prev, cur) {
        (Class::Delim, c) if c != Class::Delim => BONUS_BOUNDARY,
        (Class::Lower, Class::Upper) | (Class::Lower | Class::Upper, Class::Digit) => BONUS_CAMEL,
        _ => 0,
    }
}

/// fzf-v1 style: leftmost-ending match, shrunk from the right, then scored
/// with boundary/camel/consecutive bonuses. Returns None when no match.
pub fn fuzzy_score(name: &[u8], q: &[u8]) -> Option<i32> {
    fuzzy_score_capped(name, q, 100)
}

/// `fuzzy_score` with the whole-name/stem/prefix bonus capped at `cap`.
fn fuzzy_score_capped(name: &[u8], q: &[u8], cap: i32) -> Option<i32> {
    // Leftmost-ending match: jump to each query byte in turn (memchr is
    // SIMD; most names fail on the first or second byte).
    let mut end = 0;
    let mut from = 0;
    for &c in q {
        end = from + find_folded(&name[from..], c)?;
        from = end + 1;
    }
    if q.len() == 1 {
        return Some(single_score(name, end, cap));
    }
    // Shrink from the right: the latest start that still ends at `end`.
    let mut start = end + 1;
    for &c in q.iter().rev() {
        start = rfind_folded(&name[..start], c)?;
    }
    // Score the greedy match from `start`, jumping between matched bytes:
    // each gap costs GAP_START then GAP_EXT per byte, a run of consecutive
    // matches carries its strongest boundary bonus along.
    let mut score = 0;
    let (mut at, mut first_bonus) = (start, 0);
    for (k, &c) in q.iter().enumerate() {
        let mut run = false;
        if k > 0 {
            let last = at;
            at = last + 1 + find_folded(&name[last + 1..=end], c)?;
            run = at == last + 1;
            if !run {
                score += GAP_START + (at - last - 2) as i32 * GAP_EXT;
            }
        }
        let prev = if at == 0 {
            Class::Delim
        } else {
            class(name[at - 1])
        };
        let mut b = bonus(prev, class(name[at]));
        if run {
            if b >= BONUS_BOUNDARY && b > first_bonus {
                first_bonus = b;
            }
            b = b.max(first_bonus).max(BONUS_CONSEC);
        } else {
            first_bonus = b;
        }
        score += SCORE_MATCH + if k == 0 { b * 2 } else { b };
    }
    // Whole-name and stem matches are what people mean most of the time. A
    // leading dot doesn't count: "zshrc" means ~/.zshrc.
    let off = (name.len() > 1 && name[0] == b'.') as usize;
    let stem = name
        .iter()
        .rposition(|&b| b == b'.')
        .filter(|&p| p > off)
        .unwrap_or(name.len());
    let contiguous = end + 1 - start == q.len();
    let placed = if start == off && contiguous && end + 1 == name.len() {
        100
    } else if start == off && contiguous && end + 1 == stem {
        80
    } else if start == off && contiguous {
        30
    } else {
        0
    };
    Some(score + placed.min(cap) - (name.len() as i32).min(80) / 3)
}

/// First byte of `s` that folds to `c` (an already-lowercased query byte).
#[inline(always)]
fn find_folded(s: &[u8], c: u8) -> Option<usize> {
    if c.is_ascii_lowercase() {
        memchr::memchr2(c, c - 32, s)
    } else {
        memchr::memchr(c, s)
    }
}

/// Last byte of `s` that folds to `c`.
#[inline(always)]
fn rfind_folded(s: &[u8], c: u8) -> Option<usize> {
    if c.is_ascii_lowercase() {
        memchr::memrchr2(c, c - 32, s)
    } else {
        memchr::memrchr(c, s)
    }
}

/// `fuzzy_score` for a one-byte query matched at `i`: the general scoring
/// loop collapses to one step.
#[inline]
fn single_score(name: &[u8], i: usize, cap: i32) -> i32 {
    let prev = if i == 0 {
        Class::Delim
    } else {
        class(name[i - 1])
    };
    let mut score = SCORE_MATCH + bonus(prev, class(name[i])) * 2;
    let off = (name.len() > 1 && name[0] == b'.') as usize;
    if i == off {
        let stem = name
            .iter()
            .rposition(|&b| b == b'.')
            .filter(|&p| p > off)
            .unwrap_or(name.len());
        score += cap.min(if i + 1 == name.len() {
            100
        } else if i + 1 == stem {
            80
        } else {
            30
        });
    }
    score - (name.len() as i32).min(80) / 3
}

/// Best score for `q` read with one typo (see `one_edit_prefix`) at the
/// start of `name` or of a space-separated word in it: scored as if the
/// right letters had been typed, minus TYPO_COST, and never placed above a
/// prefix: "manif" is "manifest" being typed, not a typo of "manic". `m` is
/// as for `token_score`. Other word starts (`_`, `-`, camelCase) would cost
/// a scan of every name per query, ~10x the price.
fn typo_score(name: &[u8], q: &[u8]) -> Option<i32> {
    let mut best = typo_at(name, (name.len() > 1 && name[0] == b'.') as usize, q);
    for sp in memchr::memchr_iter(b' ', name) {
        best = best.max(typo_at(name, sp + 1, q));
    }
    best
}

fn typo_at(name: &[u8], s: usize, q: &[u8]) -> Option<i32> {
    if name.get(s).is_none_or(|&b| fold(b) != q[0]) {
        return None;
    }
    let mut fixed = [0u8; 128];
    let fixed = fixed.get_mut(..one_edit_prefix(&name[s..], q)?)?;
    for (f, &b) in fixed.iter_mut().zip(&name[s..]) {
        *f = fold(b);
    }
    Some(fuzzy_score_capped(name, fixed, 30)? - TYPO_COST)
}

/// How long a prefix of `w` the query `q` spells with exactly one edit (a
/// wrong, extra, missing or swapped letter), if it does. Digits are never
/// edited: "hat_18" is another file than "hat_98", not a typo of it.
fn one_edit_prefix(w: &[u8], q: &[u8]) -> Option<usize> {
    let starts =
        |w: &[u8], q: &[u8]| w.len() >= q.len() && w.iter().zip(q).all(|(&a, &b)| fold(a) == b);
    // The first difference; none means `q` is a clean prefix, not a typo.
    let i = (0..q.len()).find(|&i| i >= w.len() || fold(w[i]) != q[i])?;
    if q[i].is_ascii_digit() || w.get(i).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let rest = &q[i + 1..];
    let after = w.get(i + 1..).unwrap_or_default();
    if i + 1 < q.len()
        && i + 1 < w.len()
        && fold(w[i]) == q[i + 1]
        && fold(w[i + 1]) == q[i]
        && starts(&w[i + 2..], &q[i + 2..])
    {
        return Some(q.len());
    }
    if i < w.len() && starts(after, rest) {
        return Some(q.len());
    }
    if starts(&w[i..], rest) {
        return Some(q.len() - 1);
    }
    (i < w.len() && starts(after, &q[i..])).then_some(q.len() + 1)
}

fn token_score(name: &[u8], t: &Token) -> Option<i32> {
    let len_pen = (name.len() as i32).min(80) / 3;
    match t.mode {
        Mode::Fuzzy if takes_typos(&t.text, t.mode) => {
            fuzzy_score(name, &t.text).max(typo_score(name, &t.text))
        }
        Mode::Fuzzy => fuzzy_score(name, &t.text),
        Mode::Exact => find_ci(name, &t.text).map(|p| 40 + if p == 0 { 30 } else { 0 } - len_pen),
        Mode::Prefix => (name.len() >= t.text.len()
            && name.iter().zip(&t.text).all(|(&a, &b)| fold(a) == b))
        .then(|| 60 - len_pen),
        Mode::Suffix => (name.len() >= t.text.len()
            && name[name.len() - t.text.len()..]
                .iter()
                .zip(&t.text)
                .all(|(&a, &b)| fold(a) == b))
        .then(|| 50 - len_pen),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_subsequence_and_ranking() {
        let q = Query::parse("rdme").unwrap();
        assert!(q.score_name(b"README.md").is_some());
        assert!(q.score_name(b"photo.jpg").is_none());
        let exact = Query::parse("readme")
            .unwrap()
            .score_name(b"readme")
            .unwrap();
        let loose = Query::parse("readme")
            .unwrap()
            .score_name(b"a_really_long_eadme_file")
            .unwrap_or(i32::MIN);
        assert!(exact > loose);
    }

    #[test]
    fn typo_tolerance() {
        let q = Query::parse("invoise").unwrap();
        assert!(q.score_name(b"invoice_2024.pdf").is_some());
        let swapped = Query::parse("reprot").unwrap();
        assert!(swapped.score_name(b"report.docx").is_some());
        assert!(Query::parse("hat_18")
            .unwrap()
            .score_name(b"hat_98")
            .is_none());
    }

    #[test]
    fn modes_and_negation() {
        assert!(Query::parse("^rep")
            .unwrap()
            .score_name(b"report")
            .is_some());
        assert!(Query::parse("^rep")
            .unwrap()
            .score_name(b"my_report")
            .is_none());
        assert!(Query::parse("pdf$").unwrap().score_name(b"a.pdf").is_some());
        assert!(Query::parse("'port !draft")
            .unwrap()
            .score_name(b"report_draft")
            .is_none());
        assert!(Query::parse("!x").is_err());
    }
}
