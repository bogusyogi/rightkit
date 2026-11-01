use super::{control::re, number_words::collapse_repetitions_line};
use regex::{Captures, Regex};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

// SPOKEN-SYMBOL SAFETY (L0 precision-first). Bare ambiguous nouns were
// replaced unconditionally, silently corrupting ordinary prose: "five star
// review" -> "five * review", "a period of time" -> "a . of time", "the dot
// product", "colon cancer", "a dash of salt", and sentence-initial "Plus, we
// should ship it" -> "+, we should ship it". L0 doctrine: a MISSED formatting
// command is cheaper than a meaning-changing deterministic rewrite, so the
// bare forms (period, dot, colon, dash, plus, divided by) are
// gone and only unambiguous spoken forms remain ("full stop", "em dash",
// "asterisk", "forward slash", "plus sign", ...). Measured on 99 real
// dictations: zero occurrences of any removed bare form, so this costs no
// observed recall. "comma" stays -- it has no realistic lexical corruption
// case. Bare "slash" and "star" are equally ambiguous ("Slash is the
// guitarist", "five star review"), so only "forward slash" and "asterisk"
// retain deterministic symbol behavior.
// Dropping bare "colon" also un-breaks "semi colon": the bare rule ran
// FIRST, turning "semi colon" into "semi :" so the ';' rule was dead code.
pub(crate) fn inline_replacements() -> &'static [(Regex, &'static str)] {
    static REPLACEMENTS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    REPLACEMENTS
        .get_or_init(|| {
            vec![
                (Regex::new(r"(?i)\b(?:new[\s-]?paragraph|start a new paragraph)\b").unwrap(), "\n\n"),
                (
                    Regex::new(r"(?i)\b(?:new[\s-]?line|newline|next[\s-]?line|line[\s-]?break|skip a line)\b")
                        .unwrap(),
                    "\n",
                ),
                (Regex::new(r"(?i)\bfull[\s-]?stop\b").unwrap(), "."),
                (Regex::new(r"(?i)\bcomma\b").unwrap(), ","),
                (Regex::new(r"(?i)\bquestion[\s-]?mark\b").unwrap(), "?"),
                (
                    Regex::new(r"(?i)\bexclamation[\s-]?(?:mark|point)\b").unwrap(),
                    "!",
                ),
                (Regex::new(r"(?i)\bsemi[\s-]?colon\b").unwrap(), ";"),
                (Regex::new(r"(?i)\bem[\s-]?dash\b").unwrap(), "-"),
                (Regex::new(r"(?i)\bellipsis\b").unwrap(), "..."),
                (Regex::new(r"(?i)\b(?:quotation[\s-]?mark|double[\s-]?quote)\b").unwrap(), "\""),
                (Regex::new(r"(?i)\b(?:apostrophe|single[\s-]?quote)\b").unwrap(), "'"),
                (Regex::new(r"(?i)\basterisk\b").unwrap(), "*"),
                (Regex::new(r"(?i)\bampersand\b").unwrap(), "&"),
                (Regex::new(r"(?i)\b(?:percent[\s-]?sign|per[\s-]?cent|percentage[\s-]?symbol)\b").unwrap(), "%"),
                (Regex::new(r"(?i)\bforward[\s-]?slash\b").unwrap(), "/"),
                (Regex::new(r"(?i)\bback[\s-]?slash\b").unwrap(), "\\"),
                (Regex::new(r"(?i)\bunderscore\b").unwrap(), "_"),
                (Regex::new(r"(?i)\b(?:hashtag|number[\s-]?sign|pound[\s-]?sign)\b").unwrap(), "#"),
                (Regex::new(r"(?i)\btilde\b").unwrap(), "~"),
                (Regex::new(r"(?i)\bat[\s-]?(?:sign|symbol|the[\s-]?rate)\b").unwrap(), "@"),
                (Regex::new(r"(?i)\b(?:(?:open|opening|left)[\s-]?angle[\s-]?bracket|less[\s-]?than[\s-]?sign)\b").unwrap(), "<"),
                (Regex::new(r"(?i)\b(?:(?:close|closing|closed|right)[\s-]?angle[\s-]?bracket|greater[\s-]?than[\s-]?sign)\b").unwrap(), ">"),
                (Regex::new(r"(?i)\bplus[\s-]?sign\b").unwrap(), "+"),
                (Regex::new(r"(?i)\b(?:minus[\s-]?sign|negative[\s-]?sign)\b").unwrap(), "-"),
                (Regex::new(r"(?i)\bequals(?:[\s-]?sign)?\b").unwrap(), "="),
                (Regex::new(r"(?i)\b(?:trademark|tm)\b").unwrap(), "™"),
                (Regex::new(r"(?i)\bregistered[\s-]?trademark\b").unwrap(), "®"),
                (Regex::new(r"(?i)\bcopyright(?:[\s-]?symbol)?\b").unwrap(), "©"),
                (Regex::new(r"(?i)\bdegrees?[\s-]?c(?:elsius|entigrade)?\b").unwrap(), "°C"),
                (Regex::new(r"(?i)\bdegrees?[\s-]?f(?:ahrenheit)?\b").unwrap(), "°F"),
                (Regex::new(r"(?i)\bdegree[\s-]?(?:sign|symbol)\b").unwrap(), "°"),
                (
                    Regex::new(r"(?i)\b(?:open|opening)[\s-]?(?:paren|parenthesis|parentheses)\b")
                        .unwrap(),
                    "(",
                ),
                (
                    Regex::new(r"(?i)\b(?:close|closing|closed)[\s-]?(?:paren|parenthesis|parentheses)\b")
                        .unwrap(),
                    ")",
                ),
                (
                    Regex::new(r"(?i)\b(?:open|opening)[\s-]?(?:bracket|brackets|square[\s-]?bracket)\b")
                        .unwrap(),
                    "[",
                ),
                (
                    Regex::new(r"(?i)\b(?:close|closing|closed)[\s-]?(?:bracket|brackets|square[\s-]?bracket)\b")
                        .unwrap(),
                    "]",
                ),
                (
                    Regex::new(r"(?i)\b(?:open|opening)[\s-]?(?:brace|braces|curly[\s-]?brace)\b")
                        .unwrap(),
                    "{",
                ),
                (
                    Regex::new(r"(?i)\b(?:close|closing|closed)[\s-]?(?:brace|braces|curly[\s-]?brace)\b")
                        .unwrap(),
                    "}",
                ),
            ]
        })
        .as_slice()
}

// Punctuation/whitespace cleanups applied after the spoken-token replacements.
// Cached once to avoid recompiling the regexes on every call.
fn formatting_cleanups() -> &'static [(Regex, &'static str)] {
    static CLEANUPS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    CLEANUPS.get_or_init(|| {
        vec![
            (Regex::new(r"[ \t]*\n[ \t]*").unwrap(), "\n"),
            (Regex::new(r"[,;:]\n").unwrap(), "\n"),
            (Regex::new(r"\n[,;:.]\s*").unwrap(), "\n"),
            (Regex::new(r"\s+([?!.,:;%\)\]\}])").unwrap(), "$1"),
            (
                Regex::new(r"\.\s+(com|net|org|io|ai|app|dev|co|in|me|xyz|site|online)\b").unwrap(),
                ".$1",
            ),
            (Regex::new(r"#\s+([A-Za-z0-9_])").unwrap(), "#$1"),
            (Regex::new(r"(\d)\s+°([CF])").unwrap(), "$1°$2"),
            (Regex::new(r"<\s+([A-Za-z/])").unwrap(), "<$1"),
            (
                Regex::new(r"\s+([()\[\]{}<>#@&*_~=+/\\-])\s+").unwrap(),
                "$1",
            ),
            (Regex::new(r"\s+([<>#@&*_~=+/\\-])$").unwrap(), "$1"),
            (
                Regex::new(r",\s*([?!.,:;()\[\]{}])\s*,?\s*").unwrap(),
                "$1 ",
            ),
        ]
    })
}

pub fn apply_inline_formatting(text: &str) -> String {
    // Cow-threaded: `replace_all` returns Cow::Borrowed when a pattern doesn't
    // match (the common case for most of these ~50 narrow patterns on any given
    // utterance). Only reassign — and therefore only allocate — on an actual
    // match; skipping that saves a full string clone per no-op pass.
    let mut out: Cow<str> = Cow::Owned(normalize_spoken_addresses(text));
    for (pattern, replacement) in inline_replacements() {
        if let Cow::Owned(s) = pattern.replace_all(&out, *replacement) {
            out = Cow::Owned(s);
        }
    }
    for (pattern, replacement) in formatting_cleanups() {
        if let Cow::Owned(s) = pattern.replace_all(&out, *replacement) {
            out = Cow::Owned(s);
        }
    }
    collapse_horizontal_space(&out).trim().to_string()
}

fn normalize_spoken_addresses(text: &str) -> String {
    // Chain via Cow instead of forcing `.into_owned()` after each of the 3
    // passes — only the final `.into_owned()` allocates, and only if the text
    // actually changed somewhere in the chain.
    // TLD-GATED: the old pattern collapsed ANY "<word> dot <word>" into a fake
    // domain, so ordinary prose was corrupted -- "the dot product is wrong"
    // became "the.product is wrong". Require the FINAL segment to be a known
    // TLD (same list `tighten_domain_spacing` uses) so "gmail dot com" still
    // works but "dot product" / "dot matrix" / "dot notation" stay prose.
    let domains = re(concat!(
        r"(?i)\b([a-z0-9][a-z0-9-]*(?:\s+dot\s+[a-z0-9][a-z0-9-]*)*",
        r"\s+dot\s+(?:com|net|org|io|ai|app|dev|co|in|me|xyz|site|online|uk|cc))\b"
    ))
    .replace_all(text, |caps: &Captures| {
        caps[1]
            .split_whitespace()
            .filter(|part| !part.eq_ignore_ascii_case("dot"))
            .collect::<Vec<_>>()
            .join(".")
    });
    let emails =
        re(r"(?i)\b([a-z0-9._%+-]+)\s+at\s+([a-z0-9][a-z0-9-]*(?:\.[a-z0-9][a-z0-9-]*)+)\b")
            .replace_all(&domains, "$1@$2");
    re(r"(?i)(^|[\s\(\[])at\s+([a-z0-9][a-z0-9-]*(?:\.[a-z0-9][a-z0-9-]*)+)\b")
        .replace_all(&emails, "$1@$2")
        .into_owned()
}

pub fn tighten_domain_spacing(text: &str) -> String {
    re(r"(?i)\.\s+(com|net|org|io|ai|app|dev|co|in|me|xyz|site|online|uk|cc)\b")
        .replace_all(text, |caps: &Captures| {
            format!(".{}", caps[1].to_ascii_lowercase())
        })
        .into_owned()
}

fn inline_edit_command_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Just the command + trailing separators. Whether a match is a retraction
    // (vs transitive prose like "delete that file") is decided by the caller's
    // is_scratch / command_closed guard — the regex stays dumb on purpose.
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:delete|scratch)\s+(?:that|this)\b[\s.!?,]*")
            .expect("valid inline edit regex")
    })
}

const RETRACTION_LEADINS: [&str; 6] = ["actually", "no", "wait", "sorry", "um", "uh"];

pub fn apply_inline_edits(text: &str) -> String {
    let pattern = inline_edit_command_re();
    let mut out = text.to_string();
    // `search_from` advances past skipped (transitive-prose) matches so one
    // "delete that file" early in the text cannot mask a real retraction later
    // (external review blocker: `break` stopped the whole scan).
    let mut search_from = 0usize;
    while let Some(m) = pattern.find_at(&out, search_from) {
        let tail = &out[m.end()..];
        let matched = &out[m.start()..m.end()];
        // "scratch that/this" has no transitive reading in dictation — always a
        // retraction. "delete that/this" IS transitive prose when followed by an
        // uncapitalized continuation with no closing punctuation on the command
        // ("please delete that file…") — skip those and keep scanning.
        let is_scratch = matched.to_ascii_lowercase().contains("scratch");
        let command_closed = matched.contains(['.', '!', '?', ','])
            || tail.is_empty()
            || tail.starts_with(|c: char| c.is_uppercase());
        if !is_scratch && !command_closed {
            search_from = m.end();
            continue;
        }
        // Retract to the previous sentence boundary…
        let mut clause_start = out[..m.start()]
            .rfind(['.', '!', '?', '\n'])
            .map(|i| i + 1)
            .unwrap_or(0);
        // …and if what remains before the command is only a discourse lead-in
        // ("Actually," / "no wait"), extend the retraction one sentence further.
        let lead = out[clause_start..m.start()]
            .trim()
            .trim_matches(|c: char| c == ',' || c == '.')
            .to_ascii_lowercase();
        let lead_only = !lead.is_empty()
            && lead
                .split_whitespace()
                .all(|w| RETRACTION_LEADINS.contains(&w.trim_matches(',')));
        if lead.is_empty() || lead_only {
            clause_start = out[..clause_start.saturating_sub(1)]
                .rfind(['.', '!', '?', '\n'])
                .map(|i| i + 1)
                .unwrap_or(0);
        }
        out.replace_range(clause_start..m.end(), " ");
        out = collapse_horizontal_space(&out).trim().to_string();
        search_from = 0; // text shifted; restart the scan from the top
    }
    out
}

pub fn expand_snippets(text: &str, snippets: &HashMap<String, String>) -> String {
    if text.is_empty() || snippets.is_empty() {
        return text.to_string();
    }
    let lookup: HashMap<String, String> = snippets
        .iter()
        .map(|(k, v)| {
            (
                k.trim().trim_start_matches('/').to_ascii_lowercase(),
                v.clone(),
            )
        })
        .collect();
    let out = slash_snippet_re().replace_all(text, |caps: &Captures| {
        let leading = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let key = caps[2].to_ascii_lowercase();
        lookup
            .get(&key)
            .map(|value| format!("{leading}{value}"))
            .unwrap_or_else(|| caps[0].to_string())
    });
    spoken_snippet_re()
        .replace_all(&out, |caps: &Captures| {
            let leading = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let key = caps[2].to_ascii_lowercase();
            lookup
                .get(&key)
                .map(|value| format!("{leading}{value}"))
                .unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

fn slash_snippet_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(^|\s)/([a-z0-9_-]+)\b").unwrap())
}

fn spoken_snippet_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(^|[^A-Za-z0-9_])slash[\s-]([a-z0-9_-]+)\b").unwrap())
}

pub(crate) fn collapse_horizontal_space(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[ \t]+").unwrap())
        .replace_all(text, " ")
        .into_owned()
}

/// High-precision discourse-"like" removal. Only two patterns are unambiguous
/// filler in ASR output, both anchored on punctuation the model actually emits:
///   1. comma-bracketed:   "it was, like, huge"  -> "it was huge"
///   2. sentence-initial:  "Like, we should go"  -> "we should go"
/// Verb/preposition/quotative uses ("I like it", "something like that",
/// "I was like, no") never match: none of them carry a comma on BOTH sides /
/// start the sentence with a trailing comma. Un-punctuated fillers are the
/// LLM lane's job (L1) — deterministic stays precision-first.
pub fn aggressive_speech_cleanup(text: &str) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    // Fillers are NOT removed here. N0 owns hesitation removal, at the piece
    // level in the ASR detokenizer, so it
    // applies before L0 AND before L1 (which is fed raw ASR and therefore never
    // saw L0's edits). Doing it in both places was duplication with two
    // different word lists.
    // WORD DELETION IS NOT L0's JOB.
    //
    // Unconditionally stripping discourse fillers ("so"/"okay"/"well" leading,
    // "you know"/"i mean" anywhere, "like") makes the pipeline
    // self-contradictory: the same sentence comes back with different WORDS
    // depending on whether the cloud call succeeded, and a leading "Okay" is
    // frequently content, not clutter. Deleting speech is the AI layer's
    // decision, made with the whole utterance in view. L0's job is
    // deterministic normalisation — spacing, punctuation, repeats — not
    // vocabulary: one owner, measured, or nothing.
    let mut out = text.to_string();

    out = fix_punctuation_spacing(&collapse_repetitions(&out));
    collapse_horizontal_space(&out)
        .trim_matches(|c: char| c.is_whitespace() || c == ',')
        .to_string()
}

#[allow(dead_code)] // off until the acoustic wake word ships (see deterministic_polish)
fn strip_boundary_wake_words(text: &str) -> String {
    let wake = r"(?:zephyr|zephir|zefer|zeffer|zepher|zephar|zephyrs|zeppe|zepper|zeppa|zaffer|zapper|зэфир|зафир|зэфер|зэфэр|завер)";
    let start = re(&format!(r"(?i)^\s*{wake}[\s,.;:!?-]*"));
    let end = re(&format!(r"(?i)[\s,.;:!?-]*{wake}\s*$"));
    let out = start.replace(text, "").into_owned();
    end.replace(&out, "").trim().to_string()
}

pub(crate) fn fix_punctuation_spacing(text: &str) -> String {
    // Cow-chained: each pass is a Cow::Borrowed no-op unless it actually matched,
    // so a typical call only pays for the passes that changed something instead
    // of re-allocating on all 6 every time.
    let out = re(r"\s+([,.!?;:])").replace_all(text, "$1");
    let out = re(r"([,.!?;:])([A-Za-z])").replace_all(&out, "$1 $2");
    let out = re(r"([\(\[\{])\s+").replace_all(&out, "$1");
    let out = re(r"\s+([\)\]\}])").replace_all(&out, "$1");
    // Add the OUTER space brackets need: a space before an opening bracket glued to
    // a preceding char ("you?(coming)" -> "you? (coming)"), and after a closing
    // bracket glued to a following word/number ("(coming)next" -> "(coming) next").
    // The preceding/following char excludes brackets so nested "((" / "))" stay tight.
    let out = re(r"([^\s(\[{])([(\[{])").replace_all(&out, "$1 $2");
    let out = re(r"([)\]}])([A-Za-z0-9])").replace_all(&out, "$1 $2");
    out.into_owned()
}

pub(crate) fn collapse_repetitions(text: &str) -> String {
    // Preserve newlines (spoken "new line"/"new paragraph"): collapse repeats
    // WITHIN each line, then rejoin with \n. A bare split_whitespace()+join(" ")
    // flattens \n to spaces — which silently ate newlines on the second
    // deterministic_polish pass (the polish pipeline runs it before + after Harper).
    text.split('\n')
        .map(collapse_repetitions_line)
        .collect::<Vec<_>>()
        .join("\n")
}
