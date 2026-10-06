//! Synthetic input for tests and self-diagnostics.
//!
//! Events are created from a *private* event source, so they carry exactly the flags set on them
//! and never merge with, or leave behind, the user's real modifier state. That is what lets an
//! end-to-end run exercise registration and taps without stealing a physical shortcut.

use super::ffi::*;
use super::tap::TapLocation;

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn can_listen() -> bool {
    unsafe { CGPreflightListenEventAccess() }
}

pub fn can_post() -> bool {
    unsafe { CGPreflightPostEventAccess() }
}

fn tap_location(location: TapLocation) -> u32 {
    match location {
        TapLocation::Hid => K_CG_HID_EVENT_TAP,
        TapLocation::Session => K_CG_SESSION_EVENT_TAP,
        TapLocation::AnnotatedSession => K_CG_ANNOTATED_SESSION_EVENT_TAP,
    }
}

/// Post one key event with explicit `flags` (see the `FLAG_*` constants).
pub fn post_key(location: TapLocation, keycode: u16, down: bool, flags: u64) -> bool {
    unsafe {
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_PRIVATE);
        if source.is_null() {
            return false;
        }
        let event = CGEventCreateKeyboardEvent(source, keycode, down);
        if event.is_null() {
            CFRelease(source);
            return false;
        }
        CGEventSetFlags(event, flags);
        CGEventPost(tap_location(location), event);
        CFRelease(event);
        CFRelease(source);
    }
    true
}

/// Post a modifier transition (`FlagsChanged`) for `keycode` with the resulting `flags`.
pub fn post_flags_changed(location: TapLocation, keycode: u16, flags: u64) -> bool {
    // A keyboard event for a modifier keycode is delivered as a flags-changed event.
    post_key(location, keycode, flags != 0, flags)
}

/// Post an "other" mouse button press (button 2 = middle) at `(x, y)`.
pub fn post_other_mouse(location: TapLocation, button: u32, x: f64, y: f64, down: bool) -> bool {
    unsafe {
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_PRIVATE);
        if source.is_null() {
            return false;
        }
        let event = CGEventCreateMouseEvent(
            source,
            if down { K_CG_EVENT_OTHER_MOUSE_DOWN_T } else { K_CG_EVENT_OTHER_MOUSE_UP_T },
            CGPoint { x, y },
            button,
        );
        if event.is_null() {
            CFRelease(source);
            return false;
        }
        CGEventPost(tap_location(location), event);
        CFRelease(event);
        CFRelease(source);
    }
    true
}

/// Pump the calling thread's run loop for `seconds` (Carbon hotkey events are delivered through
/// the main run loop).
pub fn pump_run_loop(seconds: f64) {
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, false);
    }
}
