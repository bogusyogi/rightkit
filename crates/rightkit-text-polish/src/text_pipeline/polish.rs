use super::{control::*, inline::*, normalize::*, number_words::*};
use regex::Captures;
use std::borrow::Cow;

fn normalize_numbers(text: &str) -> String {
    let hundreds = re(
        r"(?i)\b(one|two|three|four|five|six|seven|eight|nine)\s+hundred(?:\s+(twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety)(?:\s+(one|two|three|four|five|six|seven|eight|nine))?|\s+(one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen))?\b",
    );
    let out = hundreds.replace_all(text, |caps: &Captures| {
        let base = word_value(&caps[1]).unwrap_or(0) * 100;
        let tail = caps
            .get(2)
            .and_then(|m| word_value(m.as_str()))
            .unwrap_or(0)
            + caps
                .get(3)
                .and_then(|m| word_value(m.as_str()))
                .unwrap_or(0)
            + caps
                .get(4)
                .and_then(|m| word_value(m.as_str()))
                .unwrap_or(0);
        (base + tail).to_string()
    });

    let spoken_hundreds = re(
        r"(?i)\b(one|two|three|four|five|six|seven|eight|nine)\s+(twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety)(?:\s+(one|two|three|four|five|six|seven|eight|nine))\b",
    );
    let out = spoken_hundreds.replace_all(&out, |caps: &Captures| {
        let n = word_value(&caps[1]).unwrap_or(0) * 100
            + word_value(&caps[2]).unwrap_or(0)
            + word_value(&caps[3]).unwrap_or(0);
        n.to_string()
    });

    let tens = re(
        r"(?i)\b(twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety)(?:[\s-]+(one|two|three|four|five|six|seven|eight|nine))?\b",
    );
    tens.replace_all(&out, |caps: &Captures| {
        let n = word_value(&caps[1]).unwrap_or(0)
            + caps
                .get(2)
                .and_then(|m| word_value(m.as_str()))
                .unwrap_or(0);
        n.to_string()
    })
    .into_owned()
}

/// Spoken integers 0–19 → digits (larger numbers are handled by normalize_numbers).
/// "one" is guarded because it doubles as a pronoun ("no one", "the one", "one of");
/// the other small words aren't idiomatic, so they always become digits. Bias is
/// toward KEEPING "one" as a word when ambiguous (a stray "1" reads worse than a
/// spelled "one").
fn normalize_small_numbers(text: &str) -> String {
    let re_small = re(
        r"(?i)\b(zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen)\b",
    );
    re_small
        .replace_all(text, |caps: &Captures| {
            let m = caps.get(0).unwrap();
            let word = caps[1].to_ascii_lowercase();
            if word == "one" && one_is_idiomatic(text, m.start(), m.end()) {
                return caps[0].to_string();
            }
            word_value(&word)
                .map(|v| v.to_string())
                .unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

/// Whether a "one" at `text[start..end]` reads as the pronoun rather than a count.
fn one_is_idiomatic(text: &str, start: usize, end: usize) -> bool {
    let word_split = |c: char| !c.is_alphanumeric() && c != '\'';
    let prev = text[..start]
        .split(word_split)
        .rfind(|s| !s.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();
    let next = text[end..]
        .split(word_split)
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Determiners/pronoun-makers before, or "of"/"another" after.
    const PROTECT_PREV: [&str; 12] = [
        "no", "the", "this", "that", "these", "those", "which", "any", "every", "each", "only",
        "another",
    ];
    const PROTECT_NEXT: [&str; 2] = ["of", "another"];
    PROTECT_PREV.contains(&prev.as_str()) || PROTECT_NEXT.contains(&next.as_str())
}

fn apply_lexicon(text: &str) -> String {
    let replacements = [
        (r"(?i)\btsf\b", "TSF"),
        (r"(?i)\bt\s*s\s*f\b", "TSF"),
        (r"(?i)\bu\s*i\b", "UI"),
        (r"(?i)\bu\s*x\b", "UX"),
        (r"(?i)\ba\s*p\s*i\b", "API"),
        (r"(?i)\bo\s*n\s*n\s*x\b", "ONNX"),
        (r"(?i)\bo\s*r\s*t\b", "ORT"),
        (r"(?i)\brnnt\b", "RNN-T"),
        (r"(?i)\br\s*n\s*n\s*t\b", "RNN-T"),
        (r"(?i)\brnn[\s-]?t\b", "RNN-T"),
        (r"(?i)\btdt\b", "TDT"),
        (r"(?i)\bt\s*d\s*t\b", "TDT"),
        (r"(?i)\bdml\b", "DML"),
        (r"(?i)\basr\b", "ASR"),
        (r"(?i)\bkws\b", "KWS"),
        (r"(?i)\bgpu\b", "GPU"),
        (r"(?i)\bcpu\b", "CPU"),
        (r"(?i)\bapi\b", "API"),
        (r"(?i)\bui\b", "UI"),
        (r"(?i)\bux\b", "UX"),
        (r"(?i)\bonnx\b", "ONNX"),
        (r"(?i)\bort\b", "ORT"),
        (r"(?i)\bjson\b", "JSON"),
        (r"(?i)\bwav\b", "WAV"),
        (r"(?i)\bnew york\b", "New York"),
        (r"(?i)\btuesday\b", "Tuesday"),
        (r"(?i)\bwispr flow\b", "Wispr Flow"),
        (r"(?i)\bsuperwhisper\b", "Superwhisper"),
        (r"(?i)\bbandra\b", "Bandra"),
        (r"(?i)\bmumbai\b", "Mumbai"),
        (r"(?i)\bkoregaon\b", "Koregaon"),
        (r"(?i)\bdji mic mini\b", "DJI Mic Mini"),
        (r"(?i)\brtx\s+4070\b", "RTX 4070"),
        (r"(?i)\bwhisper\b", "Whisper"),
        (r"(?i)\bllama\b", "Llama"),
        (r"(?i)\bollama\b", "Ollama"),
        (r"(?i)\bgemma\b", "Gemma"),
        (r"(?i)\bparakeet\b", "Parakeet"),
        (r"(?i)\bzephyr\b", "Zephyr"),
        // Unambiguous suite-wide brand proper nouns (no common-word collision).
        (r"(?i)\bsquarespace\b", "Squarespace"),
        (r"(?i)\bsquare space\b", "Squarespace"),
        (r"(?i)\btiktok\b", "TikTok"),
        (r"(?i)\btik tok\b", "TikTok"),
        (r"(?i)\byoutube\b", "YouTube"),
        (r"(?i)\binstagram\b", "Instagram"),
        (r"(?i)\boracle\b", "Oracle"),
        (r"(?i)\bnetflix\b", "Netflix"),
        (r"(?i)\bhuawei\b", "Huawei"),
    ];
    // Cow-threaded: ~40 narrow lexicon patterns, almost none of which match any
    // given utterance — only reassign (and allocate) on an actual hit instead of
    // cloning the whole string on every no-op pass.
    let mut out: Cow<str> = Cow::Borrowed(text);
    for (pattern, replacement) in replacements {
        if let Cow::Owned(s) = re(pattern).replace_all(&out, replacement) {
            out = Cow::Owned(s);
        }
    }
    out.into_owned()
}

/// How one brand homophone decides to keep its literal reading instead of being repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomophoneGuard<'a> {
    /// Repair unless the previous word is one of these (lowercase), e.g. the pronoun idiom
    /// "I heard right".
    LiteralAfter(&'a [&'a str]),
    /// Repair unless the next word is one of these (lowercase), e.g. "herd right there".
    LiteralBefore(&'a [&'a str]),
    /// The literal reading is the common one: repair only when the sentence carries one of the
    /// [`BrandHomophones::context_terms`].
    NeedsBrandContext,
    /// Always repair (the phrase has no plausible literal reading).
    Always,
}

/// Multi-word ASR corruptions of a product or brand name, repaired to the exact brand spelling.
///
/// A token-level context bias can only nudge single tokens, so a brand that decodes as two
/// ordinary words ("heard right", "her drive") needs a phrase-level repair. Every phrase carries
/// a [`HomophoneGuard`]: a missed repair is far cheaper than corrupting a correct sentence, so
/// ambiguous forms must clear a higher bar.
#[derive(Debug, Clone, Copy)]
pub struct BrandHomophones<'a> {
    /// The exact spelling every repaired phrase becomes.
    pub brand: &'a str,
    /// Lowercase, word-boundary-anchored phrases and their guards.
    pub phrases: &'a [(&'a str, HomophoneGuard<'a>)],
    /// Lowercase words that are independent evidence the speaker is talking about the product
    /// (used by [`HomophoneGuard::NeedsBrandContext`]).
    pub context_terms: &'a [&'a str],
}

impl BrandHomophones<'_> {
    /// True to keep the literal reading (skip the repair).
    fn keep_literal(
        &self,
        guard: HomophoneGuard<'_>,
        prev: &str,
        next: &str,
        sentence: &str,
    ) -> bool {
        match guard {
            HomophoneGuard::LiteralAfter(words) => words.contains(&prev),
            HomophoneGuard::LiteralBefore(words) => words.contains(&next),
            HomophoneGuard::NeedsBrandContext => !self.has_brand_context(sentence),
            HomophoneGuard::Always => false,
        }
    }

    /// True when the sentence contains independent evidence that the speaker is talking about
    /// the product. Word-boundary matched so "happier" cannot satisfy "app".
    fn has_brand_context(&self, sentence: &str) -> bool {
        sentence
            .split(|c: char| !c.is_alphanumeric() && c != '\'')
            .filter(|w| !w.is_empty())
            .any(|word| {
                let lowered = word.to_ascii_lowercase();
                self.context_terms.contains(&lowered.as_str())
            })
    }
}

/// Repair brand homophones in `text` according to `rules`.
pub fn apply_brand_homophones(text: &str, rules: &BrandHomophones<'_>) -> String {
    let word_split = |c: char| !c.is_alphanumeric() && c != '\'';
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    // Collect all candidate matches across every homophone form, then process
    // them in source order so overlapping/adjacent phrases never double-fire.
    let mut matches: Vec<(usize, usize, HomophoneGuard<'_>)> = Vec::new();
    for (phrase, guard) in rules.phrases {
        let pattern = format!(r"(?i)\b{}\b", regex::escape(phrase));
        for m in re(&pattern).find_iter(text) {
            matches.push((m.start(), m.end(), *guard));
        }
    }
    matches.sort_by_key(|&(start, _, _)| start);
    for (start, end, guard) in matches {
        if start < last {
            continue; // already covered by an earlier, overlapping match
        }
        out.push_str(&text[last..start]);
        let prev = text[..start]
            .split(word_split)
            .rfind(|s| !s.is_empty())
            .unwrap_or("")
            .to_ascii_lowercase();
        let next = text[end..]
            .split(word_split)
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_ascii_lowercase();
        // Sentence containing the match, used by the context-evidence forms.
        // Bounded by terminal punctuation on either side so brand evidence from
        // a neighbouring sentence cannot license a repair here.
        let sentence_start = text[..start]
            .rfind(['.', '!', '?', '\n'])
            .map_or(0, |i| i + 1);
        let sentence_end = text[end..]
            .find(['.', '!', '?', '\n'])
            .map_or(text.len(), |i| end + i);
        let sentence = &text[sentence_start..sentence_end];
        if rules.keep_literal(guard, &prev, &next, sentence) {
            out.push_str(&text[start..end]); // keep the literal reading verbatim
        } else {
            out.push_str(rules.brand);
        }
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

fn casing_rules(text: &str, capitalize_start: bool) -> String {
    let out = re(r"\bi\b").replace_all(text, "I");
    let out = re(r"(?i)\bi('(?:m|ve|ll|d|s|re))\b").replace_all(&out, "I$1");
    let mut out = re(r"([.!?]\s+)([a-z])")
        .replace_all(&out, |caps: &Captures| {
            format!("{}{}", &caps[1], caps[2].to_ascii_uppercase())
        })
        .into_owned();
    if capitalize_start {
        let starts_with_email = out
            .split_whitespace()
            .next()
            .is_some_and(|token| token.contains('@'));
        if !starts_with_email {
            if let Some(first) = out.chars().next() {
                if first.is_ascii_lowercase() {
                    out.replace_range(0..first.len_utf8(), &first.to_ascii_uppercase().to_string());
                }
            }
        }
    }
    out
}

/// Deterministic local (L0) polish of a finished transcript: inline edits, repetition collapse,
/// spoken formatting, punctuation spacing, numbers, dates, units, casing and a small proper-noun
/// lexicon. No brand repair; see [`deterministic_polish_with_brand`].
pub fn deterministic_polish(text: &str) -> String {
    deterministic_polish_inner(text, true, None)
}

/// [`deterministic_polish`] plus the host's brand homophone repair, applied right after the
/// lexicon so casing rules cannot undo the brand spelling.
pub fn deterministic_polish_with_brand(text: &str, brand: Option<&BrandHomophones<'_>>) -> String {
    deterministic_polish_inner(text, true, brand)
}

/// Restore saved vocabulary terms to their exact spelling/casing. Vocabulary biases
/// ASR recognition, but the local polish (deterministic + Harper spelling) can still
/// lowercase or mis-case a known term ("cloudflare" when the user saved "Cloudflare").
/// Replace whole-word, case-insensitive matches with the saved form. Longest terms
/// first so a multi-word term ("Damned Designs") wins over its sub-words. Local only —
/// this never leaves the device (belongs at L0/L1, not the cloud L2/L3 context).
pub fn restore_vocabulary_casing(text: &str, terms: &[String]) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let mut sorted: Vec<&str> = terms
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    sorted.sort_by_key(|t| std::cmp::Reverse(t.len()));
    let keys: Vec<String> = sorted.iter().map(|term| (*term).to_string()).collect();
    let regexes = regexes_for_keys(&VOCABULARY_REGEX_CACHE, &keys);
    // Cow-threaded: most saved vocab terms don't appear in any given utterance —
    // only reassign (and allocate) when a term actually matched.
    let mut out: Cow<str> = Cow::Borrowed(text);
    for (term, pattern) in sorted.into_iter().zip(regexes) {
        if let Cow::Owned(s) = pattern.replace_all(&out, |caps: &regex::Captures| {
            // Only rewrite when the match differs (avoid needless allocations and
            // leave an already-correct term untouched).
            if &caps[0] == term {
                caps[0].to_string()
            } else {
                term.to_string()
            }
        }) {
            out = Cow::Owned(s);
        }
    }
    out.into_owned()
}

/// Apply the user's deterministic text replacements: case-insensitive,
/// whole-word/phrase, longest key first (so a multi-word key wins over a
/// shorter overlapping key). The repeated-mishear lever — "always fix X to Y" — separate from
/// vocabulary (spelling guidance only) and snippets (trigger expansion).
/// Local only; runs at L0/L1 before anything reaches an LLM.
pub fn apply_replacements(
    text: &str,
    replacements: &std::collections::HashMap<String, String>,
) -> String {
    if text.is_empty() || replacements.is_empty() {
        return text.to_string();
    }
    let mut sorted: Vec<(&str, &str)> = replacements
        .iter()
        .map(|(k, v)| (k.trim(), v.as_str()))
        .filter(|(k, _)| !k.is_empty())
        .collect();
    sorted.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    let keys: Vec<String> = sorted.iter().map(|(from, _)| (*from).to_string()).collect();
    let regexes = regexes_for_keys(&REPLACEMENT_REGEX_CACHE, &keys);
    // Cow-threaded: same reasoning as restore_vocabulary_casing — most saved
    // replacement keys don't appear in any given utterance.
    let mut out: Cow<str> = Cow::Borrowed(text);
    for ((_, to), pattern) in sorted.into_iter().zip(regexes) {
        // NoExpand: the replacement is literal text, `$` must not expand.
        if let Cow::Owned(s) = pattern.replace_all(&out, regex::NoExpand(to)) {
            out = Cow::Owned(s);
        }
    }
    out.into_owned()
}

/// [`deterministic_polish`] for a continuation chunk: the start is not capitalized.
pub fn deterministic_polish_tail(text: &str) -> String {
    deterministic_polish_inner(text, false, None)
}

/// [`deterministic_polish_tail`] plus the host's brand homophone repair.
pub fn deterministic_polish_tail_with_brand(
    text: &str,
    brand: Option<&BrandHomophones<'_>>,
) -> String {
    deterministic_polish_inner(text, false, brand)
}

/// Add cheap visual breaks to a long prose transcript without pretending to
/// understand its structure. Existing line breaks stay intact; new breaks are
/// inserted after complete sentences near a 30-word target. This is the L0
/// wall-of-text guard, not semantic paragraphing.
///
/// Commands and code-like/path-like text are left untouched. An exceptionally
/// long ASR run-on may be split at a comma or semicolon near the same target.
pub fn visual_chunk_transcript(text: &str) -> String {
    if text.trim().is_empty() || should_skip_visual_chunking(text) {
        return text.to_string();
    }
    text.split('\n')
        .map(chunk_visual_line)
        .collect::<Vec<_>>()
        .join("\n")
}

const VISUAL_CHUNK_TARGET_WORDS: usize = 30;
const VISUAL_CHUNK_MIN_WORDS: usize = 20;
const VISUAL_CHUNK_SOFT_MAX_WORDS: usize = 40;
const VISUAL_CHUNK_LONG_SENTENCE_WORDS: usize = 45;
const VISUAL_CHUNK_EMERGENCY_WORDS: usize = 60;

fn should_skip_visual_chunking(text: &str) -> bool {
    let trimmed = text.trim();
    if rightkit_voice_commands::command_recognition::recognize_command(trimmed).is_some()
        || rightkit_voice_commands::command_recognition::app_launch_query(trimmed).is_some()
    {
        return true;
    }
    if trimmed.contains('`') || trimmed.contains("://") {
        return true;
    }
    for line in text.lines().map(str::trim_start) {
        if line
            .chars()
            .next()
            .is_some_and(|character| matches!(character, '$' | '>' | '#'))
            || line.starts_with(">>>")
            || line.starts_with("PS ")
        {
            return true;
        }
        let lower = line.to_ascii_lowercase();
        if [
            "git ", "cargo ", "npm ", "pnpm ", "yarn ", "python ", "python3 ", "rustc ", "curl ",
            "wget ", "ssh ", "docker ", "kubectl ", "cd ", "mkdir ", "rm ", "cp ", "mv ", "let ",
            "const ", "import ", "export ", "fn ", "class ", "def ", "return ",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            return true;
        }
        if line.split_whitespace().any(|token| {
            token.starts_with("/")
                || token.starts_with("./")
                || token.starts_with("../")
                || token.starts_with("~/")
                || (token.len() >= 3
                    && token.as_bytes()[0].is_ascii_alphabetic()
                    && token.as_bytes()[1] == b':'
                    && matches!(token.as_bytes()[2], b'\\' | b'/'))
        }) {
            return true;
        }
    }
    false
}

fn chunk_visual_line(line: &str) -> String {
    let mut sentence_ends = Vec::new();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    for (position, &(byte_index, character)) in chars.iter().enumerate() {
        if !matches!(character, '.' | '?' | '!') {
            continue;
        }
        if character == '.' {
            let previous = position
                .checked_sub(1)
                .and_then(|i| chars.get(i))
                .map(|(_, c)| *c);
            let next = chars.get(position + 1).map(|(_, c)| *c);
            if previous.is_some_and(|c| c.is_ascii_digit())
                && next.is_some_and(|c| c.is_ascii_digit())
            {
                continue;
            }
        }
        let mut end = byte_index + character.len_utf8();
        while let Some((_, closing)) = line[end..].char_indices().next() {
            if !matches!(
                closing,
                '"' | '\'' | ')' | ']' | '}' | '\u{201D}' | '\u{2019}'
            ) {
                break;
            }
            end += closing.len_utf8();
        }
        if end == line.len() || line[end..].chars().next().is_some_and(char::is_whitespace) {
            sentence_ends.push(end);
        }
    }

    if sentence_ends.is_empty() {
        return split_unpunctuated_line(line);
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut start = 0;
    for end in sentence_ends {
        let sentence = line[start..end].trim();
        if sentence.is_empty() {
            start = end;
            continue;
        }
        start = end;
        accumulate_chunk_pieces(split_long_sentence(sentence), &mut current, &mut chunks);
    }
    let remainder = line[start..].trim();
    if !remainder.is_empty() {
        accumulate_chunk_pieces(split_long_sentence(remainder), &mut current, &mut chunks);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.len() < 2 {
        line.to_string()
    } else {
        chunks.join("\n")
    }
}

/// Fold a run of split sentence pieces into `current`/`chunks`, flushing
/// `current` into `chunks` whenever it hits the target/soft-max word
/// thresholds. Shared by the main per-sentence loop and the trailing
/// remainder handling in `chunk_visual_line`, which apply identical
/// accumulation logic to two different piece sources.
fn accumulate_chunk_pieces(pieces: Vec<String>, current: &mut String, chunks: &mut Vec<String>) {
    for piece in pieces {
        let piece_words = visual_word_count(&piece);
        let current_words = visual_word_count(current);
        let would_exceed_soft_max = current_words + piece_words > VISUAL_CHUNK_SOFT_MAX_WORDS;
        if !current.is_empty()
            && (current_words >= VISUAL_CHUNK_TARGET_WORDS
                || (current_words >= VISUAL_CHUNK_MIN_WORDS && would_exceed_soft_max))
        {
            chunks.push(std::mem::take(current));
        }
        append_visual_piece(current, &piece);
        let chunk_words = visual_word_count(current);
        if chunk_words >= VISUAL_CHUNK_TARGET_WORDS
            || piece_words >= VISUAL_CHUNK_LONG_SENTENCE_WORDS
        {
            chunks.push(std::mem::take(current));
        }
    }
}

fn append_visual_piece(target: &mut String, piece: &str) {
    if !target.is_empty() {
        target.push(' ');
    }
    target.push_str(piece);
}

fn visual_word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn split_long_sentence(sentence: &str) -> Vec<String> {
    if visual_word_count(sentence) <= VISUAL_CHUNK_EMERGENCY_WORDS {
        return vec![sentence.to_string()];
    }
    let mut pieces = Vec::new();
    let mut remaining = sentence.trim();
    while visual_word_count(remaining) > VISUAL_CHUNK_EMERGENCY_WORDS {
        let Some(break_at) = secondary_punctuation_break(remaining) else {
            break;
        };
        let piece = remaining[..break_at].trim();
        if piece.is_empty() {
            break;
        }
        pieces.push(piece.to_string());
        remaining = remaining[break_at..].trim_start();
    }
    if !remaining.is_empty() {
        pieces.push(remaining.to_string());
    }
    if pieces.len() < 2 {
        vec![sentence.to_string()]
    } else {
        pieces
    }
}

/// Return a comma/semicolon boundary nearest the visual target, while keeping
/// both sides large enough to remain readable chunks.
fn secondary_punctuation_break(text: &str) -> Option<usize> {
    let total_words = visual_word_count(text);
    let mut words_seen: usize = 0;
    let mut in_word = false;
    let mut best: Option<(usize, usize)> = None;
    for (byte_index, character) in text.char_indices() {
        if character.is_whitespace() {
            in_word = false;
        } else if !in_word {
            words_seen += 1;
            in_word = true;
        }
        if !matches!(character, ',' | ';')
            || words_seen < VISUAL_CHUNK_MIN_WORDS
            || total_words.saturating_sub(words_seen) < VISUAL_CHUNK_MIN_WORDS
        {
            continue;
        }
        let distance = words_seen.abs_diff(VISUAL_CHUNK_TARGET_WORDS);
        let end = byte_index + character.len_utf8();
        if best.is_none_or(|(_, best_distance)| distance < best_distance) {
            best = Some((end, distance));
        }
    }
    best.map(|(break_at, _)| break_at)
}

fn split_unpunctuated_line(line: &str) -> String {
    let word_count = visual_word_count(line);
    if word_count <= VISUAL_CHUNK_EMERGENCY_WORDS {
        return line.to_string();
    }
    let mut pieces = Vec::new();
    let mut remaining = line.trim();
    while visual_word_count(remaining) > VISUAL_CHUNK_EMERGENCY_WORDS {
        let Some(break_at) =
            secondary_punctuation_break(remaining).or_else(|| whitespace_break(remaining))
        else {
            break;
        };
        let piece = remaining[..break_at].trim();
        if piece.is_empty() {
            break;
        }
        pieces.push(piece.to_string());
        remaining = remaining[break_at..].trim_start();
    }
    if !remaining.is_empty() {
        pieces.push(remaining.to_string());
    }
    if pieces.len() < 2 {
        line.to_string()
    } else {
        pieces.join("\n")
    }
}

fn whitespace_break(text: &str) -> Option<usize> {
    let mut words_seen: usize = 0;
    let mut in_word = false;
    let mut best: Option<(usize, usize)> = None;
    for (byte_index, character) in text.char_indices() {
        if character.is_whitespace() {
            if in_word {
                let distance = words_seen.abs_diff(VISUAL_CHUNK_TARGET_WORDS);
                if best.is_none_or(|(_, best_distance)| distance < best_distance) {
                    best = Some((byte_index, distance));
                }
            }
            in_word = false;
        } else if !in_word {
            words_seen += 1;
            in_word = true;
        }
    }
    best.map(|(break_at, _)| break_at)
}

fn deterministic_polish_inner(
    text: &str,
    capitalize_start: bool,
    brand: Option<&BrandHomophones<'_>>,
) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    let mut out = collapse_horizontal_space(text.trim());
    // NOTE: boundary wake-word stripping ("zephyr" at the start/end) is deliberately
    // OFF while the acoustic wake word is unwired — it deleted the real word "zephyr"
    // from dictation. Real control tails ("… zephyr stop/send/cancel") are still
    // handled by parse_control_command before polish. Re-add when wake ships.
    out = apply_inline_edits(&out);
    out = collapse_repetitions(&out);
    out = apply_inline_formatting(&out);
    out = fix_punctuation_spacing(&out);
    out = normalize_decimals(&out);
    out = normalize_large_numbers(&out);
    out = normalize_time_money_percent(&out);
    out = normalize_dates(&out);
    out = normalize_units(&out);
    out = normalize_numbers(&out);
    out = normalize_small_numbers(&out);
    out = casing_rules(&out, capitalize_start);
    out = apply_lexicon(&out);
    if let Some(brand) = brand {
        out = apply_brand_homophones(&out, brand);
    }
    out = tighten_domain_spacing(&out);
    out.trim().to_string()
}
