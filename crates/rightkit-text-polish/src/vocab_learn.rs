//! Learn-from-edits vocabulary suggestions (suggest-only).
//!
//! Pure text diff over (delivered transcript, user-edited transcript): no IO,
//! no store access — callers decide persistence. Extracts proper-noun-shaped
//! corrections the user made by hand so they can be offered as vocabulary /
//! sound-alike entries.
//!
//! Token-level Levenshtein alignment with FOUR op kinds. `CaseFix`
//! (normalize-equal, casing/punct-only difference) is the discriminator that
//! keeps phrase corrections whole: classifying "flow" -> "Flow" as a plain
//! Match would split the "whisper flow" -> "Wispr Flow" run and yield only
//! "Wispr"; classifying it as Replace would glue garbage runs like "on Monday".
//! CaseFix joins runs (so a multi-word correction stays one suggestion) but only
//! becomes a standalone suggestion when `case_signal` says the token is
//! vocabulary-shaped (MixedCase, or non-stoplisted TitleCase mid-sentence — a
//! name correction like adrian -> Adrian, never Monday/Hello).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub term: String,
    pub heard_as: String,
}

/// Common English words + measured brand false-fire set + weekday/month names.
/// A single one of these is never a vocabulary correction on its own — its
/// mid-sentence capitalization is normal English, and using it as a sound-alike
/// alias would rewrite ordinary speech ("cut me some slack"). Shared with the
/// vocabulary alias guard so learned and manually-entered aliases agree.
pub(crate) const COMMON_WORD_STOPLIST: &[&str] = &[
    // measured brand false-fire set
    "slack",
    "whisper",
    "herd",
    "flow",
    "arc",
    "notion",
    "spark",
    "oracle",
    "craft",
    "bear",
    "signal",
    "teams",
    "brave",
    "edge",
    "code",
    "zed",
    "warp",
    "kitty",
    // weekdays + months (sentence-medial capitalization is not vocabulary)
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    // top common English words
    "the",
    "a",
    "an",
    "and",
    "or",
    "but",
    "if",
    "then",
    "so",
    "because",
    "as",
    "of",
    "at",
    "by",
    "for",
    "with",
    "about",
    "against",
    "between",
    "into",
    "through",
    "during",
    "before",
    "after",
    "above",
    "below",
    "to",
    "from",
    "up",
    "down",
    "in",
    "out",
    "on",
    "off",
    "over",
    "under",
    "again",
    "further",
    "once",
    "here",
    "there",
    "when",
    "where",
    "why",
    "how",
    "all",
    "any",
    "both",
    "each",
    "few",
    "more",
    "most",
    "other",
    "some",
    "such",
    "no",
    "nor",
    "not",
    "only",
    "own",
    "same",
    "than",
    "too",
    "very",
    "can",
    "will",
    "just",
    "should",
    "now",
    "i",
    "me",
    "my",
    "we",
    "us",
    "our",
    "you",
    "your",
    "he",
    "him",
    "his",
    "she",
    "her",
    "it",
    "its",
    "they",
    "them",
    "their",
    "this",
    "that",
    "these",
    "those",
    "am",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "being",
    "have",
    "has",
    "had",
    "do",
    "does",
    "did",
    "would",
    "could",
    "should",
    "shall",
    "may",
    "might",
    "must",
    "hello",
    "hi",
    "hey",
    "yes",
    "okay",
    "ok",
    "please",
    "thanks",
    "thank",
    "let",
    "get",
    "got",
    "make",
    "made",
    "send",
    "sent",
    "ship",
    "meet",
    "call",
    "email",
    "message",
    "text",
    "note",
    "add",
    "fix",
    "check",
    "use",
    "used",
    "need",
    "want",
    "like",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "today",
    "tomorrow",
    "yesterday",
    "week",
    "month",
    "year",
    "day",
    "time",
    "noon",
    "morning",
    "night",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Match(usize, usize),
    Replace(usize, usize),
    CaseFix(usize, usize),
    Insert(usize),
    Delete(usize),
}

/// Compare the delivered transcript with the user's edited version and extract
/// proper-noun-shaped corrections. Conservative: a >40% semantic-token change
/// ratio means the user rewrote rather than corrected, and nothing is
/// suggested; pure insert/delete runs (added/removed words) are not corrections.
pub fn suggestion_candidates(delivered: &str, edited: &str) -> Vec<Suggestion> {
    let a: Vec<&str> = delivered.split_whitespace().collect();
    let b: Vec<&str> = edited.split_whitespace().collect();
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let ops = align_tokens(&a, &b);
    // Rewrite guard counts SEMANTIC changes only (Replace/Insert/Delete — a
    // casing fix is not a content edit).
    let changed = ops
        .iter()
        .filter(|o| matches!(o, Op::Replace(..) | Op::Insert(..) | Op::Delete(..)))
        .count();
    if changed * 10 > a.len().max(b.len()) * 4 {
        return Vec::new();
    }
    let mut out: Vec<Suggestion> = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        if matches!(ops[i], Op::Match(..)) {
            i += 1;
            continue;
        }
        // Consume a maximal contiguous non-Match run.
        let mut j = i;
        let (mut heard_toks, mut fixed_toks) = (Vec::new(), Vec::new());
        let mut has_replace = false;
        let mut case_fixes: Vec<(usize, usize)> = Vec::new();
        while j < ops.len() && !matches!(ops[j], Op::Match(..)) {
            match ops[j] {
                Op::Replace(ai, bi) => {
                    has_replace = true;
                    heard_toks.push(a[ai]);
                    fixed_toks.push(b[bi]);
                }
                Op::CaseFix(ai, bi) => {
                    case_fixes.push((ai, bi));
                    heard_toks.push(a[ai]);
                    fixed_toks.push(b[bi]);
                }
                // Pure insert/delete = words the user ADDED or REMOVED, not part
                // of the correction itself — they must not join the phrase or they
                // corrupt the heard/fixed alignment (a trailing "please" deletion
                // pushed "whisper flow please" past the edit-distance budget).
                Op::Delete(..) | Op::Insert(..) => {}
                Op::Match(..) => unreachable!(),
            }
            j += 1;
        }
        if has_replace {
            // Phrase correction: a Replace anchors it; CaseFix neighbors ride along.
            if !heard_toks.is_empty()
                && !fixed_toks.is_empty()
                && heard_toks.len() <= 3
                && fixed_toks.len() <= 3
            {
                let heard = heard_toks.join(" ");
                let fixed = fixed_toks.join(" ");
                if vocabulary_shaped(&heard, &fixed) {
                    out.push(Suggestion {
                        term: strip_terminal_punct(&fixed),
                        heard_as: strip_terminal_punct(&heard).to_lowercase(),
                    });
                }
            }
        } else {
            // CaseFix-only run: per-token name/brand casing corrections.
            for (ai, bi) in case_fixes {
                if case_signal(&b, bi) {
                    out.push(Suggestion {
                        term: strip_terminal_punct(b[bi]),
                        heard_as: strip_terminal_punct(a[ai]).to_lowercase(),
                    });
                }
            }
        }
        i = j;
    }
    out
}

/// Token-level Levenshtein DP + backtrace. The diagonal cost treats
/// normalize-equal pairs as free (0) so the DP prefers aligning casing variants
/// over insert/delete splits; the backtrace classifies each diagonal as Match
/// (identical), CaseFix (casing/punct-only difference), or Replace. Noise like
/// "monday" -> "Monday" is a CaseFix here but is filtered out at emit time by
/// `case_signal` (stoplist + sentence-position check), so it never becomes a
/// suggestion and cannot glue onto an adjacent Insert.
fn align_tokens(a: &[&str], b: &[&str]) -> Vec<Op> {
    let n = a.len();
    let m = b.len();
    let diag_cost = |i: usize, j: usize| -> usize {
        if a[i - 1] == b[j - 1] || normalize(a[i - 1]) == normalize(b[j - 1]) {
            0
        } else {
            1
        }
    };
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + diag_cost(i, j));
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && dp[i][j] == dp[i - 1][j - 1] + diag_cost(i, j) {
            let (ta, tb) = (a[i - 1], b[j - 1]);
            ops.push(if ta == tb {
                Op::Match(i - 1, j - 1)
            } else if normalize(ta) == normalize(tb) {
                // Casing/punct-only difference. Classify as CaseFix so a phrase
                // like "whisper flow" -> "Wispr Flow" stays one run; whether a
                // CaseFix-only run actually SUGGESTS anything is decided later by
                // `case_signal` (which stoplists Monday/Hello and requires
                // MixedCase or non-common TitleCase).
                Op::CaseFix(i - 1, j - 1)
            } else {
                Op::Replace(i - 1, j - 1)
            });
            i -= 1;
            j -= 1;
        } else if j > 0 && dp[i][j] == dp[i][j - 1] + 1 {
            ops.push(Op::Insert(j - 1));
            j -= 1;
        } else {
            ops.push(Op::Delete(i - 1));
            i -= 1;
        }
    }
    ops.reverse();
    ops
}

fn normalize(t: &str) -> String {
    t.trim_matches(|c: char| c.is_ascii_punctuation())
        .to_lowercase()
}

fn strip_terminal_punct(t: &str) -> String {
    t.trim_matches(|c: char| c.is_ascii_punctuation() && c != '-' && c != '@')
        .to_string()
}

fn mixed_case(t: &str) -> bool {
    let has_inner_upper = t.chars().skip(1).any(char::is_uppercase);
    let has_lower = t.chars().any(char::is_lowercase);
    has_inner_upper && has_lower
}

fn title_case(t: &str) -> bool {
    let mut chars = t.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_uppercase()
        && !t.chars().skip(1).any(char::is_uppercase)
        && t.chars().any(char::is_lowercase)
}

fn sentence_initial(b: &[&str], i: usize) -> bool {
    i == 0 || b[i - 1].ends_with(['.', '!', '?'])
}

/// Is a standalone casing fix vocabulary-shaped? MixedCase always is
/// (acmecorp -> AcmeCorp). TitleCase only when NOT at sentence position and
/// NOT a common word — adrian -> Adrian yes; Monday / Hello no.
fn case_signal(b: &[&str], bi: usize) -> bool {
    let t = b[bi];
    if mixed_case(t) {
        return true;
    }
    title_case(t)
        && !sentence_initial(b, bi)
        && !COMMON_WORD_STOPLIST.contains(&normalize(t).as_str())
}

fn vocabulary_shaped(heard: &str, fixed: &str) -> bool {
    let h = normalize(heard);
    let f = normalize(fixed);
    if h == f {
        return fixed.split_whitespace().any(mixed_case)
            || fixed
                .split_whitespace()
                .any(|w| w.chars().next().is_some_and(char::is_uppercase));
    }
    let dist = strsim_levenshtein(&h, &f);
    let capitalized = fixed
        .split_whitespace()
        .any(|w| w.chars().next().is_some_and(char::is_uppercase));
    capitalized && dist <= 1 + h.len().max(f.len()) / 4
}

fn strsim_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
#[path = "vocab_learn_tests.rs"]
mod tests;
