//! Middle-click gesture tap (HeardRight donor).
//!
//! `kCGEventOtherMouseDown` covers every non-left, non-right button (HID buttons 2-31:
//! back/forward, DPI, EasySwitch). Only the wheel button (2) may toggle; every other button passes
//! through before any accessibility scan or callback. A middle click over a link also passes
//! through so browsers still open it in a new tab.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::ffi::*;
use super::tap::{EventTap, TapConfig, TapDecision, TapError, TapEvent, TapKind, TapLocation};
use crate::decision::other_mouse_down_is_middle;

const UTF8: u32 = 0x0800_0100;

unsafe fn cf_str(s: &'static [u8]) -> *const std::ffi::c_void {
    CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), UTF8)
}

unsafe fn role_is_link(element: *const std::ffi::c_void) -> bool {
    if element.is_null() {
        return false;
    }
    let attr = cf_str(b"AXRole\0");
    let mut value: *const std::ffi::c_void = std::ptr::null();
    let status = AXUIElementCopyAttributeValue(element, attr, &mut value);
    CFRelease(attr);
    if status != 0 || value.is_null() {
        return false;
    }
    let mut buffer = [0u8; 64];
    let ok = CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as isize, UTF8);
    CFRelease(value);
    ok && buffer.starts_with(b"AXLink\0")
}

/// Best-effort: is the element at `(x, y)` (or one of its two nearest ancestors) a link? Text inside
/// `<a>` resolves to a text run, so the parents are checked. Any error answers `false`.
pub fn ax_link_at(x: f64, y: f64) -> bool {
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return false;
        }
        let mut element: *const std::ffi::c_void = std::ptr::null();
        let status = AXUIElementCopyElementAtPosition(system, x as f32, y as f32, &mut element);
        CFRelease(system);
        if status != 0 || element.is_null() {
            return false;
        }
        if role_is_link(element) {
            CFRelease(element);
            return true;
        }
        let parent_attr = cf_str(b"AXParent\0");
        let mut ancestor = element;
        for _ in 0..2 {
            let mut next: *const std::ffi::c_void = std::ptr::null();
            let status = AXUIElementCopyAttributeValue(ancestor, parent_attr, &mut next);
            CFRelease(ancestor);
            if status != 0 || next.is_null() {
                CFRelease(parent_attr);
                return false;
            }
            ancestor = next;
            if role_is_link(ancestor) {
                CFRelease(ancestor);
                CFRelease(parent_attr);
                return true;
            }
        }
        CFRelease(ancestor);
        CFRelease(parent_attr);
        false
    }
}

/// Telemetry name for a non-middle `OtherMouseDown` that the tap passes through.
pub const DIAG_OTHER_BUTTON: &str = "middle_mouse_other_button";

/// Decision for one `OtherMouseDown`. Only the wheel button toggles; every other button is passed
/// through and reported on the diagnostics hook as `button=<n>`.
fn decide_other_mouse_down(
    gate: &AtomicBool,
    event: &TapEvent,
    on_toggle: &Arc<dyn Fn() + Send + Sync + 'static>,
    is_link: &dyn Fn(f64, f64) -> bool,
    diag: &dyn Fn(&str, &str),
) -> TapDecision {
    if event.kind != TapKind::OtherMouseDown || !gate.load(Ordering::SeqCst) {
        return TapDecision::Pass;
    }
    if !other_mouse_down_is_middle(event.button) {
        diag(DIAG_OTHER_BUTTON, &format!("button={}", event.button));
        return TapDecision::Pass;
    }
    if is_link(event.location.0, event.location.1) {
        return TapDecision::Pass;
    }
    let cb = on_toggle.clone();
    std::thread::spawn(move || cb());
    TapDecision::Swallow
}

pub struct MiddleMouseTap {
    enabled: Arc<AtomicBool>,
    _tap: EventTap,
}

impl MiddleMouseTap {
    /// `is_link` decides whether a click at a point should pass through (use [`ax_link_at`] in
    /// production). `on_toggle` runs on a fresh thread so the tap callback stays fast.
    pub fn spawn(
        location: TapLocation,
        on_toggle: impl Fn() + Send + Sync + 'static,
        is_link: impl Fn(f64, f64) -> bool + Send + 'static,
    ) -> Result<Self, TapError> {
        Self::spawn_with_diag(location, on_toggle, is_link, |_, _| {})
    }

    /// Like [`MiddleMouseTap::spawn`], and reports each passed-through non-middle button through
    /// `diag` as `middle_mouse_other_button` with `button=<n>`. `diag` runs inside the tap callback,
    /// so it must return quickly.
    pub fn spawn_with_diag(
        location: TapLocation,
        on_toggle: impl Fn() + Send + Sync + 'static,
        is_link: impl Fn(f64, f64) -> bool + Send + 'static,
        diag: impl Fn(&str, &str) + Send + 'static,
    ) -> Result<Self, TapError> {
        let enabled = Arc::new(AtomicBool::new(true));
        let gate = enabled.clone();
        let on_toggle: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(on_toggle);
        let tap = EventTap::spawn(
            TapConfig {
                name: "rightkit-middle-mouse-tap".into(),
                location,
                listen_only: false,
                kinds: vec![TapKind::OtherMouseDown],
            },
            move |event| decide_other_mouse_down(&gate, event, &on_toggle, &is_link, &diag),
        )?;
        Ok(Self { enabled, _tap: tap })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn other_button(button: i64) -> TapEvent {
        TapEvent {
            kind: TapKind::OtherMouseDown,
            keycode: 0,
            flags: 0,
            button,
            location: (10.0, 20.0),
            user_data: 0,
        }
    }

    fn no_op_toggle() -> Arc<dyn Fn() + Send + Sync + 'static> {
        Arc::new(|| {})
    }

    #[test]
    fn non_middle_button_passes_and_reports_its_number() {
        let gate = AtomicBool::new(true);
        let lines: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
        let diag = |kind: &str, detail: &str| {
            lines
                .lock()
                .unwrap()
                .push((kind.to_string(), detail.to_string()));
        };
        let decision = decide_other_mouse_down(
            &gate,
            &other_button(3),
            &no_op_toggle(),
            &|_: f64, _: f64| false,
            &diag,
        );
        assert_eq!(decision, TapDecision::Pass);

        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 1, "exactly one diagnostic per passed button");
        assert_eq!(lines[0].0, "middle_mouse_other_button");
        assert_eq!(lines[0].1, "button=3");
    }

    #[test]
    fn disabled_tap_passes_without_diagnostics() {
        let gate = AtomicBool::new(false);
        let calls = Mutex::new(0usize);
        let diag = |_: &str, _: &str| *calls.lock().unwrap() += 1;
        let decision = decide_other_mouse_down(
            &gate,
            &other_button(3),
            &no_op_toggle(),
            &|_: f64, _: f64| false,
            &diag,
        );
        assert_eq!(decision, TapDecision::Pass);
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[test]
    fn middle_click_toggles_without_diagnostics() {
        let gate = AtomicBool::new(true);
        let calls = Mutex::new(0usize);
        let diag = |_: &str, _: &str| *calls.lock().unwrap() += 1;
        let decision = decide_other_mouse_down(
            &gate,
            &other_button(2),
            &no_op_toggle(),
            &|_: f64, _: f64| false,
            &diag,
        );
        assert_eq!(decision, TapDecision::Swallow);
        assert_eq!(*calls.lock().unwrap(), 0);
    }
}
