//! Hard exposure rules, applied before any allowlist or admission hook and not
//! overridable by configuration.
//!
//! HeardRight's rules (DESKTOP_PASTE_AND_MEDIA_SPEC; `rightkit-automation`
//! LOCKED_RULES): dictation control, text delivery into other apps, the system
//! pasteboard and transcript history are never agent-facing. A tool or app
//! command whose name carries any of those words is refused outright, for every
//! app and every caller. `sensitive` apps additionally lose every generic read
//! channel (`eval`, `dom`, `ax`, `screenshot`), because those could read the
//! same data back out of the webview.

/// Name tokens that are never exposed.
pub const FORBIDDEN_TOKENS: &[&str] = &[
    "dictation",
    "dictate",
    "dictating",
    "paste",
    "pasting",
    "pasteboard",
    "clipboard",
    "history",
    "transcripts",
    "deliver",
    "delivery",
    "detokenize",
    "detokenization",
    "ptt",
];

/// Multi-word names that are never exposed (words split on any non-alphanumeric).
pub const FORBIDDEN_PHRASES: &[&[&str]] = &[
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

/// `Some(reason)` when `name` (a tool name, or an app command name) is never exposed.
pub fn forbidden(name: &str) -> Option<String> {
    let toks = tokens(name);
    for t in &toks {
        if FORBIDDEN_TOKENS.contains(&t.as_str()) {
            return Some(format!(
                "`{t}` is never exposed to agents (no dictation, paste, pasteboard or history)"
            ));
        }
    }
    for p in FORBIDDEN_PHRASES {
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
    None
}

/// Generic control methods that can read arbitrary app content back out.
/// `wait` evaluates page script and `batch` carries any of them, so both count.
pub const GENERIC_READ_METHODS: &[&str] = &["eval", "dom", "ax", "screenshot", "wait", "batch"];
