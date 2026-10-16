//! Middle-click gesture tap (HeardRight donor).
//!
//! `kCGEventOtherMouseDown` covers every non-left, non-right button (HID buttons 2-31:
//! back/forward, DPI, EasySwitch). Only the wheel button (2) may toggle; every other button passes
//! through before any accessibility scan or callback. A middle click over a link also passes
//! through so browsers still open it in a new tab.
//!
//! The tap callback never runs the accessibility lookup: AX calls can block, and macOS disables a
//! tap whose callback is slow. A middle press is swallowed at once, one worker thread runs the
//! lookup, and for a link the click is replayed as a marked synthetic press and release (see
//! [`crate::middle_click`] for the state machine). If the OS disables the tap anyway, it is
//! re-enabled and any click in flight is dropped.

use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use super::ffi::*;
use super::synth::post_other_mouse_marked;
use super::tap::{EventTap, TapConfig, TapDecision, TapError, TapKind, TapLocation};
use crate::middle_click::{
    handle_other_mouse, run_job, Job, JobActions, MiddleClickGate, MouseEdge, Verdict,
};

pub use crate::middle_click::DIAG_OTHER_BUTTON;

/// `kCGEventSourceUserData` stamped on the synthetic press and release that replay a click over a
/// link. The tap passes events carrying it untouched.
pub const REPLAY_USER_DATA: i64 = 0x5249_4748_544B_4D43;

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

pub struct MiddleMouseTap {
    enabled: Arc<AtomicBool>,
    gate: Arc<MiddleClickGate>,
    tap: EventTap,
}

impl MiddleMouseTap {
    /// `is_link` decides whether a click at a point should pass through (use [`ax_link_at`] in
    /// production). It runs on a worker thread, never inside the tap callback, so it may block;
    /// while it runs, further middle presses pass through. `on_toggle` runs on a fresh thread.
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
        let gate = Arc::new(MiddleClickGate::new());
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();

        let worker_gate = gate.clone();
        let mut actions = JobActions {
            is_link: Box::new(is_link),
            post: Box::new(move |down, x, y| {
                post_other_mouse_marked(location, 2, x, y, down, REPLAY_USER_DATA);
            }),
            toggle: Arc::new(on_toggle),
        };
        // The worker ends when the tap (the only sender) is dropped.
        std::thread::Builder::new()
            .name("rightkit-middle-mouse-lookup".into())
            .spawn(move || {
                for job in jobs_rx {
                    run_job(&worker_gate, job, &mut actions);
                }
            })
            .map_err(|e| TapError::Thread(e.to_string()))?;

        let handler_enabled = enabled.clone();
        let handler_gate = gate.clone();
        let recovery_gate = gate.clone();
        let tap = EventTap::spawn_with_recovery(
            TapConfig {
                name: "rightkit-middle-mouse-tap".into(),
                location,
                listen_only: false,
                kinds: vec![TapKind::OtherMouseDown, TapKind::OtherMouseUp],
            },
            move |event| {
                if event.user_data == REPLAY_USER_DATA {
                    return TapDecision::Pass;
                }
                let edge = match event.kind {
                    TapKind::OtherMouseDown => MouseEdge::Down,
                    TapKind::OtherMouseUp => MouseEdge::Up,
                    _ => return TapDecision::Pass,
                };
                let verdict = handle_other_mouse(
                    &handler_enabled,
                    &handler_gate,
                    edge,
                    event.button,
                    event.location,
                    Instant::now(),
                    &|job| jobs_tx.send(job).is_ok(),
                    &diag,
                );
                match verdict {
                    Verdict::Pass => TapDecision::Pass,
                    Verdict::Swallow => TapDecision::Swallow,
                }
            },
            move || recovery_gate.reset(),
        )?;
        Ok(Self { enabled, gate, tap })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }

    /// How many times the OS disabled the tap and it was re-enabled (any click in flight is
    /// dropped each time).
    pub fn reenable_count(&self) -> u64 {
        self.tap.reenable_count()
    }

    /// `true` when no middle click is waiting on a lookup.
    pub fn is_idle(&self) -> bool {
        self.gate.is_idle()
    }
}
