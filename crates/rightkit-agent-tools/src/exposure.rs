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

fn tokens(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
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
}
