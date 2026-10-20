//! Hard exposure rules, applied before any allowlist or admission hook and not
//! overridable by configuration.
//!
//! Two layers:
//!
//! * **Universal** ([`forbidden`]): the system pasteboard is never agent-facing, for every app
//!   and every caller. Pasting or reading the clipboard crosses into other apps' data.
//! * **Dictation** ([`forbidden_with`] with `dictation = true`): HeardRight's rules
//!   (DESKTOP_PASTE_AND_MEDIA_SPEC; `rightkit-automation` LOCKED_RULES). Dictation control, text
//!   delivery into other apps and transcript history are never agent-facing *for an app that
//!   declares itself a dictation app* ([`crate::AppProfile::dictation`]). Other apps keep their
//!   own `history` or `delivery` commands; those words mean something else there.
//!
//! Names are matched by word, not by raw substring: `snake_case`, `kebab-case`, dotted, colon
//! and camelCase / PascalCase names all split into the same lowercase tokens, so
//! `pasteText`, `StartDictation` and `recentTranscripts` are refused exactly like
//! `paste_text`, `start_dictation` and `recent_transcripts`.
//!
//! `sensitive` apps additionally lose every generic read channel (`eval`, `dom`, `ax`,
//! `screenshot`), because those could read the same data back out of the webview.

/// Name tokens that are never exposed, for any app.
pub const FORBIDDEN_TOKENS: &[&str] = &["paste", "pasting", "pasteboard", "clipboard"];

/// Name tokens that are never exposed for a dictation app.
pub const DICTATION_TOKENS: &[&str] = &[
    "dictation",
    "dictate",
    "dictating",
    "history",
    "transcripts",
    "deliver",
    "delivery",
    "detokenize",
    "detokenization",
    "ptt",
];

/// Multi-word names that are never exposed for a dictation app (words split on any
/// non-alphanumeric).
pub const DICTATION_PHRASES: &[&[&str]] = &[
    &["recent", "transcript"],
    &["last", "transcript"],
    &["start", "capture"],
    &["stop", "capture"],
    &["push", "to", "talk"],
];

/// Compact (separator-free, lowercase) substrings that are refused wherever they appear, so a
/// name with no separator or case boundary (`getclipboardtext`) cannot slip past the token
/// split. Only words with no ordinary use inside another word.
const FORBIDDEN_COMPACT: &[&str] = &["pasteboard", "clipboard"];

/// Split `name` into lowercase word tokens: on every non-ASCII-alphanumeric character, and
/// inside a run on camelCase / PascalCase boundaries (`pasteText`, `StartDictation`), on
/// acronym ends (`PTTStart` -> `ptt`, `start`) and on letter/digit changes (`paste2text`).
/// `snake_case`, `kebab-case`, `dotted.names` and `colon:subjects` split as before.
fn tokens(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for run in name.split(|c: char| !c.is_ascii_alphanumeric()) {
        let chars: Vec<char> = run.chars().collect();
        let mut cur = String::new();
        for (i, &c) in chars.iter().enumerate() {
            if i > 0 {
                let prev = chars[i - 1];
                let next = chars.get(i + 1).copied();
                let boundary = (prev.is_ascii_lowercase() && c.is_ascii_uppercase())
                    || (prev.is_ascii_digit() != c.is_ascii_digit())
                    || (prev.is_ascii_uppercase()
                        && c.is_ascii_uppercase()
                        && next.is_some_and(|n| n.is_ascii_lowercase()));
                if boundary && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            cur.push(c.to_ascii_lowercase());
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

fn compact(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `Some(reason)` when `name` (a tool name, or an app command name) is never exposed for any
/// app (the universal layer only).
pub fn forbidden(name: &str) -> Option<String> {
    forbidden_with(name, false)
}

/// `Some(reason)` when `name` is never exposed: the universal layer, plus the dictation layer
/// when `dictation` is set.
pub fn forbidden_with(name: &str, dictation: bool) -> Option<String> {
    let toks = tokens(name);
    let compact = compact(name);
    for w in FORBIDDEN_COMPACT {
        if compact.contains(w) {
            return Some(format!(
                "`{w}` is never exposed to agents (no paste or pasteboard)"
            ));
        }
    }
    for t in &toks {
        if FORBIDDEN_TOKENS.contains(&t.as_str()) {
            return Some(format!(
                "`{t}` is never exposed to agents (no paste or pasteboard)"
            ));
        }
        if dictation && DICTATION_TOKENS.contains(&t.as_str()) {
            return Some(format!(
                "`{t}` is never exposed to agents for a dictation app (no dictation, delivery or history)"
            ));
        }
    }
    if dictation {
        for p in DICTATION_PHRASES {
            let hit = toks.windows(p.len()).any(|w| {
                w.iter()
                    .zip(p.iter())
                    .all(|(a, b)| a.trim_end_matches('s') == *b)
            });
            if hit {
                return Some(format!(
                    "`{}` is never exposed to agents (dictation rule)",
                    p.join("_")
                ));
            }
        }
    }
    None
}

/// Generic control methods that can read arbitrary app content back out.
/// `wait` evaluates page script and `batch` carries any of them, so both count.
pub const GENERIC_READ_METHODS: &[&str] = &["eval", "dom", "ax", "screenshot", "wait", "batch"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasteboard_is_refused_for_every_app() {
        for name in [
            "clipboard.read",
            "paste_text",
            "pasteboard",
            "genright.command:paste",
        ] {
            assert!(forbidden(name).is_some(), "{name}");
            assert!(forbidden_with(name, true).is_some(), "{name}");
        }
    }

    #[test]
    fn dictation_words_are_refused_only_for_a_dictation_app() {
        for name in [
            "history",
            "chat_history",
            "delivery_status",
            "start_dictation",
            "push_to_talk",
            "recent_transcripts",
        ] {
            assert!(
                forbidden(name).is_none(),
                "{name} must stay open for other apps"
            );
            assert!(
                forbidden_with(name, true).is_some(),
                "{name} must stay closed for dictation"
            );
        }
        assert!(forbidden_with("list_models", true).is_none());
    }

    #[test]
    fn camel_case_names_split_like_snake_case() {
        let t = |n: &str| tokens(n);
        assert_eq!(t("pasteText"), ["paste", "text"]);
        assert_eq!(t("StartDictation"), ["start", "dictation"]);
        assert_eq!(t("PTTStart"), ["ptt", "start"]);
        assert_eq!(t("recentTranscripts"), ["recent", "transcripts"]);
        assert_eq!(t("paste2text"), ["paste", "2", "text"]);
        assert_eq!(
            t("genright.command:startCapture"),
            ["genright", "command", "start", "capture"]
        );
        assert_eq!(t("snake_case-kebab"), ["snake", "case", "kebab"]);
        assert_eq!(t("HTTPServer"), ["http", "server"]);
        assert!(t("").is_empty() && t("__..").is_empty());
    }

    #[test]
    fn camel_case_pasteboard_names_are_refused_for_every_app() {
        for name in [
            "pasteText",
            "PasteText",
            "readPasteboard",
            "getClipboard",
            "clipboardRead",
            "PASTE",
            "genright.command:pasteText",
            "getclipboardtext",
            "READPASTEBOARD",
            "pasting2",
        ] {
            assert!(forbidden(name).is_some(), "{name}");
            assert!(forbidden_with(name, true).is_some(), "{name}");
        }
    }

    #[test]
    fn camel_case_dictation_names_are_refused_only_for_a_dictation_app() {
        for name in [
            "startDictation",
            "chatHistory",
            "deliveryStatus",
            "pushToTalk",
            "PushToTalk",
            "recentTranscripts",
            "lastTranscript",
            "startCapture",
            "stopCapture",
            "PTTStart",
            "heardright.command:startDictation",
        ] {
            assert!(
                forbidden(name).is_none(),
                "{name} must stay open for other apps"
            );
            assert!(forbidden_with(name, true).is_some(), "{name}");
        }
    }

    #[test]
    fn camel_case_hardening_does_not_refuse_ordinary_names() {
        for name in [
            "listModels",
            "modelStatus",
            "pasteurize",
            "startServer",
            "captureScreen",
            "pushNotification",
            "talkToModel",
            "lastModified",
        ] {
            // `pasteurize` is one token, not `paste`; the dictation layer keeps its phrases.
            assert!(forbidden_with(name, true).is_none(), "{name}");
        }
    }
}
