// Pure post-ASR text pipeline: voice-control suffixes, inline formatting, and
// deterministic cleanup that must run before paste.

use regex::Regex;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Compile-once cache for finite built-in patterns. User-controlled vocabulary
/// and replacement patterns use replace-whole-snapshot caches below.
static REGEX_CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
pub(crate) static VOCABULARY_REGEX_CACHE: OnceLock<Mutex<RegexSnapshotCache>> = OnceLock::new();
pub(crate) static REPLACEMENT_REGEX_CACHE: OnceLock<Mutex<RegexSnapshotCache>> = OnceLock::new();

#[derive(Default)]
pub(crate) struct RegexSnapshotCache {
    pub(crate) keys: Vec<String>,
    pub(crate) regexes: Vec<Regex>,
    #[cfg(test)]
    pub(crate) builds: usize,
}

pub(crate) fn regexes_for_keys(
    cache: &OnceLock<Mutex<RegexSnapshotCache>>,
    keys: &[String],
) -> Vec<Regex> {
    let cache = cache.get_or_init(|| Mutex::new(RegexSnapshotCache::default()));
    // Cache updates leave either the previous snapshot or a valid partially
    // updated snapshot, so a panic during allocation must not turn the next
    // text request into a process abort.
    let mut snapshot = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if snapshot.keys != keys {
        snapshot.regexes = keys
            .iter()
            .map(|key| {
                Regex::new(&format!(r"(?i)\b{}\b", regex::escape(key))).expect("escaped term regex")
            })
            .collect();
        snapshot.keys = keys.to_vec();
        #[cfg(test)]
        {
            snapshot.builds += 1;
        }
    }
    snapshot.regexes.clone()
}

pub(crate) fn re(pattern: &str) -> Regex {
    let cache = REGEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // HashMap remains structurally valid if an insertion panics; recover the
    // guard so subsequent requests can still use already-compiled patterns.
    let mut map = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(r) = map.get(pattern) {
        return r.clone();
    }
    let compiled = Regex::new(pattern).expect("valid pipeline regex");
    map.insert(pattern.to_string(), compiled.clone());
    compiled
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlIntent {
    Stop,
    Send,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlCommand {
    pub clean_text: String,
    pub wake_word: String,
    pub verb: String,
    pub intent: ControlIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTransformIntent {
    Prompt,
    Summarize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTransformCommand {
    pub clean_text: String,
    pub intent: AiTransformIntent,
}

/// Word-level timing carried from final ASR into transform routing. This is a
/// core type deliberately independent of any ASR backend crate: every backend
/// can either provide timing or leave the timed route unused.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedToken {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

/// Parse a trailing transform only when final ASR timing proves a meaningful
/// pause before its final substantive word. Untimed text remains on the
/// conservative lexical path, while timed routing rejects literals such as
/// "the word prompt" and quoted/code-like uses.
pub fn parse_timed_ai_transform_command(
    text: &str,
    tokens: &[TimedToken],
) -> Option<AiTransformCommand> {
    let command = parse_ai_transform_command(text)?;
    let words = reconstruct_timed_words(tokens);
    let (trigger_index, trigger) = words.iter().enumerate().rev().find(|(_, word)| {
        matches!(
            word.normalized.as_str(),
            "prompt" | "summarize" | "summarise" | "summariz" | "summaris"
        )
    })?;
    if !trigger.start.is_finite() || !trigger.end.is_finite() || trigger.end < trigger.start {
        return None;
    }
    let previous_end = words[..trigger_index]
        .iter()
        .rev()
        .find(|word| !word.normalized.is_empty())
        .map(|word| word.end)?;
    if !previous_end.is_finite() {
        return None;
    }
    // Target 250 ms, with 20 ms tolerance for backend timestamp quantization.
    // This still distinguishes a spoken phrase boundary from a normal gap.
    if trigger.start - previous_end + 0.02 < 0.25 {
        return None;
    }
    if transform_tail_is_literal(text, &trigger.normalized) {
        return None;
    }
    Some(command)
}

#[derive(Debug)]
struct TimedWord {
    normalized: String,
    start: f32,
    end: f32,
}

/// Parakeet emits SentencePiece-like pieces, not guaranteed whole words. A
/// leading whitespace marks a new word; pieces without it belong to prior
/// word. Reconstruct before routing so `summar` + `ize` remains one trigger.
fn reconstruct_timed_words(tokens: &[TimedToken]) -> Vec<TimedWord> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut start = 0.0;
    let mut end = 0.0;
    for token in tokens {
        // Punctuation-only pieces can span silence up to the next word in
        // Parakeet timing. They do not extend the preceding spoken word.
        if token.text.is_empty() || !token.text.chars().any(char::is_alphanumeric) {
            continue;
        }
        let starts_new = token.text.chars().next().is_some_and(char::is_whitespace);
        if starts_new && !current.is_empty() {
            words.push(TimedWord {
                normalized: current
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_ascii_lowercase(),
                start,
                end,
            });
            current.clear();
        }
        if current.is_empty() {
            start = token.start;
        }
        current.push_str(token.text.trim_start());
        end = end.max(token.end);
    }
    if !current.is_empty() {
        words.push(TimedWord {
            normalized: current
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_ascii_lowercase(),
            start,
            end,
        });
    }
    words
}

fn transform_tail_is_literal(text: &str, trigger: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let trigger = trigger.to_ascii_lowercase();
    let Some(prefix) = lower.strip_suffix(&trigger) else {
        return false;
    };
    let prefix = prefix.trim_end_matches(|c: char| c.is_whitespace() || ",.;:!?-".contains(c));
    ["the word", "word", "called", "label", "literal"]
        .iter()
        .any(|marker| prefix.ends_with(marker))
}

fn control_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?ix)
            (?:^|[\s,.;:!?-]+)
            (?P<wake>
                zephyr|zephir|zefer|zeffer|zepher|zephar|zephyrs|zeppe|zepper|zeppa|
                zaffer|zapper|zipper|zifr|zeppr|зэфир|зафир|зэфер|зэфэр|завер
            )
            [\s,.;:!?-]+
            (?P<verb>
                stopped|stop|stahp|stab|stuff|stap|step|stock|stuck|
                cancelled|canceled|cancel|cansel|cancle|
                sent|send|sen|sand|said|says|say|sea|entered|enter
            )
            [\s,.;:!?-]*$",
        )
        .expect("valid control regex")
    })
}

fn legacy_stop_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?ix)(?:^|[\s,.;:!?-]+)stop[\s,.;:!?-]+(?P<verb>stop|send|enter)[\s,.;:!?-]*$")
            .expect("valid legacy stop regex")
    })
}

fn legacy_compact_stop_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?ix)(?:^|[\s,.;:!?-]+)stop(?P<verb>send|enter)[\s,.;:!?-]*$")
            .expect("valid compact legacy stop regex")
    })
}

pub fn parse_control_command(text: &str) -> Option<ControlCommand> {
    let caps = control_re()
        .captures(text)
        .or_else(|| legacy_stop_re().captures(text))
        .or_else(|| legacy_compact_stop_re().captures(text));
    let Some(caps) = caps else {
        return parse_fuzzy_zephyr_tail_command(text)
            .or_else(|| parse_compact_fuzzy_zephyr_tail_command(text));
    };
    let m = caps.get(0)?;
    let verb = caps.name("verb")?.as_str().to_ascii_lowercase();
    let wake_word = caps
        .name("wake")
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "stop".to_string());
    let intent = normalized_control_intent(&verb).unwrap_or(ControlIntent::Stop);
    let clean_text = text[..m.start()]
        .trim_matches(|c: char| c.is_whitespace() || ",.;:!?-".contains(c))
        .to_string();
    Some(ControlCommand {
        clean_text,
        wake_word,
        verb,
        intent,
    })
}

pub fn parse_ai_transform_command(text: &str) -> Option<AiTransformCommand> {
    let stripped = text
        .trim()
        .trim_end_matches(|c: char| c.is_whitespace() || ",.;:!?".contains(c));
    if stripped.is_empty() {
        return None;
    }
    const TAILS: &[(&str, AiTransformIntent)] = &[
        ("please summarize this", AiTransformIntent::Summarize),
        ("please summarise this", AiTransformIntent::Summarize),
        ("please summarize", AiTransformIntent::Summarize),
        ("please summarise", AiTransformIntent::Summarize),
        ("summarize this", AiTransformIntent::Summarize),
        ("summarise this", AiTransformIntent::Summarize),
        ("summarize", AiTransformIntent::Summarize),
        ("summarise", AiTransformIntent::Summarize),
        // Final ASR can lose the last phoneme when Stop lands immediately
        // after the routing word. These are impossible English words and are
        // accepted only as complete tail tokens, so ordinary prose stays safe.
        ("summariz", AiTransformIntent::Summarize),
        ("summaris", AiTransformIntent::Summarize),
        ("prompt", AiTransformIntent::Prompt),
    ];
    let lower = stripped.to_ascii_lowercase();
    for (tail, intent) in TAILS {
        if lower == *tail {
            return None;
        }
        let Some(body_len) = lower.strip_suffix(tail).map(|body| body.len()) else {
            continue;
        };
        let before = lower[..body_len].chars().next_back()?;
        if before.is_alphanumeric() {
            continue;
        }
        let clean_text = stripped[..body_len]
            .trim_matches(|c: char| c.is_whitespace() || ",.;:!?-".contains(c))
            .to_string();
        if clean_text.is_empty() {
            return None;
        }
        return Some(AiTransformCommand {
            clean_text,
            intent: *intent,
        });
    }
    None
}

/// Resolve a standalone transform command against an explicit text selection.
///
/// This is deliberately separate from `parse_ai_transform_command`: a bare
/// "prompt" or "summarize" must remain ordinary dictation when no text is
/// selected. The caller owns selection capture and replacement delivery.
pub fn parse_selected_text_ai_transform_command(
    command_text: &str,
    selected_text: Option<&str>,
) -> Option<AiTransformCommand> {
    let selected_text = selected_text?.trim();
    if selected_text.is_empty() {
        return None;
    }
    let command = command_text
        .trim()
        .trim_end_matches(|c: char| c.is_whitespace() || ",.;:!?".contains(c))
        .to_ascii_lowercase();
    // Selection lane is summarize-ONLY: nobody selects
    // text and says "prompt" — that intent stays a dictation-tail command.
    let intent = match command.as_str() {
        "summarize" | "summarise" | "summariz" | "summaris" => AiTransformIntent::Summarize,
        _ => return None,
    };
    Some(AiTransformCommand {
        clean_text: selected_text.to_string(),
        intent,
    })
}

/// True when the dictated utterance is exactly the bare selection-transform
/// trigger ("summarize"/"summarise", trailing punctuation tolerated). Used by
/// the engine's copy-fallback: read-only selections (chat transcripts, web
/// pages) are invisible to the focused-control UIA read, so when the utterance
/// is the bare trigger but no selection was captured, the engine fetches the
/// selection via a clipboard-preserving Ctrl+C. Reuses the parser with a dummy
/// selection so the trigger normalization can never drift from it.
pub fn is_bare_summarize_trigger(command_text: &str) -> bool {
    parse_selected_text_ai_transform_command(command_text, Some("x")).is_some()
}

#[derive(Debug)]
struct TailToken {
    text: String,
    start: usize,
}

fn tail_word_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)[a-z]+").expect("valid tail word regex"))
}

fn tail_tokens(text: &str) -> Vec<TailToken> {
    let tail_start = text
        .char_indices()
        .rev()
        .nth(80)
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    tail_word_re()
        .find_iter(&text[tail_start..])
        .map(|m| TailToken {
            text: m.as_str().to_ascii_lowercase(),
            start: tail_start + m.start(),
        })
        .collect()
}

fn normalized_control_intent(verb: &str) -> Option<ControlIntent> {
    match verb {
        // Include common ASR near-homophones — Parakeet flips "send"↔"sent" at a
        // phrase end constantly, and drops the final consonants ("send"→"sea"),
        // and similarly mangles the past tenses. The "zephyr <verb>$" anchor
        // keeps false positives in normal dictation negligible.
        "send" | "sent" | "sen" | "sand" | "said" | "says" | "say" | "sea" | "enter"
        | "entered" => Some(ControlIntent::Send),
        "cancel" | "canceled" | "cancelled" | "cansel" | "cancle" => Some(ControlIntent::Cancel),
        // "stuff": Parakeet flips "stop"→"stuff" mid-utterance.
        "stop" | "stopped" | "stahp" | "stab" | "stuff" | "stap" | "step" | "stock" | "stuck" => {
            Some(ControlIntent::Stop)
        }
        _ => None,
    }
}

fn is_fuzzy_zephyr_word(word: &str) -> bool {
    if matches!(
        word,
        "zefyr"
            | "zefir"
            | "zeffir"
            | "zeffyr"
            | "zephir"
            | "zefer"
            | "zefe"
            | "zeffer"
            | "zepher"
            | "zephar"
            | "zephier"
            | "zephyer"
            | "zephyr"
            | "zephyrs"
            | "zeppe"
            | "zepper"
            | "zeppr"
            | "zeppa"
            | "zeppi"
            | "zeppy"
            | "zaffer"
            | "zapper"
            | "zipper"
            | "zifr"
    ) {
        return true;
    }
    word.starts_with('z') && (4..=8).contains(&word.len()) && levenshtein_ascii(word, "zephyr") <= 2
}

/// True when one transcribed word is recognisably the wake word, including the
/// compact forms the recognizer produces when the verb is glued on
/// ("Zephrasen", "zephyrstab", "Zephyr's"). Used ONLY to locate where a fired
/// command starts more precisely; it never decides whether a command is removed.
pub fn looks_like_wake_word(word: &str) -> bool {
    let word: String = word
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    // Misheard forms from real recordings: "Zephrasen", "Zapersand", "Zaperson",
    // "Zacher", "Cephers", "Jeffrey", "Jefferson". Any z-word of five letters or
    // more qualifies, plus the c/s/j stems. Over-matching is harmless here: this
    // only moves a boundary that the spotter already placed LATER, onto this word.
    (word.starts_with('z') && word.len() >= 5)
        || ["ceph", "seph", "jeff"]
            .iter()
            .any(|stem| word.starts_with(stem))
        || is_fuzzy_zephyr_word(&word)
        || is_fuzzy_zephyr_word(word.trim_end_matches('s'))
}

fn parse_compact_fuzzy_zephyr_tail_command(text: &str) -> Option<ControlCommand> {
    const VERBS: &[&str] = &[
        "cancelled",
        "canceled",
        "stopped",
        "cancel",
        "cansel",
        "cancle",
        "entered",
        "stuff",
        "stock",
        "stuck",
        "stahp",
        "stab",
        "stap",
        "step",
        "stop",
        "sent",
        "send",
        "sand",
        "said",
        "says",
        "enter",
        "sen",
        "say",
        "sea",
    ];

    let tokens = tail_tokens(text);
    let compact = tokens.last()?;
    for verb in VERBS {
        let Some(wake) = compact.text.strip_suffix(verb) else {
            continue;
        };
        if !is_fuzzy_zephyr_word(wake) {
            continue;
        }
        let intent = normalized_control_intent(verb)?;
        let clean_text = text[..compact.start]
            .trim_matches(|c: char| c.is_whitespace() || ",.;:!?-".contains(c))
            .to_string();
        return Some(ControlCommand {
            clean_text,
            wake_word: wake.to_string(),
            verb: (*verb).to_string(),
            intent,
        });
    }
    None
}

/// True when the current ASR tail ends in a bare Zephyr variant and is therefore
/// one word away from becoming a recording control. The streaming worker uses
/// this only as a scheduling hint: it does not stop capture or strip text until
/// `parse_control_command` also recognizes the verb.
pub fn has_trailing_control_wake(text: &str) -> bool {
    tail_tokens(text)
        .last()
        .is_some_and(|token| is_fuzzy_zephyr_word(&token.text))
}

fn levenshtein_ascii(a: &str, b: &str) -> usize {
    let mut costs: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.bytes().enumerate() {
        let mut prev = i;
        costs[0] = i + 1;
        for (j, cb) in b.bytes().enumerate() {
            let old = costs[j + 1];
            costs[j + 1] = if ca == cb {
                prev
            } else {
                1 + prev.min(costs[j]).min(costs[j + 1])
            };
            prev = old;
        }
    }
    costs[b.len()]
}

fn fuzzy_control_intent_after_wake(verb: &str) -> Option<ControlIntent> {
    if let Some(intent) = normalized_control_intent(verb) {
        return Some(intent);
    }

    // Observed/credible Parakeet phrase-end confusions. These deliberately live
    // behind a confirmed Zephyr token; treating these words as controls globally
    // would corrupt ordinary dictation.
    match verb {
        "slop" | "soft" => return Some(ControlIntent::Stop),
        "and" | "zen" => return Some(ControlIntent::Send),
        _ => {}
    }

    // Accept a single edit for short actions and two for the longer "cancel".
    // Require a unique nearest intent so an ambiguous tail never fires.
    let candidates = [
        ("stop", ControlIntent::Stop, 1usize),
        ("send", ControlIntent::Send, 1usize),
        ("cancel", ControlIntent::Cancel, 2usize),
    ];
    let mut best: Option<(usize, ControlIntent)> = None;
    let mut tied = false;
    for (canonical, intent, limit) in candidates {
        let distance = levenshtein_ascii(verb, canonical);
        if distance > limit {
            continue;
        }
        match best {
            None => {
                best = Some((distance, intent));
                tied = false;
            }
            Some((best_distance, _)) if distance < best_distance => {
                best = Some((distance, intent));
                tied = false;
            }
            Some((best_distance, _)) if distance == best_distance => tied = true,
            _ => {}
        }
    }
    (!tied).then_some(best?.1)
}

fn parse_fuzzy_zephyr_tail_command(text: &str) -> Option<ControlCommand> {
    let tokens = tail_tokens(text);
    let verb = tokens.last()?;
    let wake = tokens.get(tokens.len().checked_sub(2)?)?;
    if !is_fuzzy_zephyr_word(&wake.text) {
        return None;
    }
    let intent = fuzzy_control_intent_after_wake(&verb.text)?;
    let clean_text = text[..wake.start]
        .trim_matches(|c: char| c.is_whitespace() || ",.;:!?-".contains(c))
        .to_string();
    Some(ControlCommand {
        clean_text,
        wake_word: wake.text.clone(),
        verb: verb.text.clone(),
        intent,
    })
}

fn trim_command_punct(text: &str) -> String {
    text.trim_matches(|c: char| c.is_whitespace() || ",.;:!?-".contains(c))
        .to_string()
}

/// Strip the trailing "`<wake> <verb>`" control tail when a control was ALREADY
/// recognized upstream — i.e. the streaming probe fired on it and passed us the
/// `intent`. Only identifiable command residue may be removed. The streaming
/// fire proves the audio contained a command, but it does not prove which final
/// transcript tokens represent it: the full-buffer decode can omit or rewrite
/// the trigger. Preserving an unrecognized suffix is therefore mandatory to
/// avoid deleting dictated content.
pub fn strip_fired_control_tail(text: &str, intent: ControlIntent) -> String {
    let mut current = trim_command_punct(text);
    loop {
        if let Some(cmd) = parse_control_command(&current) {
            if cmd.intent == intent && cmd.clean_text.len() < current.len() {
                current = trim_command_punct(&cmd.clean_text);
                continue;
            }
        }
        let tokens = tail_tokens(&current);
        let Some(last) = tokens.last() else {
            break;
        };
        if is_fuzzy_zephyr_word(&last.text) {
            current = trim_command_punct(&current[..last.start]);
            continue;
        }
        if is_exact_onset_wake_residue(&last.text) {
            current = trim_joiner_punct(&current[..last.start]);
            continue;
        }
        if let Some(start) = truncated_verb_tail_start(&tokens) {
            current = trim_joiner_punct(&current[..start]);
            continue;
        }
        break;
    }
    current
}

/// Residue left when the audio was cut EXACTLY at the KWS onset because no
/// pause preceded the command. The spotter fires slightly late, so the decode
/// keeps the start of "zephyr" as a fragment, or hears it as "is that".
/// Measured on 103 no-pause owner recordings; every entry below
/// was observed there. Call ONLY for that cut: a seam cut has a real pause and
/// its dictation may legitimately end in "is that". The confirmed-command strip
/// still runs afterwards, which removes an earlier command the spotter missed.
pub fn strip_kws_onset_cut_residue(text: &str) -> String {
    let mut current = trim_joiner_punct(text);
    let mut dropped_fragment = false;
    while let Some(last) = tail_tokens(&current)
        .last()
        .map(|t| (t.text.clone(), t.start))
    {
        if !is_kws_onset_fragment(&last.0) || !starts_standalone_word(&current, last.1) {
            break;
        }
        current = trim_joiner_punct(&current[..last.1]);
        dropped_fragment = true;
    }
    if !dropped_fragment {
        let tokens = tail_tokens(&current);
        if let [.., first, second] = tokens.as_slice() {
            if first.text == "is"
                && starts_standalone_word(&current, first.start)
                && starts_standalone_word(&current, second.start)
                && matches!(second.text.as_str(), "that" | "ever" | "there" | "it")
            {
                current = trim_joiner_punct(&current[..first.start]);
            }
        }
    }
    current
}

/// `it's` tokenizes as `it` + `s`; only a word preceded by whitespace (or the
/// start of the text) is a separate spoken word.
fn starts_standalone_word(text: &str, start: usize) -> bool {
    text[..start]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
}

fn is_kws_onset_fragment(word: &str) -> bool {
    is_exact_onset_wake_residue(word)
        || matches!(
            word,
            "zen" | "zap" | "zab" | "zam" | "zach" | "sett" | "x" | "s"
        )
}

/// Short ASR residue left when native KWS cuts inside the acoustic Zephyr
/// span. This runs only after KWS has already fired a terminal control, so it
/// never alters ordinary dictation or decides control intent.
fn is_exact_onset_wake_residue(word: &str) -> bool {
    matches!(
        word,
        "z" | "ze" | "zeb" | "zep" | "zeph" | "zephy" | "zephyr" | "zero"
    )
}

/// Trim only what joined the command onto the sentence — whitespace and a
/// dangling comma/dash — while KEEPING terminal sentence punctuation.
///
/// `trim_command_punct` also eats `.!?`, which is right when the punctuation is
/// command noise but wrong here: in "What is broken? Zephyr s" the question mark
/// is the user's own sentence, and cutting the command must not silently take it.
fn trim_joiner_punct(text: &str) -> String {
    text.trim_end_matches(|c: char| c.is_whitespace() || ",;:-".contains(c))
        .to_string()
}

/// Longest trailing fragment treated as a chopped command verb, in tokens and
/// in characters. "stop"/"send"/"cancel" clipped mid-word give at most a couple
/// of very short tokens; real dictation after the trigger word does not.
const TRUNCATED_VERB_MAX_TOKENS: usize = 2;
const TRUNCATED_VERB_MAX_CHARS: usize = 6;

/// Byte offset at which to cut when the tail is an identifiable TRIGGER word
/// followed by a chopped verb — the "Zephyr s" shape.
///
/// (Terminology: `zephyr` is the trigger word. The `wake`/`wake_word` naming in
/// the surrounding legacy identifiers predates that distinction — nothing here
/// wakes an idle app, because the acoustic KWS path is not wired in this build.)
///
/// Exists because the probe lane (context bias 5.0) and the full-buffer decode
/// (bias 1.0) can render the same "zephyr stop" audio differently — e.g. the
/// full-buffer pass as "Zephyr s". `parse_control_command` cannot parse "s" as
/// a verb, and the bare-trigger branch above only fires when the trigger is
/// the LAST token, so without this fn "Zephyr s" would be delivered into the
/// user's text.
///
/// This is safe precisely because it is narrow. The caller only reaches
/// `strip_fired_control_tail` after a control has ALREADY fired, so the audio is
/// known to end in a command; the trigger token is independently identifiable;
/// and the residue after it is capped at a couple of very short tokens, so
/// ordinary speech that merely mentions the trigger word ("the zephyr
/// constellation is bright") keeps every word — its trailing content is far too
/// long to qualify.
fn truncated_verb_tail_start(tokens: &[TailToken]) -> Option<usize> {
    let (trigger_index, trigger_tok) = tokens
        .iter()
        .enumerate()
        .rev()
        .find(|(_, tok)| is_fuzzy_zephyr_word(&tok.text))?;
    let residue = &tokens[trigger_index + 1..];
    if residue.is_empty() || residue.len() > TRUNCATED_VERB_MAX_TOKENS {
        return None;
    }
    let residue_chars: usize = residue.iter().map(|tok| tok.text.chars().count()).sum();
    if residue_chars > TRUNCATED_VERB_MAX_CHARS {
        return None;
    }
    Some(trigger_tok.start)
}

/// Strip a single trailing BARE wake word ("… zephyr" with no command verb yet).
/// The streaming committer commits this tail BEFORE the verb lands, so without this
/// it would be sent to the L3 LLM (prewarm) and leak the wake word into the polished
/// prefix. Returns the trimmed text when a trailing wake token was removed, else None.
/// Safe: the wake word (and its near-homophones) is never normal dictation.
fn strip_trailing_bare_wake(text: &str) -> Option<String> {
    let tokens = tail_tokens(text);
    let last = tokens.last()?;
    if !is_fuzzy_zephyr_word(&last.text) {
        return None;
    }
    Some(trim_command_punct(&text[..last.start]))
}

/// Strip a STANDALONE bare AI-transform trigger ("prompt" / "summarize" on its own).
/// `parse_ai_transform_command` returns None for a bare trigger (it needs content to
/// transform), but for the L0 scrub a lone trigger word is a command marker, not
/// dictation — so drop it. Content ending in a trigger ("write me a prompt") is
/// already handled by `parse_ai_transform_command`, which keeps the content.
fn strip_trailing_bare_transform(text: &str) -> Option<String> {
    match trim_command_punct(text).to_ascii_lowercase().as_str() {
        "prompt" | "summarize" | "summarise" => Some(String::new()),
        _ => None,
    }
}

/// Canonical **L0** scrub: remove every wake / control / AI-transform token from a
/// transcript (or partial) so they can NEVER reach the L3 LLM or the delivered text.
/// Idempotent; iterates because tokens stack at the tail ("… prompt, zephyr stop").
/// Used by BOTH the streaming-polish prewarm (pre-LLM) and finalize, so the two paths
/// can't drift. Removes: a complete "zephyr `<verb>`" control tail, a complete
/// "`<text>` prompt|summarize" transform tail, and a trailing bare wake word. It does
/// NOT strip bare control verbs (e.g. a legitimately dictated "send"/"stop") — those
/// only count as commands when preceded by the wake word.
pub fn scrub_command_tokens(text: &str) -> String {
    let mut current = trim_command_punct(text);
    loop {
        if let Some(cmd) = parse_control_command(&current) {
            if cmd.clean_text.len() < current.len() {
                current = cmd.clean_text;
                continue;
            }
        }
        if let Some(cmd) = parse_ai_transform_command(&current) {
            if cmd.clean_text.len() < current.len() {
                current = cmd.clean_text;
                continue;
            }
        }
        if let Some(stripped) = strip_trailing_bare_wake(&current) {
            if stripped.len() < current.len() {
                current = stripped;
                continue;
            }
        }
        if let Some(stripped) = strip_trailing_bare_transform(&current) {
            if stripped.len() < current.len() {
                current = stripped;
                continue;
            }
        }
        break;
    }
    current
}
