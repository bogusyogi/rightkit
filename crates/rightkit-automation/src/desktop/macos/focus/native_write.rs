//! Native (non-clipboard) selection-write capability probe and the direct
//! `AXSelectedText` write it gates — HR-DELIVERY-005. Capability decision and
//! write attempt only; callers must confirm via `insertion_confirmed_for_pid`
//! against a `FocusedFieldProof` (HR-DELIVERY-006).
//!
//! Split out of the former single-file `macos/focus.rs`
//! (docs/agent-rules.md: pure move, no logic changes).

use core_foundation::base::{CFType, TCFType};

use super::super::ffi::{
    ax_attribute_settable, ax_copy, ax_element_pid, ax_set_str, ax_string, AXIsProcessTrusted,
    AXUIElementRef,
};
use super::context::{ax_text_marker_string, focused_element_for_pid, secure_state_for_roles};
use super::pin::{focus_pin_running_identity_matches, FocusPin};

/// Capability probe for native (non-clipboard) insertion — HR-DELIVERY-005.
/// True only when the CURRENT focused element, for the CURRENT process,
/// genuinely accepts a direct write to its selection AND this codebase can
/// trust that write to be a real native AppKit/UIKit text control rather than
/// web/Electron/rich-editor content — see the 0.1.146 history below for why
/// the second condition is load-bearing. This is a per-element accessibility
/// capability check, never an app-name allowlist. Four independent signals
/// must ALL hold:
///   1. AX-trusted, and a focused element resolves for `pid`.
///   2. Not a secure (password) field.
///   3. No `AXWebArea` ancestor within a bounded `AXParent` walk
///      (`has_web_area_ancestor`) — Safari, Chrome/Electron content, and
///      WebKit-hosted rich editors (the exact class of target 0.1.146 broke)
///      live inside an `AXWebArea` subtree; a native AppKit `NSTextField`/
///      `NSTextView` control never does.
///   4. `AXValue` is DIRECTLY readable as a plain string (no text-marker
///      fallback needed) AND `AXSelectedText` reports settable via
///      `AXUIElementIsAttributeSettable`. Chromium/WebKit text generally does
///      not expose a direct `AXValue` string at all — the marker-range APIs
///      (`ax_text_marker_string`) exist in this module specifically because
///      that content requires them — so requiring a direct read is an
///      independent, technical signal against the same web/rich-editor
///      class signal 3 targets, catching cases where only one signal fires.
///
/// Writing `AXSelectedText` replaces exactly the current selection (a
/// collapsed selection is just the caret) without touching the rest of the
/// control's value, satisfying selection-exact replacement (HR-DELIVERY-010)
/// unlike a whole-`AXValue` rewrite.
///
/// A native-write attempt gated on (2) + settable-`AXSelectedText` alone
/// (signal 4's second half only, without 3 or the first half of 4) is unsafe
/// — see the regression test `src-tauri/tests/paste_evidence_gated_restore_contract_tests.rs::
/// macos_capability_gated_native_insertion_requires_exact_read_back_proof`.
/// Safari and Claude (both WebKit/web content)
/// can report `AXSelectedText` settable, with the setter returning success, but the
/// edit either never applies or applies without a readable value to confirm
/// it — breaking delivery silently. Signals 3 and 4 close that hole at the
/// CAPABILITY-DECISION stage, before any write is attempted, so the
/// decide-before-mutate requirement (HR-DELIVERY-008) holds: this function is
/// never expected to report capable for a WebKit/Chromium/Electron target.
/// The write itself is additionally never trusted on its own —
/// `write_native_selected_text_for_pid`'s return is not proof; every caller
/// MUST confirm via `insertion_confirmed_for_pid` against a
/// `FocusedFieldProof` captured beforehand (HR-DELIVERY-006).
///
/// Wired into both live delivery paths this repo owns for this capability —
/// `src-tauri/src/delivery/backend.rs` /
/// the host's insertion path —
/// see `docs/dispatch/evidence/native-insertion-capability-gated.md`.
pub fn native_selection_write_capable_for_pid(pid: i32) -> bool {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return false;
        }
        let Some(owner) = focused_element_for_pid(pid) else {
            return false;
        };
        let element = owner.as_CFTypeRef() as AXUIElementRef;
        if ax_string(element, "AXRole").as_deref() == Some("AXSecureTextField")
            || ax_string(element, "AXSubrole").as_deref() == Some("AXSecureTextField")
        {
            return false;
        }
        if has_web_area_ancestor(element) {
            return false;
        }
        if ax_string(element, "AXValue").is_none() {
            return false;
        }
        ax_attribute_settable(element, "AXSelectedText")
    }
}

/// Capability probe against the exact editor retained at stop time. This
/// avoids rediscovering the current focused element by PID after processing.
pub fn native_selection_write_capable_for_pin(pin: &FocusPin) -> bool {
    unsafe {
        if AXIsProcessTrusted() == 0 || !focus_pin_running_identity_matches(pin) {
            return false;
        }
        let element = pin.element.as_CFTypeRef() as AXUIElementRef;
        if ax_element_pid(element) != Some(pin.pid)
            || secure_state_for_roles(
                ax_string(element, "AXRole").as_deref(),
                ax_string(element, "AXSubrole").as_deref(),
            ) != Some(false)
        {
            return false;
        }
        if has_web_area_ancestor(element) || ax_string(element, "AXValue").is_none() {
            return false;
        }
        ax_attribute_settable(element, "AXSelectedText")
    }
}

/// Bounded walk up `AXParent` looking for an `AXWebArea` ancestor — the
/// signal that a focused element lives inside web/Electron/WebKit content
/// rather than a native AppKit control. See
/// `native_selection_write_capable_for_pid` (signal 3). Bounded to 32 hops
/// so a cyclic or unexpectedly deep AX tree cannot hang the capability probe
/// (mirrors the `MAX_VISITS`/deadline bounds used elsewhere in this module).
pub(in crate::desktop::macos) unsafe fn has_web_area_ancestor(element: AXUIElementRef) -> bool {
    let mut current = CFType::wrap_under_get_rule(element);
    for _ in 0..32 {
        let current_ref = current.as_CFTypeRef() as AXUIElementRef;
        if ax_string(current_ref, "AXRole").as_deref() == Some("AXWebArea") {
            return true;
        }
        let Some(parent) = ax_copy(current_ref, "AXParent") else {
            return false;
        };
        current = CFType::wrap_under_create_rule(parent);
    }
    false
}

/// Attempt the native write itself: post `text` directly into the focused
/// element's `AXSelectedText`. Returns whether the AX SETTER call reported
/// success.
///
/// This return value is NOT insertion proof (HR-DELIVERY-006) — an
/// `AXUIElementSetAttributeValue` success only means the target ACKNOWLEDGED
/// the call, exactly the same weak signal a synthetic keystroke's "posted OK"
/// is for the clipboard+paste path. Every caller MUST additionally confirm
/// via `insertion_confirmed_for_pid` against a `FocusedFieldProof` captured
/// with `focused_field_proof_for_pid` BEFORE this call, and must treat a
/// setter success with unconfirmed read-back as failure, not as delivered —
/// see `rightkit_transcript_library::delivery::InsertionProof`.
pub fn write_native_selected_text_for_pid(pid: i32, text: &str) -> bool {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return false;
        }
        let Some(owner) = focused_element_for_pid(pid) else {
            return false;
        };
        let element = owner.as_CFTypeRef() as AXUIElementRef;
        ax_set_str(element, "AXSelectedText", text)
    }
}

/// Attempt native insertion into exact retained editor from stop capture.
/// Caller must verify read-back with [`crate::desktop::macos::insertion_confirmed_for_pin`].
pub fn write_native_selected_text_for_pin(pin: &FocusPin, text: &str) -> bool {
    unsafe {
        if AXIsProcessTrusted() == 0
            || !focus_pin_running_identity_matches(pin)
            || ax_element_pid(pin.element.as_CFTypeRef() as AXUIElementRef) != Some(pin.pid)
        {
            return false;
        }
        ax_set_str(
            pin.element.as_CFTypeRef() as AXUIElementRef,
            "AXSelectedText",
            text,
        )
    }
}

/// Read-back evidence that `needle` is now present in the focused element's
/// field text or selection - proof that a clipboard paste landed. Deliberately
/// untruncated (unlike `focused_field_context_for_pid`, which caps for
/// polish-context privacy): a truncated read could miss a long transcript
/// landing inside a long pre-existing field and misreport "not consumed".
pub fn focused_element_contains_text(pid: i32, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return true;
    }
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return false;
        }
        let Some(focused_owner) = focused_element_for_pid(pid) else {
            return false;
        };
        let focused = focused_owner.as_CFTypeRef() as AXUIElementRef;
        let field_text = ax_string(focused, "AXValue")
            .or_else(|| ax_text_marker_string(focused, "AXTextMarkerRangeForUIElement"));
        if field_text
            .as_deref()
            .is_some_and(|text| text.contains(needle))
        {
            return true;
        }
        let selected_text = ax_string(focused, "AXSelectedText")
            .or_else(|| ax_text_marker_string(focused, "AXSelectedTextMarkerRange"));
        selected_text
            .as_deref()
            .is_some_and(|text| text.contains(needle))
    }
}
