//! Dictation-grade desktop context (feature `desktop-context`).
//!
//! Focus continuity: capture the destination at the moment a recording stops and prove, before
//! delivery, that it is still the same editor (`capture_focus_continuity`,
//! `validate_focus_continuity`, `start_focus_observer`; macOS and Windows only). Platform glue lives in `macos`
//! (clipboard staging with exact pasteboard generations, frontmost-window identity through an
//! isolated helper, window text harvest, AX focus proofs and native writes) and `windows`
//! (clipboard staging with full-format restore, focus observer, promised clipboard).

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "macos")]
mod macos_focus_continuity;
#[cfg(target_os = "windows")]
mod windows_focus_continuity;

#[cfg(target_os = "macos")]
pub use macos_focus_continuity::{
    arm_focus_coverage, begin_focus_coverage, capture_continuous_candidate,
    capture_focus_continuity, capture_focus_continuity_with_editor, capture_hotkey_focus_anchor,
    end_focus_coverage, focus_continuity_capture_miss_reason, focus_continuity_status,
    focus_coverage_wanted, focus_observer_debug_snapshot, set_focus_observer_diagnostic_hook,
    start_focus_observer, validate_focus_continuity, validate_focus_continuity_settled,
    wait_for_focus_continuity, wait_for_focus_coverage, FocusContinuityStatus, HotkeyFocusAnchor,
    COVERAGE_READY_WAIT,
};
#[cfg(target_os = "windows")]
pub use windows_focus_continuity::{
    capture_focus_continuity, start_focus_observer, validate_focus_continuity,
};

/// Observers in engine & shell share an OS clock, not a process-local epoch.
/// Shell coverage must span stop through a recent completed observer pass.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(crate) fn continuity_window_valid(
    captured_at: u64,
    now: u64,
    coverage_started: u64,
    last_change: u64,
    last_progress: u64,
    max_staleness: u64,
) -> bool {
    captured_at != 0
        && coverage_started != 0
        && coverage_started <= captured_at
        && captured_at <= now
        && last_change <= captured_at
        && last_progress >= captured_at
        && last_progress <= now
        && now - last_progress <= max_staleness
}

#[cfg(test)]
mod continuity_tests {
    use super::continuity_window_valid as valid;

    #[test]
    fn separate_process_observers_can_cover_same_stop_timestamp() {
        assert!(valid(100, 200, 10, 90, 190, 30));
        assert!(valid(100, 200, 50, 95, 190, 30));
    }

    #[test]
    fn later_focus_change_stays_invalid_after_return_to_old_editor() {
        assert!(!valid(100, 200, 10, 110, 190, 30));
        assert!(!valid(100, 200, 10, 150, 190, 30));
    }

    #[test]
    fn late_start_restart_or_stalled_observer_cannot_prove_continuity() {
        assert!(!valid(100, 200, 110, 90, 190, 30));
        assert!(!valid(100, 200, 10, 90, 150, 30));
        assert!(!valid(100, 200, 0, 90, 190, 30));
        assert!(!valid(0, 200, 10, 90, 190, 30));
        assert!(!valid(100, 200, 10, 90, 99, 120));
    }
}

// Shared, cfg-free entry points for the small set of primitives that exist on
// both `macos` and `windows` with the same purpose AND the same signature
// (bool/Option<String> success semantics, no platform-specific proof types
// involved). Callers use these instead of repeating `#[cfg(target_os = ...)]`
// dispatch at each call site. Primitives whose two sides carry different
// error/identity semantics (e.g. the accessibility-gated paste/enter used by
// `src-tauri/src/delivery/backend.rs` /
// the host's insertion path, or clipboard
// snapshot/restore around
// `PasteboardSnapshot` vs `ClipboardSnapshot`) are deliberately NOT collapsed
// here — see the divergence-inventory report for why.

/// Write plain text to the system clipboard. Returns `true` on success.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn clipboard_write(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::set_clipboard(text)
    }
    #[cfg(target_os = "windows")]
    {
        windows::write_clipboard_text(text)
    }
}

/// Rich editors (ProseMirror in ChatGPT/Claude) store a
/// pasted blank-line paragraph break as two blocks and report it back as ONE
/// line break. Paste readback compares under this canonical form so a
/// multi-paragraph transcript can still be proven exactly. Only runs of line
/// breaks collapse; every other character must still match.
pub fn collapse_line_break_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_break = false;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            if !in_break {
                out.push('\n');
            }
            in_break = true;
        } else {
            in_break = false;
            out.push(ch);
        }
    }
    out
}

/// Verdict on whether a recorded paste may still be undone with Ctrl/Cmd+Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoVerdict {
    /// The field still ends with, or holds at the recorded caret, exactly the
    /// inserted text.
    Safe,
    /// Nothing was recorded to verify against.
    NoRecordedPaste,
    /// The field could not be read (secure, unsupported, AX/UIA failure).
    FieldUnreadable,
    /// The field no longer holds the inserted text where we put it.
    TextChanged,
}

/// Decide whether the last paste is still the thing Cmd/Ctrl+Z would undo.
/// `field_text` is the AX/UIA readback of the focused field; `inserted` is
/// the text we delivered; `caret` is the recorded character offset just after
/// the insertion, when one was recorded. With a caret, the field must hold
/// `inserted` ending exactly there; without one, it must end with `inserted`
/// (trailing whitespace ignored). Comparison uses the documented
/// `collapse_line_break_runs` form (spec section 5.1 rule 5) and nothing else.
pub fn verify_last_paste_undoable(
    field_text: Option<&str>,
    inserted: Option<&str>,
    caret: Option<usize>,
) -> UndoVerdict {
    let Some(inserted) = inserted.map(str::trim).filter(|t| !t.is_empty()) else {
        return UndoVerdict::NoRecordedPaste;
    };
    let Some(field) = field_text else {
        return UndoVerdict::FieldUnreadable;
    };
    let inserted = collapse_line_break_runs(inserted);
    let field = collapse_line_break_runs(field);
    let haystack: String = match caret {
        Some(offset) => field.chars().take(offset).collect(),
        None => field,
    };
    if haystack.trim_end().ends_with(inserted.as_str()) {
        UndoVerdict::Safe
    } else {
        UndoVerdict::TextChanged
    }
}

#[cfg(test)]
mod undo_verdict_tests {
    use super::{verify_last_paste_undoable as v, UndoVerdict::*};

    #[test]
    fn undo_only_when_field_still_ends_with_the_inserted_text() {
        assert_eq!(v(Some("hi there"), Some("there"), None), Safe);
        assert_eq!(v(Some("hi there \n"), Some("there"), None), Safe);
        assert_eq!(v(Some("a\nb"), Some("a\n\nb"), None), Safe);
        assert_eq!(v(Some("hi there!"), Some("there"), None), TextChanged);
        assert_eq!(v(Some("other"), Some("there"), None), TextChanged);
        assert_eq!(v(None, Some("there"), None), FieldUnreadable);
        assert_eq!(v(Some("x"), None, None), NoRecordedPaste);
        assert_eq!(v(Some("x"), Some("  "), None), NoRecordedPaste);
    }

    #[test]
    fn recorded_caret_must_sit_right_after_the_inserted_text() {
        assert_eq!(v(Some("hi there more"), Some("there"), Some(8)), Safe);
        assert_eq!(
            v(Some("hi there more"), Some("there"), Some(13)),
            TextChanged
        );
    }
}

#[cfg(test)]
mod line_break_tests {
    #[test]
    fn only_runs_of_line_breaks_collapse() {
        assert_eq!(
            super::collapse_line_break_runs("a\n\nb\r\n\r\nc d"),
            "a\nb\nc d"
        );
        assert_eq!(super::collapse_line_break_runs("a  b"), "a  b");
    }
}
