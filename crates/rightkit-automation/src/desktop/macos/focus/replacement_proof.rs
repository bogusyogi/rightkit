//! Read-only custody of an exact prior insertion in one retained native editor.
//! This deliberately does not reuse delivery's occurrence/mirror/marker proofs:
//! evidence sufficient to report delivery must not authorize replacing old text.

use core_foundation::base::{CFRange, CFType, TCFType};
use core_foundation::string::{CFString, CFStringGetCharacters};

use super::super::ffi::{
    ax_copy, ax_element_pid, ax_selected_range, ax_string, AXIsProcessTrusted, AXUIElementRef,
    AXUIElementSetMessagingTimeout, AX_MESSAGING_TIMEOUT_SECONDS, MAX_PROOF_UTF16_UNITS,
    MAX_PROOF_UTF8_BYTES,
};
use super::super::frontmost::frontmost_pid;
use super::super::observation::same_running_application;
use super::context::{actual_focused_element_for_pid, secure_state_for_roles};
use super::pin::{running_process_instance, FocusPin};

// No Clone, Debug, serialization, public AX handles or public text. A caller
// cannot manufacture a token from a PID, hash, transcript or delivery receipt.
struct RetainedEditor {
    pid: i32,
    process_instance: u64,
    bundle_identifier: String,
    bundle_path: String,
    element: CFType,
    window: CFType,
}

/// Single-use baseline captured immediately before a host's insertion attempt.
/// An empty field is valid; a missing selection or incomplete read is not.
pub struct ExactInsertionBaseline {
    editor: RetainedEditor,
    value: String,
    selection_utf16: (usize, usize),
}

/// Opaque, non-Clone custody of one confirmed insertion & its complete field.
/// This is read-only evidence, never permission to write after a later await.
pub struct ExactPriorInsertion {
    editor: RetainedEditor,
    value: String,
    inserted_utf16: (usize, usize),
}

impl RetainedEditor {
    fn is_current(&self) -> bool {
        unsafe {
            if AXIsProcessTrusted() == 0
                || frontmost_pid() != Some(self.pid)
                || running_process_instance(self.pid) != Some(self.process_instance)
                || !same_running_application(self.pid, &self.bundle_identifier, &self.bundle_path)
            {
                return false;
            }
            let element = self.element.as_CFTypeRef() as AXUIElementRef;
            if AXUIElementSetMessagingTimeout(element, AX_MESSAGING_TIMEOUT_SECONDS) != 0
                || ax_element_pid(element) != Some(self.pid)
                || ax_element_pid(self.window.as_CFTypeRef() as AXUIElementRef) != Some(self.pid)
            {
                return false;
            }
            let role = ax_string(element, "AXRole");
            let subrole = ax_string(element, "AXSubrole");
            if !matches!(
                role.as_deref(),
                Some("AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField")
            ) || secure_state_for_roles(role.as_deref(), subrole.as_deref()) != Some(false)
            {
                return false;
            }
            // No descendant scoring or logical-fingerprint match: focus itself
            // must identify retained editor. A container is ambiguous evidence.
            let Some(current) = actual_focused_element_for_pid(self.pid) else {
                return false;
            };
            if current != self.element {
                return false;
            }
            // No focused-window fallback: AXWindow must name this exact window.
            let Some(window) = ax_copy(element, "AXWindow") else {
                return false;
            };
            if CFType::wrap_under_create_rule(window) != self.window {
                return false;
            }
            // A text role alone also occurs in web editors. Require a complete,
            // bounded ancestor chain reaching this window, with no web area.
            // Missing parents, cycles & exhausted bounds all fail closed.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(350);
            let mut node = self.element.clone();
            for _ in 0..32 {
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                if node == self.window {
                    return true;
                }
                let node_ref = node.as_CFTypeRef() as AXUIElementRef;
                if AXUIElementSetMessagingTimeout(node_ref, AX_MESSAGING_TIMEOUT_SECONDS) != 0 {
                    return false;
                }
                let Some(role) = ax_string(node_ref, "AXRole") else {
                    return false;
                };
                let subrole = ax_string(node_ref, "AXSubrole");
                if matches!(role.as_str(), "AXWebArea" | "AXSecureTextField")
                    || subrole.as_deref() == Some("AXSecureTextField")
                {
                    return false;
                }
                let Some(parent) = ax_copy(node_ref, "AXParent") else {
                    return false;
                };
                node = CFType::wrap_under_create_rule(parent);
            }
            false
        }
    }

    fn read_current(&self) -> Option<String> {
        if !self.is_current() {
            return None;
        }
        let value = unsafe { exact_ax_value(self.element.as_CFTypeRef() as AXUIElementRef) }?;
        self.is_current().then_some(value)
    }
}

/// Capture immediately before native insertion or paste; never from a delivery
/// receipt alone. Only complete plain AXValue on a retained native editor is
/// supported. Web, marker-only, oversized & unidentified editors return None.
pub fn exact_insertion_baseline_for_pin(pin: &FocusPin) -> Option<ExactInsertionBaseline> {
    let editor = RetainedEditor {
        pid: pin.pid,
        process_instance: pin.process_instance,
        bundle_identifier: pin.bundle_identifier.clone(),
        bundle_path: pin.bundle_path.clone(),
        element: pin.element.clone(),
        window: pin.window.clone(),
    };
    if !editor.is_current() {
        return None;
    }
    let element = editor.element.as_CFTypeRef() as AXUIElementRef;
    let selection_utf16 = unsafe { ax_selected_range(element) }?;
    let value = editor.read_current()?;
    utf16_byte_range(&value, selection_utf16)?;
    if unsafe { ax_selected_range(element) } != Some(selection_utf16) || !editor.is_current() {
        return None;
    }
    Some(ExactInsertionBaseline {
        editor,
        value,
        selection_utf16,
    })
}

/// Consume a pre-write baseline immediately after an insertion attempt. Require
/// exact full-field replacement of its captured UTF-16 selection by `inserted`.
/// No trimming, line folding, mirror collapse, occurrence delta or node rebinding.
pub fn confirm_exact_prior_insertion(
    before: ExactInsertionBaseline,
    inserted: &str,
) -> Option<ExactPriorInsertion> {
    if inserted.is_empty() || inserted.len() > MAX_PROOF_UTF8_BYTES {
        return None;
    }
    let (head_end, tail_start) = utf16_byte_range(&before.value, before.selection_utf16)?;
    let after = before.editor.read_current()?;
    let head = &before.value[..head_end];
    let tail = &before.value[tail_start..];
    let inserted_end = head.len().checked_add(inserted.len())?;
    let expected_len = inserted_end.checked_add(tail.len())?;
    if after == before.value
        || after.len() != expected_len
        || after.get(..head.len()) != Some(head)
        || after.get(head.len()..inserted_end) != Some(inserted)
        || after.get(inserted_end..) != Some(tail)
    {
        return None;
    }
    Some(ExactPriorInsertion {
        editor: before.editor,
        value: after,
        inserted_utf16: (before.selection_utf16.0, inserted.encode_utf16().count()),
    })
}

impl ExactPriorInsertion {
    /// Freshly validate retained process/window/editor & entire unchanged field,
    /// then compare exact insertion range with caller's prior text. Caret moves
    /// do not rebind custody; unrelated text edits invalidate it. Read-only result
    /// cannot bridge a focus change or authorize a subsequent unchecked mutation.
    pub fn is_current(&self, inserted: &str) -> bool {
        if inserted.is_empty() {
            return false;
        }
        let Some((start, end)) = utf16_byte_range(&self.value, self.inserted_utf16) else {
            return false;
        };
        if self.value.get(start..end) != Some(inserted) {
            return false;
        }
        self.editor.read_current().as_deref() == Some(self.value.as_str())
    }
}

// Preserve raw CFString UTF-16 exactly & reject malformed surrogates. String's
// ordinary CF conversion is not used for text custody. Reject over-budget reads
// before allocation/conversion; never truncate or switch representations.
unsafe fn exact_ax_value(element: AXUIElementRef) -> Option<String> {
    let raw = ax_copy(element, "AXValue")?;
    let value = CFType::wrap_under_create_rule(raw).downcast_into::<CFString>()?;
    let length = value.char_len();
    if !(0..=MAX_PROOF_UTF16_UNITS).contains(&length) {
        return None;
    }
    let mut units = vec![0u16; length as usize];
    CFStringGetCharacters(
        value.as_concrete_TypeRef(),
        CFRange {
            location: 0,
            length,
        },
        units.as_mut_ptr(),
    );
    let text = String::from_utf16(&units).ok()?;
    (text.len() <= MAX_PROOF_UTF8_BYTES).then_some(text)
}

// AX uses UTF-16 indices. Only Unicode scalar boundaries map to Rust slices;
// selection endpoints inside a surrogate pair are rejected, never repaired.
fn utf16_byte_range(value: &str, range: (usize, usize)) -> Option<(usize, usize)> {
    let end = range.0.checked_add(range.1)?;
    let mut utf16 = 0usize;
    let mut start_byte = None;
    for (byte, ch) in value
        .char_indices()
        .chain(std::iter::once((value.len(), '\0')))
    {
        if utf16 == range.0 {
            start_byte = Some(byte);
        }
        if utf16 == end {
            return Some((start_byte?, byte));
        }
        if utf16 > end {
            return None;
        }
        utf16 = utf16.checked_add(ch.len_utf16())?;
    }
    None
}
