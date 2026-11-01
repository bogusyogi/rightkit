//! Word helpers shared by the detectors.

use rightkit_transcript::Transcript;

/// A transcript word as the detectors see it.
#[derive(Debug, Clone)]
pub(crate) struct Wd {
    pub text: String,
    pub norm: String,
    pub start: i64,
    pub end: i64,
    pub conf: Option<f32>,
}

pub(crate) fn words_of(t: &Transcript) -> Vec<Wd> {
    t.words
        .iter()
        .map(|w| Wd {
            text: w.text.clone(),
            norm: normalize(&w.text),
            start: w.start_ms as i64,
            end: w.end_ms as i64,
            conf: w.confidence,
        })
        .collect()
}

/// Lowercase, fold curly apostrophes, trim non-alphanumerics from both ends.
pub(crate) fn normalize(raw: &str) -> String {
    raw.to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

pub(crate) fn ends_sentence(raw: &str) -> bool {
    raw.trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}'])
        .ends_with(['.', '?', '!'])
}

/// Trailing dash or ellipsis: the speaker broke off.
pub(crate) fn ends_interrupted(raw: &str) -> bool {
    let t = raw.trim_end_matches(['"', '\'', ')', ']']);
    t.ends_with(['-', '\u{2013}', '\u{2014}', '\u{2026}']) || t.ends_with("...")
}

pub(crate) fn ends_soft(raw: &str) -> bool {
    raw.trim_end_matches(['"', '\'', ')', ']'])
        .ends_with([',', ';', ':'])
}

const DANGLING: &[&str] = &[
    "the", "a", "an", "and", "but", "or", "so", "because", "if", "that", "which", "when", "while",
    "to", "of", "in", "on", "at", "for", "with", "from", "by", "about", "into", "is", "are", "was",
    "were", "be", "my", "your", "our", "their", "this", "these", "those", "as", "than", "then",
    "i", "we", "you", "they", "it", "he", "she",
];

/// A word a finished sentence does not end on.
pub(crate) fn is_dangling(norm: &str) -> bool {
    DANGLING.contains(&norm)
}

/// Spoken hesitation sounds, skipped when matching repeated material.
pub(crate) fn is_hesitation(norm: &str) -> bool {
    matches!(
        norm,
        "um" | "umm"
            | "uh"
            | "uhh"
            | "uhm"
            | "erm"
            | "er"
            | "err"
            | "ah"
            | "ahh"
            | "hmm"
            | "hmmm"
            | "hm"
            | "mm"
    )
}

fn lcs(a: &[&str], b: &[&str]) -> usize {
    let mut prev = vec![0u16; b.len() + 1];
    let mut cur = vec![0u16; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] as usize
}

/// LCS Dice similarity of two sequences (0 when either is empty).
pub(crate) fn dice(a: &[&str], b: &[&str]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    2.0 * lcs(a, b) as f32 / (a.len() + b.len()) as f32
}

pub(crate) fn join(words: &[Wd], from: usize, end: usize) -> String {
    words[from..end]
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Index of the first word whose start is at or after `ms`.
pub(crate) fn first_word_at(words: &[Wd], ms: i64) -> usize {
    words.partition_point(|w| w.start < ms)
}

/// Whether `phrase` (normalised words) occurs at position `pos` of `idx`
/// (indices into `words`, already filtered).
pub(crate) fn phrase_at(words: &[Wd], idx: &[usize], pos: usize, phrase: &[String]) -> bool {
    pos + phrase.len() <= idx.len()
        && phrase
            .iter()
            .enumerate()
            .all(|(k, p)| words[idx[pos + k]].norm == *p)
}

pub(crate) fn phrase_words(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(normalize)
        .filter(|t| !t.is_empty())
        .collect()
}

pub(crate) fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
