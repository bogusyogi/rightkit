//! Deterministic text rules for `ArtifactKind::Text` subjects.
//!
//! Each rule is a pure function of the text body and the thresholds and lists
//! in its `RuleKind`. None of those values is a constant here, and
//! [`policy_problem`] refuses a rule whose data is outside its domain. Every rule
//! returns verified (pass), failed (fail), or inconclusive. Evidence goes into
//! the reason as character offsets (Unicode scalar values, counted from the
//! start of the body) or as counts.
//!
//! Normalisation, applied before every text rule:
//! - U+2018 and U+2019 become U+0027, and U+201C and U+201D become U+0022.
//!   Positions do not change.
//! - Unicode NFC is not applied here. Producers supply NFC text.
//! - A word is a maximal run of alphabetic characters, numeric characters and
//!   U+0027. Dashes and hyphen-minus are not word characters.
//! - A sentence ends after each run of `.`, `!` or `?` that is followed by
//!   whitespace or the end of the text, and at each line break that ends a line
//!   holding non-whitespace. Sentences with no word are ignored by every rule.
//! - A word count is the number of words in the body.
//!
//! A text rule is inconclusive when the body has fewer words than its
//! `min_words`, when it has no body, or when its denominator is zero.

use crate::rules::{ApprovedProof, RuleKind};
use crate::subject::{Subject, TextBody};
use crate::verdict::Verdict;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

struct Sentence {
    start: usize,
    end: usize,
    words: Vec<Span>,
}

/// A normalised body. Positions index `chars`, which keeps the offsets the
/// original text had.
struct Body {
    chars: Vec<char>,
    words: Vec<Span>,
    sentences: Vec<Sentence>,
}

fn fold_quote(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        other => other,
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric() || c == '\''
}

fn word_runs(chars: &[char]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if is_word_char(chars[index]) {
            let start = index;
            while index < chars.len() && is_word_char(chars[index]) {
                index += 1;
            }
            spans.push(Span { start, end: index });
        } else {
            index += 1;
        }
    }
    spans
}

/// Sentence spans as half-open `(start, end)` character ranges. Includes
/// ranges with no word; the caller filters them.
fn sentence_bounds(chars: &[char]) -> Vec<(usize, usize)> {
    let n = chars.len();
    let mut bounds = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < n {
        let c = chars[index];
        if matches!(c, '.' | '!' | '?') {
            let mut end = index;
            while end < n && matches!(chars[end], '.' | '!' | '?') {
                end += 1;
            }
            if end == n || chars[end].is_whitespace() {
                bounds.push((start, end));
                start = end;
            }
            index = end;
        } else if c == '\n' {
            if chars[start..index].iter().any(|c| !c.is_whitespace()) {
                bounds.push((start, index));
            }
            start = index + 1;
            index += 1;
        } else {
            index += 1;
        }
    }
    if start < n && chars[start..n].iter().any(|c| !c.is_whitespace()) {
        bounds.push((start, n));
    }
    bounds
}

fn analyse(raw: &str) -> Body {
    let chars: Vec<char> = raw.chars().map(fold_quote).collect();
    let words = word_runs(&chars);
    let mut sentences = Vec::new();
    let mut next_word = 0;
    for (start, end) in sentence_bounds(&chars) {
        while next_word < words.len() && words[next_word].start < start {
            next_word += 1;
        }
        let mut last = next_word;
        while last < words.len() && words[last].start < end {
            last += 1;
        }
        if last > next_word {
            sentences.push(Sentence {
                start,
                end,
                words: words[next_word..last].to_vec(),
            });
        }
        next_word = last;
    }
    Body {
        chars,
        words,
        sentences,
    }
}

/// Lower-cases the characters and collapses each whitespace run into one space.
/// Each folded character keeps the index of the body character it came from.
fn fold(chars: &[char]) -> Vec<(char, usize)> {
    let mut out = Vec::with_capacity(chars.len());
    let mut in_space = false;
    for (index, &c) in chars.iter().enumerate() {
        if c.is_whitespace() {
            if !in_space {
                out.push((' ', index));
            }
            in_space = true;
        } else {
            in_space = false;
            out.extend(c.to_lowercase().map(|lower| (lower, index)));
        }
    }
    out
}

/// The folded, trimmed form of a banned phrase, ready to compare.
fn phrase_needle(phrase: &str) -> Vec<char> {
    let chars: Vec<char> = phrase.chars().map(fold_quote).collect();
    let folded = fold(&chars);
    let start = folded
        .iter()
        .position(|(c, _)| *c != ' ')
        .unwrap_or(folded.len());
    let end = folded
        .iter()
        .rposition(|(c, _)| *c != ' ')
        .map_or(start, |last| last + 1);
    folded[start..end].iter().map(|(c, _)| *c).collect()
}

fn span_text(chars: &[char], span: Span) -> String {
    chars[span.start..span.end].iter().collect()
}

/// Lower-cased words of a character slice, in order.
fn lower_words(chars: &[char]) -> Vec<String> {
    word_runs(chars)
        .into_iter()
        .map(|span| span_text(chars, span).to_lowercase())
        .collect()
}

/// The words of one opener entry, lower-cased, after the same quote folding.
fn opener_words(opener: &str) -> Vec<String> {
    let chars: Vec<char> = opener.chars().map(fold_quote).collect();
    lower_words(&chars)
}

fn bad_threshold(ok: bool) -> Option<String> {
    if ok {
        None
    } else {
        Some("has a threshold outside its domain".to_string())
    }
}

/// Checks the policy data of a text rule kind. Returns the problem, phrased to
/// complete "rule `id` ...", or `None` when the data is valid or the kind is not a text kind.
pub(crate) fn policy_problem(kind: &RuleKind) -> Option<String> {
    let valid_max = |value: f64| value.is_finite() && value >= 0.0;
    match kind {
        RuleKind::BannedPhrase { phrases, .. } => {
            if phrases.is_empty() {
                Some("has an empty phrase list".to_string())
            } else if phrases
                .iter()
                .any(|phrase| phrase_needle(phrase).is_empty())
            {
                Some("has an empty phrase".to_string())
            } else {
                None
            }
        }
        RuleKind::DashDensity {
            max_per_100_words, ..
        }
        | RuleKind::SemicolonDensity {
            max_per_100_words, ..
        } => bad_threshold(valid_max(*max_per_100_words)),
        RuleKind::SentenceLengthVariance { min_variance, .. } => {
            bad_threshold(valid_max(*min_variance))
        }
        RuleKind::TricolonFrequency {
            max_per_100_sentences,
            ..
        } => bad_threshold(valid_max(*max_per_100_sentences)),
        RuleKind::StockOpener { openers, .. } => {
            if openers.is_empty() {
                return Some("has an empty opener list".to_string());
            }
            let bad = openers.iter().any(|opener| {
                let count = opener_words(opener).len();
                count == 0 || count > 3
            });
            if bad {
                Some("has an opener that is not one to three words".to_string())
            } else {
                None
            }
        }
        RuleKind::ClaimProof {
            approved_proofs, ..
        } => approved_proofs.iter().find_map(proof_problem),
        _ => None,
    }
}

fn proof_problem(proof: &ApprovedProof) -> Option<String> {
    let blank = [&proof.id, &proof.unit, &proof.subject]
        .iter()
        .any(|field| field.trim().is_empty());
    if blank {
        return Some("has an approved proof with an empty id, unit or subject".to_string());
    }
    if canonical_decimal(&proof.value).is_none() {
        return Some(format!(
            "has an approved proof `{}` whose value is not a decimal",
            proof.id
        ));
    }
    match (
        parse_date(&proof.valid_from),
        parse_date(&proof.valid_until),
    ) {
        (Some(from), Some(until)) if from <= until => None,
        (Some(_), Some(_)) => Some(format!(
            "has an approved proof `{}` whose validity ends before it starts",
            proof.id
        )),
        _ => Some(format!(
            "has an approved proof `{}` whose dates are not YYYY-MM-DD",
            proof.id
        )),
    }
}

/// The text body, analysed, or the inconclusive verdict that stops the rule.
fn measured(subject: &Subject, min_words: u32) -> Result<(&TextBody, Body), Verdict> {
    let Some(text) = subject.artifact.text.as_ref() else {
        return Err(Verdict::Inconclusive {
            reason: "no text body was supplied".to_string(),
        });
    };
    let body = analyse(&text.body);
    let words = body.words.len();
    if words < min_words as usize {
        return Err(Verdict::Inconclusive {
            reason: format!("text has {words} word(s), below the minimum of {min_words}"),
        });
    }
    Ok((text, body))
}

/// Fails when any listed phrase occurs, case-insensitively and on word
/// boundaries. A boundary is any position where the neighbouring character is
/// not a word character.
pub(crate) fn banned_phrase(subject: &Subject, phrases: &[String], min_words: u32) -> Verdict {
    let (_, body) = match measured(subject, min_words) {
        Ok(found) => found,
        Err(verdict) => return verdict,
    };
    let folded = fold(&body.chars);
    let mut hits = Vec::new();
    for phrase in phrases {
        let needle = phrase_needle(phrase);
        if needle.is_empty() {
            continue;
        }
        for (at, window) in folded.windows(needle.len()).enumerate() {
            if !window.iter().zip(&needle).all(|((c, _), n)| c == n) {
                continue;
            }
            let before = at.checked_sub(1).map(|index| folded[index].0);
            let after = folded.get(at + needle.len()).map(|(c, _)| *c);
            if before.is_some_and(is_word_char) || after.is_some_and(is_word_char) {
                continue;
            }
            let start = folded[at].1;
            let end = folded[at + needle.len() - 1].1 + 1;
            hits.push(format!("`{phrase}` at {start}..{end}"));
        }
    }
    if hits.is_empty() {
        Verdict::Verified
    } else {
        Verdict::Failed {
            reason: format!("banned phrase(s) found: {}", hits.join("; ")),
        }
    }
}

pub(crate) fn dash_density(subject: &Subject, max_per_100_words: f64, min_words: u32) -> Verdict {
    density(
        subject,
        max_per_100_words,
        min_words,
        "dash(es) U+2014 or U+2013",
        |c| matches!(c, '\u{2014}' | '\u{2013}'),
    )
}

pub(crate) fn semicolon_density(
    subject: &Subject,
    max_per_100_words: f64,
    min_words: u32,
) -> Verdict {
    density(
        subject,
        max_per_100_words,
        min_words,
        "semicolon(s) U+003B",
        |c| c == ';',
    )
}

/// Fails when the count of matching characters per 100 words is above `max`.
/// A rate equal to `max` passes.
fn density(
    subject: &Subject,
    max: f64,
    min_words: u32,
    label: &str,
    counts: impl Fn(char) -> bool,
) -> Verdict {
    let (_, body) = match measured(subject, min_words) {
        Ok(found) => found,
        Err(verdict) => return verdict,
    };
    let words = body.words.len();
    if words == 0 {
        return Verdict::Inconclusive {
            reason: "text has no words, so density is undefined".to_string(),
        };
    }
    let positions: Vec<usize> = body
        .chars
        .iter()
        .enumerate()
        .filter_map(|(index, &c)| counts(c).then_some(index))
        .collect();
    let rate = positions.len() as f64 * 100.0 / words as f64;
    if rate > max {
        Verdict::Failed {
            reason: format!(
                "{} {label} at offset(s) {positions:?} over {words} word(s): {rate:.2} per 100 words exceeds {max:.2}",
                positions.len()
            ),
        }
    } else {
        Verdict::Verified
    }
}

/// Fails when the population variance of words per sentence is below `min_variance`.
pub(crate) fn sentence_length_variance(
    subject: &Subject,
    min_variance: f64,
    min_sentences: u32,
    min_words: u32,
) -> Verdict {
    let (_, body) = match measured(subject, min_words) {
        Ok(found) => found,
        Err(verdict) => return verdict,
    };
    let lengths: Vec<f64> = body
        .sentences
        .iter()
        .map(|sentence| sentence.words.len() as f64)
        .collect();
    let count = lengths.len();
    if count == 0 {
        return Verdict::Inconclusive {
            reason: "text has no sentence with words".to_string(),
        };
    }
    if count < min_sentences as usize {
        return Verdict::Inconclusive {
            reason: format!(
                "text has {count} sentence(s) with words, below the minimum of {min_sentences}"
            ),
        };
    }
    let mean = lengths.iter().sum::<f64>() / count as f64;
    let variance = lengths.iter().map(|&l| (l - mean).powi(2)).sum::<f64>() / count as f64;
    if variance < min_variance {
        Verdict::Failed {
            reason: format!(
                "sentence-length variance {variance:.2} over {count} sentence(s) is below the floor {min_variance:.2}"
            ),
        }
    } else {
        Verdict::Verified
    }
}

/// Fails when tricolon sentences per 100 sentences are above `max`. A
/// sentence is a tricolon when it contains a token `A`, then `", "`, then a
/// token `B`, then an optional `","`, then a space, then `and` or `or`
/// (ASCII case-insensitive), then a space, then a token `C`. A token is a
/// maximal run of alphanumeric characters, `_`, `'` and `-`, and `A` starts
/// at a token boundary. The pattern is the crate's detector, and it accepts
/// known false positives such as "a, b and c" inside a code sample.
pub(crate) fn tricolon_frequency(
    subject: &Subject,
    max_per_100_sentences: f64,
    min_sentences: u32,
    min_words: u32,
) -> Verdict {
    let (_, body) = match measured(subject, min_words) {
        Ok(found) => found,
        Err(verdict) => return verdict,
    };
    let count = body.sentences.len();
    if count == 0 {
        return Verdict::Inconclusive {
            reason: "text has no sentence with words".to_string(),
        };
    }
    if count < min_sentences as usize {
        return Verdict::Inconclusive {
            reason: format!(
                "text has {count} sentence(s) with words, below the minimum of {min_sentences}"
            ),
        };
    }
    let offsets: Vec<usize> = body
        .sentences
        .iter()
        .filter(|sentence| has_tricolon(&body.chars[sentence.start..sentence.end]))
        .map(|sentence| sentence.start)
        .collect();
    let rate = offsets.len() as f64 * 100.0 / count as f64;
    if rate > max_per_100_sentences {
        Verdict::Failed {
            reason: format!(
                "{} tricolon sentence(s) at offset(s) {offsets:?} of {count}: {rate:.2} per 100 sentences exceeds {max_per_100_sentences:.2}",
                offsets.len()
            ),
        }
    } else {
        Verdict::Verified
    }
}

fn is_token(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\'' || c == '-'
}

fn token_end(chars: &[char], from: usize) -> usize {
    let mut end = from;
    while chars.get(end).is_some_and(|&c| is_token(c)) {
        end += 1;
    }
    end
}

fn word_at_ci(chars: &[char], at: usize, word: &str) -> bool {
    word.chars().enumerate().all(|(offset, w)| {
        chars
            .get(at + offset)
            .is_some_and(|c| c.eq_ignore_ascii_case(&w))
    })
}

fn tricolon_at(chars: &[char], start: usize) -> bool {
    let first_end = token_end(chars, start);
    if chars.get(first_end) != Some(&',') || chars.get(first_end + 1) != Some(&' ') {
        return false;
    }
    let second_start = first_end + 2;
    let second_end = token_end(chars, second_start);
    if second_end == second_start {
        return false;
    }
    let mut conjunction = second_end;
    if chars.get(conjunction) == Some(&',') {
        conjunction += 1;
    }
    if chars.get(conjunction) != Some(&' ') {
        return false;
    }
    let word_start = conjunction + 1;
    let conjunction_len = if word_at_ci(chars, word_start, "and") {
        3
    } else if word_at_ci(chars, word_start, "or") {
        2
    } else {
        return false;
    };
    if chars.get(word_start + conjunction_len) != Some(&' ') {
        return false;
    }
    let third_start = word_start + conjunction_len + 1;
    token_end(chars, third_start) > third_start
}

fn has_tricolon(chars: &[char]) -> bool {
    (0..chars.len()).any(|index| {
        is_token(chars[index])
            && (index == 0 || !is_token(chars[index - 1]))
            && tricolon_at(chars, index)
    })
}

/// Fails when any sentence opens with a listed opener. An opener matches when
/// its one to three words equal the sentence's first words, case-insensitively.
pub(crate) fn stock_opener(subject: &Subject, openers: &[String], min_words: u32) -> Verdict {
    let (_, body) = match measured(subject, min_words) {
        Ok(found) => found,
        Err(verdict) => return verdict,
    };
    let listed: Vec<(&str, Vec<String>)> = openers
        .iter()
        .map(|opener| (opener.as_str(), opener_words(opener)))
        .collect();
    let mut hits = Vec::new();
    for sentence in &body.sentences {
        let words: Vec<String> = sentence
            .words
            .iter()
            .map(|&span| span_text(&body.chars, span).to_lowercase())
            .collect();
        let found = (1..=words.len().min(3)).find_map(|count| {
            listed
                .iter()
                .find(|(_, tokens)| tokens.len() == count && tokens[..] == words[..count])
                .map(|(opener, _)| *opener)
        });
        if let Some(opener) = found {
            hits.push(format!(
                "sentence at offset {} opens with `{opener}`",
                sentence.start
            ));
        }
    }
    if hits.is_empty() {
        Verdict::Verified
    } else {
        Verdict::Failed {
            reason: format!("stock opener(s) found: {}", hits.join("; ")),
        }
    }
}

/// Fails when any claim annotation has no approved proof that matches it.
/// Value is compared as an exact decimal string after canonicalisation
/// (leading zeros and trailing fractional zeros removed), never as a float.
/// Unit and subject must be equal, and the date must fall inside the proof's
/// inclusive window. Malformed annotation values or dates fail.
pub(crate) fn claim_proof(subject: &Subject, proofs: &[ApprovedProof], min_words: u32) -> Verdict {
    let (text, _) = match measured(subject, min_words) {
        Ok(found) => found,
        Err(verdict) => return verdict,
    };
    let mut problems = Vec::new();
    for (index, claim) in text.claims.iter().enumerate() {
        let (Some(value), Some(date)) = (canonical_decimal(&claim.value), parse_date(&claim.date))
        else {
            problems.push(format!(
                "claim {index} has a value or date that is not a decimal or YYYY-MM-DD"
            ));
            continue;
        };
        let mut matched = false;
        let mut outside_window = false;
        for proof in proofs {
            if proof.unit != claim.unit || proof.subject != claim.subject {
                continue;
            }
            if canonical_decimal(&proof.value).as_deref() != Some(value.as_str()) {
                continue;
            }
            let (Some(from), Some(until)) = (
                parse_date(&proof.valid_from),
                parse_date(&proof.valid_until),
            ) else {
                continue;
            };
            if (from..=until).contains(&date) {
                matched = true;
                break;
            }
            outside_window = true;
        }
        if !matched {
            let why = if outside_window {
                "is outside the validity window of its approved proof"
            } else {
                "matches no approved proof on value, unit and subject"
            };
            problems.push(format!(
                "claim {index} (`{}` {} for `{}` on {}) {why}",
                claim.value, claim.unit, claim.subject, claim.date
            ));
        }
    }
    if problems.is_empty() {
        Verdict::Verified
    } else {
        Verdict::Failed {
            reason: problems.join("; "),
        }
    }
}

/// Canonical form of a plain decimal `-?[0-9]+(\.[0-9]+)?`: no leading zeros,
/// no trailing fractional zeros, and no sign on zero. `None` when the text
/// is not in that grammar.
pub(crate) fn canonical_decimal(raw: &str) -> Option<String> {
    let (negative, unsigned) = match raw.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, raw),
    };
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || fraction.is_some_and(|part| !digits(part)) {
        return None;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.unwrap_or("").trim_end_matches('0');
    let zero = whole == "0" && fraction.is_empty();
    let mut out = String::new();
    if negative && !zero {
        out.push('-');
    }
    out.push_str(whole);
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(fraction);
    }
    Some(out)
}

/// Parses `YYYY-MM-DD` into `yyyymmdd`, checking the month and day ranges.
pub(crate) fn parse_date(raw: &str) -> Option<u32> {
    if raw.len() != 10 || raw.as_bytes()[4] != b'-' || raw.as_bytes()[7] != b'-' {
        return None;
    }
    let part = |range: std::ops::Range<usize>| -> Option<u32> {
        let text = raw.get(range)?;
        if text.bytes().all(|b| b.is_ascii_digit()) {
            text.parse().ok()
        } else {
            None
        }
    };
    let year = part(0..4)?;
    let month = part(5..7)?;
    let day = part(8..10)?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if (1..=days).contains(&day) {
        Some(year * 10_000 + month * 100 + day)
    } else {
        None
    }
}
