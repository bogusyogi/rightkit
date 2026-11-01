//! The "Say prompt at the end ..." tip trigger.
//!
//! This flow used to live in the pill webview (`usePillPromptHintSubscription` in
//! `src/pill/usePillEvents.ts`): on every `delivery-recorded` it decided from the delivery's
//! target whether the user was dictating into an AI tool, counted those deliveries in
//! `localStorage`, and offered the tip once, at the third. With the webview gone the same rules
//! run here and persist in `prompt-hint.json` instead of `localStorage`.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// `PROMPT_HINT_THRESHOLD`: the tip is offered on the third AI-tool delivery.
pub const THRESHOLD: u32 = 3;

const ANY_AI: [&str; 12] = [
    "codex",
    "claude",
    "chatgpt",
    "openai",
    "grok",
    "gemini",
    "perplexity",
    "cursor",
    "windsurf",
    "copilot",
    "cline",
    "continue",
];
const TERMINAL_OR_EDITOR: [&str; 9] = [
    "code-insiders",
    "windowsterminal",
    "powershell",
    "alacritty",
    "conhost",
    "wezterm",
    "pwsh",
    "code",
    "cmd",
];
const TERMINAL_TITLE: [&str; 9] = [
    "codex",
    "claude code",
    "claude",
    "chatgpt",
    "openai",
    "grok",
    "gemini",
    "llm",
    "ai",
];
const BROWSERS: [&str; 6] = ["chrome", "msedge", "firefox", "brave", "arc", "opera"];
const BROWSER_TITLE: [&str; 7] = [
    "chatgpt",
    "claude",
    "grok",
    "gemini",
    "perplexity",
    "openai",
    "poe",
];

fn any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// `isLikelyAiTarget`, ported verbatim (substring matches on the lower-cased process name and
/// window title, which is what the JS regexes did).
pub fn is_likely_ai_target(process: Option<&str>, title: Option<&str>) -> bool {
    let process = process.unwrap_or_default().to_lowercase();
    let title = title.unwrap_or_default().to_lowercase();
    let haystack = format!("{process} {title}");
    if haystack.trim().is_empty() {
        return false;
    }
    if any(&haystack, &ANY_AI) {
        return true;
    }
    // `wt` (Windows Terminal) is a bare substring in the original pattern.
    if any(&process, &TERMINAL_OR_EDITOR) || process.contains("wt") {
        return any(&title, &TERMINAL_TITLE);
    }
    if any(&process, &BROWSERS) {
        return any(&title, &BROWSER_TITLE);
    }
    false
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptHintStore {
    /// Deliveries into AI targets seen so far (`hr_prompt_hint_ai_recordings_v1`).
    #[serde(default)]
    pub count: u32,
    /// The tip was already shown once (`hr_prompt_hint_shown_v1`).
    #[serde(default)]
    pub seen: bool,
}

impl PromptHintStore {
    /// Record one delivery. Returns `true` when the tip should be offered now.
    pub fn record(&mut self, likely_ai_target: bool) -> bool {
        if !likely_ai_target || self.seen {
            return false;
        }
        self.count = self.count.saturating_add(1);
        self.count >= THRESHOLD
    }

    pub fn mark_seen(&mut self) {
        self.seen = true;
    }
}

/// What the pill webview kept in `localStorage` before this flow moved to `prompt-hint.json`:
/// `hr_prompt_hint_ai_recordings_v1`, `hr_prompt_hint_shown_v1` and `hr_prompt_hint_disabled`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegacyPromptHint {
    pub count: u32,
    pub seen: bool,
    pub disabled: bool,
}

/// The store to write for a legacy state, or `None` when `prompt-hint.json` already exists and
/// stays as it is. A tip the user switched off or was already shown counts as seen, so an upgrade
/// does not offer it a second time.
pub fn legacy_import(
    existing: Option<PromptHintStore>,
    legacy: LegacyPromptHint,
) -> Option<PromptHintStore> {
    existing.is_none().then_some(PromptHintStore {
        count: legacy.count,
        seen: legacy.seen || legacy.disabled,
    })
}

/// One-time carry-over of the webview's tip state into `path`. Returns whether it wrote.
pub fn import_legacy(path: &std::path::Path, legacy: LegacyPromptHint) -> bool {
    let existing = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PromptHintStore>(&bytes).ok());
    // An unreadable file is not "absent": leave it for the normal flow to rewrite.
    if existing.is_none() && path.exists() {
        return false;
    }
    let Some(store) = legacy_import(existing, legacy) else {
        return false;
    };
    serde_json::to_vec_pretty(&store)
        .ok()
        .is_some_and(|bytes| super::actions::replace_file_atomically(path, &bytes).is_ok())
}

#[cfg(target_os = "windows")]
pub fn on_delivery_recorded(process: Option<&str>, title: Option<&str>) {
    let likely = is_likely_ai_target(process, title);
    if !likely {
        return;
    }
    let path = super::actions::actions()
        .map(|actions| actions.data_file("prompt-hint.json"))
        .unwrap_or_default();
    let mut store: PromptHintStore = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let show = store.record(true);
    if show {
        store.mark_seen();
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&store) {
        let _ = super::actions::replace_file_atomically(&path, &bytes);
    }
    super::actions::log_message(
        "prompt_hint_trigger",
        &format!(
            "count={} outcome={}",
            store.count,
            if show { "open" } else { "below_threshold" }
        ),
    );
    if show {
        super::show_hint_kind("prompt_hint");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_apps_match_anywhere() {
        assert!(is_likely_ai_target(Some("Cursor.exe"), Some("main.rs")));
        assert!(is_likely_ai_target(Some("notepad"), Some("Claude notes")));
        assert!(is_likely_ai_target(Some("slack"), Some("chat with Gemini")));
    }

    #[test]
    fn terminals_and_editors_need_an_ai_word_in_the_title() {
        assert!(is_likely_ai_target(
            Some("WindowsTerminal.exe"),
            Some("claude code")
        ));
        assert!(is_likely_ai_target(
            Some("code.exe"),
            Some("my llm project")
        ));
        assert!(!is_likely_ai_target(Some("pwsh.exe"), Some("build output")));
        // `wt` and `ai` are bare substrings in the original patterns.
        assert!(is_likely_ai_target(Some("wt.exe"), Some("repair")));
    }

    #[test]
    fn browsers_need_a_chat_site_in_the_title() {
        assert!(is_likely_ai_target(
            Some("chrome.exe"),
            Some("Perplexity - search")
        ));
        assert!(!is_likely_ai_target(
            Some("firefox.exe"),
            Some("Weather today")
        ));
    }

    #[test]
    fn unknown_or_empty_targets_are_not_ai() {
        assert!(!is_likely_ai_target(None, None));
        assert!(!is_likely_ai_target(Some(""), Some("")));
        assert!(!is_likely_ai_target(
            Some("explorer.exe"),
            Some("Downloads")
        ));
    }

    #[test]
    fn the_tip_is_offered_on_the_third_ai_delivery_once() {
        let mut store = PromptHintStore::default();
        assert!(!store.record(true));
        assert!(!store.record(false), "non-AI deliveries do not count");
        assert!(!store.record(true));
        assert!(store.record(true), "third AI delivery");
        store.mark_seen();
        assert!(!store.record(true), "never again once shown");
        assert_eq!(store.count, 3);
    }

    #[test]
    fn store_round_trips_through_json() {
        let store = PromptHintStore {
            count: 2,
            seen: false,
        };
        let json = serde_json::to_string(&store).unwrap();
        assert_eq!(
            serde_json::from_str::<PromptHintStore>(&json).unwrap(),
            store
        );
        assert_eq!(
            serde_json::from_str::<PromptHintStore>("{}").unwrap(),
            PromptHintStore::default()
        );
    }
}
