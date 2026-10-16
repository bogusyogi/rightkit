//! macOS evidence for the capture-safety probe (see `crate::capture_safety`): secure event
//! input, the focused AX element, and each window's sharing state. Public ApplicationServices,
//! Carbon (HIToolbox) and CoreGraphics calls only; nothing is sent to the target.

use super::ax::{self, El};
use super::ffi::*;
use crate::capture_safety::{
    ax_focus_outcome, sharing_evidence, GapKind, SafetyCheck, SafetyEvidence, SafetyOutcome,
    SafetyReason, WindowRow,
};
use crate::model::{Error, Target};

/// `kAXErrorNoValue`: the attribute exists and currently has no value (nothing is focused).
const AX_ERROR_NO_VALUE: i32 = -25212;

pub fn collect(target: &Target) -> Vec<SafetyEvidence> {
    let mut out = vec![secure_event_input(), focused_secure_field(target.pid)];
    out.extend(window_sharing(target));
    out
}

/// `IsSecureEventInputEnabled` is system-wide: it does not say which process holds secure
/// input, so a true value is reported against every target (conservative).
fn secure_event_input() -> SafetyEvidence {
    let on = unsafe { IsSecureEventInputEnabled() } != 0;
    let outcome = if on {
        SafetyOutcome::Found(SafetyReason::SecureEventInput)
    } else {
        SafetyOutcome::Clear
    };
    SafetyEvidence::new(SafetyCheck::SecureEventInput, outcome)
}

fn focused_secure_field(pid: i32) -> SafetyEvidence {
    SafetyEvidence::new(SafetyCheck::FocusedSecureField, focus_outcome(pid))
}

fn focus_outcome(pid: i32) -> SafetyOutcome {
    let app = match El::app(pid) {
        Ok(app) => app,
        Err(Error::PermissionDenied(what)) => {
            return SafetyOutcome::Unavailable(GapKind::PermissionMissing(what))
        }
        Err(_) => {
            return SafetyOutcome::Unavailable(GapKind::ReadFailed("AXUIElementCreateApplication"))
        }
    };
    let key = Cf::string("AXFocusedUIElement");
    let mut raw: CFTypeRef = std::ptr::null();
    let code = unsafe { AXUIElementCopyAttributeValue((app.0).0, key.0, &mut raw) };
    if code == AX_ERROR_NO_VALUE {
        // The app answered and has no focused element, so no focused secure field.
        return SafetyOutcome::Clear;
    }
    if code != 0 {
        return SafetyOutcome::Unavailable(GapKind::ReadFailed("AXFocusedUIElement"));
    }
    // SAFETY: a +1 reference from a successful AX Copy call, owned by the returned `El`.
    let Some(first) = (unsafe { El::from_raw(raw) }) else {
        return SafetyOutcome::Unavailable(GapKind::ReadFailed("AXFocusedUIElement"));
    };
    // Descend from a container to the deepest focused element; keep the first on failure.
    let el = ax::focused_editor(&app).unwrap_or(first);
    ax_focus_outcome(
        el.str_attr("AXRole").as_deref(),
        el.str_attr("AXSubrole").as_deref(),
    )
}

fn window_sharing(target: &Target) -> Vec<SafetyEvidence> {
    // 16 = kCGWindowListExcludeDesktopElements, without OnScreenOnly: every window.
    let Some(arr) = Cf::new(unsafe { CGWindowListCopyWindowInfo(16, 0) }) else {
        return vec![SafetyEvidence::new(
            SafetyCheck::WindowCapturable,
            SafetyOutcome::Unavailable(GapKind::ReadFailed("CGWindowListCopyWindowInfo")),
        )];
    };
    let mut rows = Vec::new();
    unsafe {
        for i in 0..CFArrayGetCount(arr.0) {
            let d = CFArrayGetValueAtIndex(arr.0, i);
            let (Some(id), Some(pid)) = (
                cf_to_i64(dict_get(d, "kCGWindowNumber")),
                cf_to_i64(dict_get(d, "kCGWindowOwnerPID")),
            ) else {
                continue;
            };
            rows.push(WindowRow {
                window_id: id as u64,
                pid: pid as i32,
                on_screen: cf_to_bool(dict_get(d, "kCGWindowIsOnscreen")).unwrap_or(false),
                sharing_state: cf_to_i64(dict_get(d, "kCGWindowSharingState")),
            });
        }
    }
    sharing_evidence(&rows, target)
}
