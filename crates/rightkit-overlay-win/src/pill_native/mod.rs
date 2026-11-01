//! Native Windows pill, hint and clip overlays .
//!
//! The Windows pill is no longer a WebView2 window. It is three layered, non-activating
//! overlays (`pill`, `Pill Hint`, `Pill Recent`) drawn by `rightkit_overlay_win`
//! from the same design source as the macOS pill (the Swift sources under
//! the native macOS pill sources).
//!
//! * `visuals`    tokens, geometry, text metrics, bars, icons and display-list builders (pure)
//! * `pill`       the capsule model (`PillModel.swift` port) and its painter
//! * `hint`       the hint card (`HintSurface`)
//! * `clipboard`  the single "clip" and recent "mini clip" card (`ClipboardSurface`)
//! * `prompt`     the AI-target tip trigger that used to live in the pill webview
//! * `driver`     the one owner thread: state, timers, hover, clicks, placement, frame pumps
//! * `gfx`        the only file that touches `rightkit_overlay_win`
//!
//! The state machine (`pill/state.rs`) still decides what the pill shows: it calls
//! `pill::show_event`, which reaches [`apply_event`] through `webview_show_event`. Idle cost is
//! one parked thread per overlay event queue plus the driver blocked in `recv()`: no timer runs
//! until something animates.

#![allow(dead_code)]

pub mod actions;
pub mod clipboard;
pub mod hint;
pub mod pill;
pub mod prompt;
pub mod visuals;

#[cfg(target_os = "windows")]
mod driver;
#[cfg(target_os = "windows")]
mod gfx;

#[cfg(target_os = "windows")]
pub use driver::{
    apply_capture_affinity, apply_event, capsule_rect, clipboard_visible, dismiss_clipboard,
    dismiss_hint, ensure_installed, hint_visible, is_visible, monitor_rect, overlay_frames,
    overlay_hwnds, reassert_on, reassert_topmost, reposition, reveal, set_visibility,
    show_clipboard, show_hint, voice_level, Visibility,
};
#[cfg(target_os = "windows")]
pub(crate) use driver::{Cmd, Shared, Surf};

use hint::HintContent;

/// Map a hint kind (`screenshot_prompt`, `prompt_hint`, or a catalog key) to its card content.
/// `None` for unknown kinds and for hints scoped to another OS.
pub fn hint_content_for_kind(kind: &str) -> Option<HintContent> {
    match kind {
        "screenshot_prompt" => Some(HintContent::Screenshot),
        "prompt_hint" => Some(HintContent::Prompt),
        other => visuals::feature_hint(other).map(|text| HintContent::Feature {
            title: text.title,
            body: text.body,
        }),
    }
}

/// Show the hint named by `kind`. Returns `false` (and logs) when there is nothing to show.
#[cfg(target_os = "windows")]
pub fn show_hint_kind(kind: &str) -> bool {
    match hint_content_for_kind(kind) {
        Some(content) => {
            show_hint(content, kind);
            true
        }
        None => {
            super::actions::log_message("pill_hint_unknown_kind", &format!("kind={kind}"));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bespoke_hints_have_their_own_cards() {
        assert_eq!(
            hint_content_for_kind("screenshot_prompt"),
            Some(HintContent::Screenshot)
        );
        assert_eq!(
            hint_content_for_kind("prompt_hint"),
            Some(HintContent::Prompt)
        );
    }

    #[test]
    fn catalog_hints_carry_the_windows_text() {
        match hint_content_for_kind("stop_and_send") {
            Some(HintContent::Feature { title, body }) => {
                assert_eq!(title, "Paste and send in one go");
                assert!(body.contains("Ctrl+Shift+Space"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_and_macos_only_hints_are_dropped() {
        assert_eq!(hint_content_for_kind("nope"), None);
        assert_eq!(hint_content_for_kind("apple_shortcuts"), None);
        assert_eq!(hint_content_for_kind("_comment"), None);
    }

    #[test]
    fn every_hint_kind_the_app_can_queue_resolves_on_windows() {
        // `note_completed_transcript_for_hints` cycles these; Windows swaps apple_shortcuts for
        // voice_commands before it reaches the card.
        for kind in [
            "voice_commands",
            "vocabulary",
            "snippets",
            "ai_polish",
            "file_transcription",
            "groq_key",
            "mini_clipboard",
            "stop_and_send",
            "cancel_saved",
        ] {
            assert!(hint_content_for_kind(kind).is_some(), "{kind}");
        }
    }
}
