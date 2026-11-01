//! Polish-only macOS AX context probe. Runs exclusively on the engine's
//! detached AX thread, so bounded Chromium/Electron retries never delay
//! recording, KWS, pill, or paste delivery.
//!
//! Split out of the former single-file `macos/focus.rs`
//! (docs/agent-rules.md: pure move, no logic changes).

use core_foundation::base::{CFType, TCFType};

use super::super::ffi::{ax_copy, ax_string, AXIsProcessTrusted, AXUIElementRef};
use super::context::{
    ax_text_marker_string, focused_descendant, focused_element_for_pid, focused_window_for_pid,
    truncate_middle, FocusedField,
};

/// Compatibility hook retained for engine lifecycle symmetry. Polish probes
/// are read-only; focus-pin accessibility observation is owned by
/// `FocusPin` and released by its lease's `Drop` implementation.
pub fn release_polish_accessibility_for_pid(_pid: i32, _generation: u64) {}

pub fn new_polish_accessibility_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Polish-only macOS context probe. Runs exclusively on engine's detached AX
/// thread, so bounded Chromium retries never delay recording, KWS, pill, or
/// paste delivery. Existing paste-target AX behavior stays untouched.
///
/// Fast path first. If no editable candidate exists, signal Chromium and poll
/// up to its measured lazy-tree budget. This function only runs on detached
/// polish threads.
pub fn polish_field_context_for_pid(
    pid: i32,
    generation: u64,
    max_chars: usize,
) -> Option<FocusedField> {
    polish_field_context_for_pid_with_budget(
        pid,
        generation,
        max_chars,
        std::time::Duration::from_millis(2_400),
    )
}

/// Stop-time probe returns within finalizer's bounded receive. Start-time probe
/// continues warming independently when Chromium's tree is still materializing.
pub fn polish_field_context_for_pid_at_stop(
    pid: i32,
    _generation: u64,
    max_chars: usize,
) -> Option<FocusedField> {
    let (field, _) = attempt_polish_capture(pid, max_chars, std::time::Duration::ZERO);
    field
}

pub(in crate::desktop::macos) fn polish_field_context_for_pid_with_budget(
    pid: i32,
    _generation: u64,
    max_chars: usize,
    escalation_budget: std::time::Duration,
) -> Option<FocusedField> {
    let (fast, fast_found_candidate) =
        attempt_polish_capture(pid, max_chars, std::time::Duration::from_millis(100));
    if fast_found_candidate {
        return fast;
    }

    let (escalated, escalated_found_candidate) =
        attempt_polish_capture(pid, max_chars, escalation_budget);
    if escalated_found_candidate {
        return escalated;
    }
    escalated.or(fast)
}

/// Second element of the return: did `focused_descendant` actually score a
/// candidate, independent of whether that candidate's text was empty.
pub(in crate::desktop::macos) fn attempt_polish_capture(
    pid: i32,
    max_chars: usize,
    retry_budget: std::time::Duration,
) -> (Option<FocusedField>, bool) {
    const RETRY_POLL: std::time::Duration = std::time::Duration::from_millis(50);
    let deadline = std::time::Instant::now() + retry_budget;
    let mut best = None;
    loop {
        if let Some((field, found_candidate)) = polish_field_context_once(pid, max_chars) {
            best = Some(field);
            if found_candidate {
                return (best, true);
            }
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return (best, false);
        }
        std::thread::sleep((deadline - now).min(RETRY_POLL));
    }
}

pub(in crate::desktop::macos) fn polish_field_context_once(
    pid: i32,
    max_chars: usize,
) -> Option<(FocusedField, bool)> {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return None;
        }
        let root_owner = focused_element_for_pid(pid)?;
        let root = root_owner.as_CFTypeRef() as AXUIElementRef;
        if ax_string(root, "AXRole").as_deref() == Some("AXSecureTextField")
            || ax_string(root, "AXSubrole").as_deref() == Some("AXSecureTextField")
        {
            return None;
        }
        let window_owner = focused_window_for_pid(pid);
        let scan_deadline = std::time::Instant::now() + std::time::Duration::from_millis(350);
        let candidate = focused_descendant(&root_owner, scan_deadline).or_else(|| {
            window_owner
                .as_ref()
                .and_then(|window| focused_descendant(window, scan_deadline))
        });
        let found_candidate = candidate.is_some();
        let focused_owner = candidate.unwrap_or_else(|| root_owner.clone());
        let focused = focused_owner.as_CFTypeRef() as AXUIElementRef;
        let role = ax_string(focused, "AXRole");
        if role.as_deref() == Some("AXSecureTextField")
            || ax_string(focused, "AXSubrole").as_deref() == Some("AXSecureTextField")
        {
            return None;
        }
        let clean = |text: String| Some(text.trim().to_string()).filter(|text| !text.is_empty());
        const PLAUSIBLE_CHARS_CEILING: usize = 20_000;
        let bounded =
            |text: String| (text.chars().count() <= PLAUSIBLE_CHARS_CEILING).then_some(text);
        let (field_text, selected_text) = if found_candidate {
            let field_text = ax_string(focused, "AXValue")
                .or_else(|| ax_text_marker_string(focused, "AXTextMarkerRangeForUIElement"))
                .and_then(clean)
                .and_then(bounded)
                .map(|text| truncate_middle(&text, max_chars));
            let selected_text = ax_string(focused, "AXSelectedText")
                .or_else(|| ax_text_marker_string(focused, "AXSelectedTextMarkerRange"))
                .and_then(clean)
                .and_then(bounded)
                .map(|text| truncate_middle(&text, max_chars));
            (field_text, selected_text)
        } else {
            (None, None)
        };
        let window_title = if let Some(window) = window_owner.as_ref() {
            ax_string(window.as_CFTypeRef() as AXUIElementRef, "AXTitle").and_then(clean)
        } else if role.as_deref() == Some("AXWindow") {
            ax_string(focused, "AXTitle").and_then(clean)
        } else if ax_string(root, "AXRole").as_deref() == Some("AXWindow") {
            ax_string(root, "AXTitle").and_then(clean)
        } else {
            ax_copy(focused, "AXWindow").and_then(|window| {
                let window = CFType::wrap_under_create_rule(window);
                ax_string(window.as_CFTypeRef() as AXUIElementRef, "AXTitle").and_then(clean)
            })
        };
        if !found_candidate
            && window_title.is_none()
            && field_text.is_none()
            && selected_text.is_none()
        {
            return None;
        }
        Some((
            FocusedField {
                window_title,
                field_text,
                selected_text,
                char_before_caret: None,
            },
            found_candidate,
        ))
    }
}
