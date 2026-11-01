//! `FocusedFieldProof` snapshot capture and insertion-confirmation proof: the
//! before/after read pairing that attributes a mutation to the app's own
//! write rather than to text that merely happened to already be there.
//!
//! Split out of the former single-file `macos/focus.rs`
//! (docs/agent-rules.md: pure move, no logic changes).

use core_foundation::base::{CFType, TCFType};

use super::super::ffi::{
    ax_copy, ax_element_pid, ax_frame, ax_selected_range, ax_string, ax_string_bounded,
    AXIsProcessTrusted, AXUIElementRef, AXUIElementSetMessagingTimeout,
    AX_MESSAGING_TIMEOUT_SECONDS, MAX_PROOF_UTF8_BYTES,
};
use super::super::frontmost::frontmost_pid;
use super::context::{
    ax_text_marker_string, focused_descendant, focused_element_for_pid, focused_window_for_pid,
    secure_state_for_roles,
};
use super::pin::{
    ax_window_for_element, focus_pin_running_identity_matches, focus_pin_window_matches,
    resolved_focused_editor, running_process_instance, FocusPin,
};

/// Which accessibility representation produced a proof's text.
///
/// The before and after reads for one proof MUST use the SAME representation.
/// `AXValue` and the text-marker APIs disagree on rich editors — Chromium and
/// WebKit publish composer text only through marker ranges, and a WebKit editor
/// can expose a placeholder-like `AXValue` while the marker range describes the
/// real document — so a proof captured as `AXValue` and confirmed through the
/// marker APIs reports a real insertion as unproven (and vice versa).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::desktop::macos) enum FieldTextSource {
    Value,
    TextMarker,
}

#[derive(Clone, Debug)]
pub(in crate::desktop::macos) struct EditorFingerprint {
    role: String,
    subrole: Option<String>,
    identifier: Option<String>,
    description: Option<String>,
    x: i64,
    y: i64,
    width: i64,
}

impl EditorFingerprint {
    fn logically_matches(&self, current: &Self) -> bool {
        let semantic_identity = self
            .identifier
            .as_ref()
            .is_some_and(|expected| current.identifier.as_ref() == Some(expected))
            || self
                .description
                .as_ref()
                .is_some_and(|expected| current.description.as_ref() == Some(expected));
        semantic_identity
            && self.role == current.role
            && self.subrole == current.subrole
            && self.x.abs_diff(current.x) <= 2
            && self.width.abs_diff(current.width) <= 2
            && self.y.abs_diff(current.y) <= 200
    }
}

/// Pre-insertion snapshot of the focused field. Verification compares the
/// field against THIS, so a confirmation is always attributable to our own
/// write rather than to text that merely happened to already be there.
#[derive(Clone, Debug, Default)]
pub struct FocusedFieldProof {
    value: Option<String>,
    /// Independent marker representation retained when AXValue is empty or
    /// unavailable; confirmation always reads the selected same source.
    marker_value: Option<String>,
    /// Representation `value` came from; the post-insertion read uses this one.
    source: Option<FieldTextSource>,
    range: Option<(usize, usize)>,
    // Retain the exact pre-paste editor. PID alone cannot identify a field
    // after focus changes, and a later field's text is not insertion proof.
    owner: Option<(i32, CFType)>,
    // Retained owning window of `owner`, so the send gate can prove the same
    // window is still the destination without re-deriving it by pid.
    owner_window: Option<CFType>,
    process_instance: Option<u64>,
    fingerprint: Option<EditorFingerprint>,
}

impl FocusedFieldProof {
    /// True when nothing readable was captured, so no mutation can be proven.
    pub fn is_blind(&self) -> bool {
        self.value.is_none() && self.marker_value.is_none()
    }

    /// Content-free diagnostics for retained AX proof shape.
    pub fn source_label(&self) -> &'static str {
        match self.source {
            Some(FieldTextSource::Value) => "ax_value",
            Some(FieldTextSource::TextMarker) => "text_marker",
            None => "none",
        }
    }

    pub fn content_fingerprint(&self) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let text = self.value.as_deref().or(self.marker_value.as_deref())?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        Some(hasher.finish())
    }

    pub fn range_known(&self) -> bool {
        self.range.is_some()
    }

    pub fn captured_chars(&self) -> usize {
        self.value
            .as_ref()
            .or(self.marker_value.as_ref())
            .map_or(0, |value| value.chars().count())
    }

    /// Read the retained editor through the SAME representation the proof's
    /// value came from. A blind proof or a vanished element is unproven.
    ///
    /// Both the capture and this read go through the same bounded reader, so a
    /// truncated pre-state can never be paired with a full post-state (the bound
    /// is symmetric by construction), and the two reads can never be compared
    /// across representations.
    fn read_current(&self) -> Option<String> {
        let (_, element) = self.owner.as_ref()?;
        let source = self.source?;
        unsafe { read_field_value_with_source(element.as_CFTypeRef() as AXUIElementRef, source) }
    }
}

pub(in crate::desktop::macos) fn occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

/// Read one named representation of an element's text.
pub(in crate::desktop::macos) unsafe fn read_field_value_with_source(
    element: AXUIElementRef,
    source: FieldTextSource,
) -> Option<String> {
    match source {
        FieldTextSource::Value => ax_string_bounded(element, "AXValue"),
        FieldTextSource::TextMarker => {
            ax_text_marker_string(element, "AXTextMarkerRangeForUIElement")
        }
    }
}

/// Read an element's text together with the representation that produced it,
/// so the confirmation read can be pinned to the same one.
pub(in crate::desktop::macos) unsafe fn read_field_value_sourced(
    element: AXUIElementRef,
) -> Option<(String, Option<String>, FieldTextSource)> {
    let value = ax_string_bounded(element, "AXValue");
    let marker = ax_text_marker_string(element, "AXTextMarkerRangeForUIElement");
    match (value, marker) {
        (Some(value), marker) => {
            let marker = marker
                .filter(|text| value.len().saturating_add(text.len()) <= MAX_PROOF_UTF8_BYTES);
            Some((value, marker, FieldTextSource::Value))
        }
        (None, Some(marker)) => Some((marker, None, FieldTextSource::TextMarker)),
        (None, None) => None,
    }
}

pub(in crate::desktop::macos) unsafe fn editor_fingerprint(
    element: AXUIElementRef,
) -> Option<EditorFingerprint> {
    let frame = ax_frame(element)?;
    let role = ax_string(element, "AXRole")?;
    let nonempty = |value: Option<String>| value.filter(|text| !text.trim().is_empty());
    Some(EditorFingerprint {
        role,
        subrole: nonempty(ax_string(element, "AXSubrole")),
        identifier: nonempty(ax_string(element, "AXDOMIdentifier"))
            .or_else(|| nonempty(ax_string(element, "AXIdentifier"))),
        description: nonempty(ax_string(element, "AXDescription")),
        x: frame.origin.x.round() as i64,
        y: frame.origin.y.round() as i64,
        width: frame.size.width.round() as i64,
    })
}

/// Snapshot a before/after proof of `element_owner` for `pid`.
///
/// Resolves the editable editor first: when the focused element itself exposes
/// no readable text (Chromium/WebKit composers reachable only through marker
/// ranges, or a focused container that hosts the real editor), the same bounded
/// descendant search the polish path uses locates the editor. Without that step
/// those targets stay `blind` and insertion can never be confirmed, which is
/// what left them permanently on the clipboard-recovery path.
pub(in crate::desktop::macos) fn focused_field_proof(
    element_owner: CFType,
    pid: i32,
) -> FocusedFieldProof {
    unsafe {
        let element = element_owner.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(element, AX_MESSAGING_TIMEOUT_SECONDS);
        let root_read = read_field_value_sourced(element);
        let root_role = ax_string(element, "AXRole");
        let root_is_native_editor = matches!(
            root_role.as_deref(),
            Some("AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField")
        );
        // An empty AXValue on a container is a placeholder, not proof that
        // container is editor. Let bounded descendant search resolve exact
        // focused editor; preserve empty native controls as valid proof.
        let root_has_useful_text = root_read.as_ref().is_some_and(|(value, marker, _)| {
            !value.is_empty() || marker.as_ref().is_some_and(|text| !text.is_empty())
        });
        let resolved = if root_is_native_editor || root_has_useful_text {
            element_owner.clone()
        } else {
            focused_descendant(
                &element_owner,
                std::time::Instant::now() + std::time::Duration::from_millis(350),
            )
            .unwrap_or_else(|| element_owner.clone())
        };
        let resolved_ref = resolved.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(resolved_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        let (value, marker_value, source) = match read_field_value_sourced(resolved_ref) {
            Some((value, marker, source)) => (Some(value), marker, Some(source)),
            None => (None, None, None),
        };
        // Fall back to the app's focused window when the element does not expose
        // AXWindow directly, mirroring `capture_focus_pin_detailed`.
        let owner_window = ax_copy(resolved_ref, "AXWindow")
            .map(|window| CFType::wrap_under_create_rule(window))
            .or_else(|| focused_window_for_pid(pid));
        FocusedFieldProof {
            value,
            marker_value,
            source,
            range: ax_selected_range(resolved_ref),
            owner: Some((pid, resolved)),
            owner_window,
            process_instance: running_process_instance(pid),
            fingerprint: editor_fingerprint(resolved_ref),
        }
    }
}

/// Attributable-delta check between two reads of the SAME representation of the
/// SAME retained editor. Extracted so both proof paths share exactly one rule.
pub(in crate::desktop::macos) fn confirm_insertion(
    before_value: &str,
    range: Option<(usize, usize)>,
    after_value: &str,
    needle: &str,
) -> bool {
    if confirm_insertion_any_representation(before_value, range, after_value, needle) {
        return true;
    }
    // Multi-paragraph transcript. ChatGPT and Claude (both measured)
    // report a pasted `\n\n` back as `\n`. Run the SAME attributable proofs,
    // mirrored ones included, under one canonical line-break form. The caret
    // range indexes the raw value, so it is not reused here.
    needle.contains('\n')
        && confirm_insertion_any_representation(
            &crate::desktop::collapse_line_break_runs(before_value),
            None,
            &crate::desktop::collapse_line_break_runs(after_value),
            &crate::desktop::collapse_line_break_runs(needle),
        )
}

pub(in crate::desktop::macos) fn confirm_insertion_any_representation(
    before_value: &str,
    range: Option<(usize, usize)>,
    after_value: &str,
    needle: &str,
) -> bool {
    if confirm_insertion_representation(before_value, range, after_value, needle) {
        return true;
    }
    // Claude/Electron mirrors one logical contenteditable value exactly twice
    // in AXValue. Normalize only that lossless representation shape, then run
    // the same exact selection/occurrence-delta proof. This is not fuzzy text
    // matching: unequal halves are never collapsed.
    let normalized_before = collapse_exact_mirror(before_value).unwrap_or(before_value);
    let normalized_after = collapse_exact_mirror(after_value).unwrap_or(after_value);
    let mirrored_representation =
        normalized_before != before_value || normalized_after != after_value;
    (mirrored_representation
        && confirm_insertion_representation(normalized_before, range, normalized_after, needle))
        // With existing rich-editor content, Electron mirrors each DOM text
        // node independently: `draftdraft` becomes
        // `draftdraft` + `transcripttranscript`, not `(draft+transcript)²`.
        // A +2 exact occurrence delta is accepted only after either snapshot
        // proves this mirrored AX representation.
        || (mirrored_representation
            && occurrences(after_value, needle) == occurrences(before_value, needle) + 2)
}

pub(in crate::desktop::macos) fn confirm_insertion_representation(
    before_value: &str,
    range: Option<(usize, usize)>,
    after_value: &str,
    needle: &str,
) -> bool {
    if after_value == before_value {
        return false;
    }
    if let Some((location, length)) = range {
        let units: Vec<u16> = before_value.encode_utf16().collect();
        let end = location.saturating_add(length);
        if end <= units.len() {
            let head = String::from_utf16_lossy(&units[..location]);
            let tail = String::from_utf16_lossy(&units[end..]);
            if after_value == format!("{head}{needle}{tail}") {
                return true;
            }
        }
    }
    // WebKit/Electron can expose a placeholder-like value plus a selection range
    // that does not describe its rich editor. A failed exact-range reconstruction
    // therefore falls through to the same attributable delta used when range is
    // absent; one new exact transcript occurrence is still proof of this paste,
    // while unchanged/pre-existing text remains rejected.
    occurrences(after_value, needle) == occurrences(before_value, needle) + 1
}

pub(in crate::desktop::macos) fn collapse_exact_mirror(value: &str) -> Option<&str> {
    let midpoint = value.len() / 2;
    (value.len().is_multiple_of(2) && value.is_char_boundary(midpoint))
        .then(|| value.split_at(midpoint))
        .filter(|(left, right)| !left.is_empty() && left == right)
        .map(|(left, _)| left)
}

/// Snapshot the focused field BEFORE an insertion or paste.
pub fn focused_field_proof_for_pid(pid: i32) -> FocusedFieldProof {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return FocusedFieldProof::default();
        }
        let Some(owner) = focused_element_for_pid(pid) else {
            return FocusedFieldProof::default();
        };
        focused_field_proof(owner, pid)
    }
}

/// True when the destination `proof` was taken from is still the live
/// destination: the same process, the same owning window, the retained editor
/// still focused, and a role that is positively known non-secure.
///
/// This is the send gate's destination check. It deliberately does NOT consult
/// the pre-paste focus-continuity window: the insertion itself mutates the
/// focused element (and therefore the observer sample that window is derived
/// from), so re-evaluating it after a proven paste rejects the destination we
/// just inserted into. It is not a replacement for the pre-paste guards, which
/// still run before any paste is dispatched.
pub fn proof_destination_is_current(proof: &FocusedFieldProof) -> bool {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return false;
        }
        let Some((pid, element)) = proof.owner.as_ref() else {
            return false;
        };
        // Same live process: pid recycled onto a different app is not the same
        // destination, so require the element's owner pid and the frontmost app.
        if proof.process_instance.is_none()
            || running_process_instance(*pid) != proof.process_instance
            || frontmost_pid() != Some(*pid)
        {
            return false;
        }
        let element_ref = element.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(element_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        if ax_element_pid(element_ref) != Some(*pid) {
            return false;
        }
        // The proven editor must still be the focused element for that process.
        // A frontmost app whose window is not key reports its focused control at
        // the app level, which is what capture reads too.
        let Some(current) = resolved_focused_editor(*pid) else {
            return false;
        };
        let current_ref = current.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(current_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        if current != *element {
            return false;
        }
        // Same window as the proof capture (a different window of the same app is
        // a different destination).
        let Some(expected_window) = proof.owner_window.as_ref() else {
            return false;
        };
        let Some(live_window) = ax_window_for_element(element_ref, *pid) else {
            return false;
        };
        if live_window != *expected_window {
            return false;
        }
        // Positive non-secure evidence only: unknown readability is not proof.
        secure_state_for_roles(
            ax_string(element_ref, "AXRole").as_deref(),
            ax_string(element_ref, "AXSubrole").as_deref(),
        ) == Some(false)
    }
}

/// Prove that `needle` was inserted at the focused selection BY US, relative
/// to `before`.
///
/// The old check asked only "does the field contain this text now?", which
/// confirms a write that never happened whenever the field already held the
/// same string — likeliest for short transcripts ("ok", "yes"). Because this
/// gates Enter in send-mode, a false confirmation submits text we never
/// inserted: the same "acted on the wrong content" class this whole path
/// exists to prevent, reached through a self-deceiving verification instead
/// of a stale clipboard.
///
/// Two tiers, strongest first:
///   1. Value AND selection range known — reconstruct the exact expected
///      result (`before[..loc] + needle + before[loc + len..]`) and require an
///      exact match. This is fully attributable and settles even the case
///      where the selection already equalled the transcript.
///   2. Value known, range not (common in Chromium/Electron) — require the
///      occurrence count of `needle` to rise by exactly one. Text that was
///      already present cannot satisfy this on its own.
///
/// Anything less readable stays UNPROVEN. Never guess in this direction.
pub fn insertion_confirmed_for_pid(pid: i32, before: &FocusedFieldProof, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() || before.is_blind() {
        return false;
    }
    let Some(before_value) = before.value.as_deref() else {
        return false;
    };
    let Some((owner_pid, _)) = before.owner.as_ref() else {
        return false;
    };
    if *owner_pid != pid {
        return false;
    }
    if unsafe { AXIsProcessTrusted() } == 0 {
        return false;
    }
    // Read the SAME representation the proof was captured through.
    if before
        .read_current()
        .is_some_and(|after| confirm_insertion(before_value, before.range, &after, needle))
    {
        return true;
    }
    // Only compare the marker representation with its own pre-paste baseline.
    before.marker_value.as_deref().is_some_and(|marker_before| {
        let Some((_, owner)) = before.owner.as_ref() else {
            return false;
        };
        unsafe {
            read_field_value_with_source(
                owner.as_CFTypeRef() as AXUIElementRef,
                FieldTextSource::TextMarker,
            )
            .is_some_and(|after| confirm_insertion(marker_before, None, &after, needle))
        }
    })
}

/// Capture proof from exact editor retained for one recording.
pub fn focused_field_proof_for_pin(pin: &FocusPin) -> FocusedFieldProof {
    if !focus_pin_running_identity_matches(pin) {
        return FocusedFieldProof::default();
    }
    // The pin already holds the exact stop-time editor; the shared builder
    // only adds the sourced representation and the retained window.
    focused_field_proof(pin.element.clone(), pin.pid())
}

pub fn focused_field_content_fingerprint_for_pin(pin: &FocusPin) -> Option<u64> {
    focused_field_proof_for_pin(pin).content_fingerprint()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertionConfirmation {
    RetainedElement,
    FreshEditorResolution,
    Unconfirmed,
}

impl InsertionConfirmation {
    pub fn confirmed(self) -> bool {
        self != Self::Unconfirmed
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::RetainedElement => "retained_element",
            Self::FreshEditorResolution => "fresh_editor_resolution",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

pub(in crate::desktop::macos) fn insertion_confirmed_for_fresh_editor_resolution(
    pin: &FocusPin,
    before: &FocusedFieldProof,
    needle: &str,
) -> bool {
    unsafe {
        if !focus_pin_running_identity_matches(pin) || frontmost_pid() != Some(pin.pid) {
            return false;
        }
        let Some(current) = resolved_focused_editor(pin.pid) else {
            return false;
        };
        // Resolve editor again because Electron/WebKit may either replace its
        // AX node or leave retained node's cached value stale after paste.
        let current_ref = current.as_CFTypeRef() as AXUIElementRef;
        if ax_element_pid(current_ref) != Some(pin.pid)
            || secure_state_for_roles(
                ax_string(current_ref, "AXRole").as_deref(),
                ax_string(current_ref, "AXSubrole").as_deref(),
            ) != Some(false)
            || !focus_pin_window_matches(current_ref, pin.pid, &pin.window)
        {
            return false;
        }
        let Some(expected_fingerprint) = before.fingerprint.as_ref() else {
            return false;
        };
        let Some(current_fingerprint) = editor_fingerprint(current_ref) else {
            return false;
        };
        if !expected_fingerprint.logically_matches(&current_fingerprint) {
            return false;
        }
        if let (Some(before_value), Some(source)) = (before.value.as_deref(), before.source) {
            if read_field_value_with_source(current_ref, source)
                .is_some_and(|after| confirm_insertion(before_value, before.range, &after, needle))
            {
                return true;
            }
        }
        before.marker_value.as_deref().is_some_and(|marker_before| {
            read_field_value_with_source(current_ref, FieldTextSource::TextMarker)
                .is_some_and(|after| confirm_insertion(marker_before, None, &after, needle))
        })
    }
}

pub fn insertion_confirmation_for_pin(
    pin: &FocusPin,
    before: &FocusedFieldProof,
    needle: &str,
) -> InsertionConfirmation {
    if insertion_confirmed_for_pin_retained(pin, before, needle) {
        InsertionConfirmation::RetainedElement
    } else if insertion_confirmed_for_fresh_editor_resolution(pin, before, needle.trim()) {
        InsertionConfirmation::FreshEditorResolution
    } else {
        InsertionConfirmation::Unconfirmed
    }
}

/// Confirm mutation against exact retained editor, never current PID focus.
pub fn insertion_confirmed_for_pin(
    pin: &FocusPin,
    before: &FocusedFieldProof,
    needle: &str,
) -> bool {
    insertion_confirmation_for_pin(pin, before, needle).confirmed()
}

pub(in crate::desktop::macos) fn insertion_confirmed_for_pin_retained(
    pin: &FocusPin,
    before: &FocusedFieldProof,
    needle: &str,
) -> bool {
    let needle = needle.trim();
    if needle.is_empty() || before.is_blind() {
        return false;
    }
    let Some(before_value) = before.value.as_deref() else {
        return false;
    };
    if before.owner.as_ref().map(|(pid, _)| *pid) != Some(pin.pid()) {
        return false;
    }
    if !focus_pin_running_identity_matches(pin) {
        return false;
    }
    // Read the SAME representation the proof was captured through.
    if before
        .read_current()
        .is_some_and(|after| confirm_insertion(before_value, before.range, &after, needle))
    {
        return true;
    }
    // Only compare the marker representation with its own pre-paste baseline.
    before.marker_value.as_deref().is_some_and(|marker_before| {
        let Some((_, owner)) = before.owner.as_ref() else {
            return false;
        };
        unsafe {
            read_field_value_with_source(
                owner.as_CFTypeRef() as AXUIElementRef,
                FieldTextSource::TextMarker,
            )
            .is_some_and(|after| confirm_insertion(marker_before, None, &after, needle))
        }
    })
}

#[cfg(test)]
#[path = "proof_tests.rs"]
mod tests;
