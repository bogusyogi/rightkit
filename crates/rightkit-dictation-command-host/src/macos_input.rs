//! macOS low-level input + window helpers.
//!
//! Shared by `delivery` (text insertion) and `command_dispatch` (key chords /
//! mouse). Keyboard + mouse posting go through CoreGraphics (`CGEvent`);
//! clipboard, frontmost-app identity, and app activation go through AppKit
//! (`NSPasteboard` / `NSWorkspace` / `NSRunningApplication`).
//!
//! IMPORTANT: posting `CGEvent`s requires the macOS **Accessibility** permission.
//! Without it the OS silently drops the events (no error, no insertion). The
//! permission onboarding is covered by the current architecture docs.
#![cfg(target_os = "macos")]

use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

pub use rightkit_automation::desktop::macos::{
    accessibility_trusted, activate_pid, copy_combo, focused_target_identity_for_pid, frontmost,
    frontmost_fast, frontmost_pid, frontmost_window_identity, get_clipboard, paste_combo, post_key,
    press_return, promote_thread_qos, set_clipboard, type_unicode, KEY_C, KEY_DELETE, KEY_RETURN,
    KEY_V,
};

fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

// Left-side modifier keycodes — used to synthesize REAL modifier key events.
const KEY_CONTROL: CGKeyCode = 0x3B;
const KEY_OPTION: CGKeyCode = 0x3A;
const KEY_SHIFT: CGKeyCode = 0x38;
const KEY_COMMAND: CGKeyCode = 0x37;

/// Post a chord by pressing each modifier as a REAL key event, holding the main
/// key down+up, then releasing the modifiers in reverse. `post_key` only *flags*
/// the main key, which suffices for shortcuts handled on keyDown (⌘C, ⌘⇧3) — but
/// the ⌘Tab app switcher and ⌘` window cycler only COMMIT on the Command key's
/// real keyUp, so a flag-only Tab shows the switcher and never switches. The brief
/// hold lets the switcher register before the commit.
pub fn post_chord(keycode: CGKeyCode, flags: CGEventFlags) -> bool {
    let Some(src) = source() else {
        return false;
    };
    let active: Vec<(CGEventFlags, CGKeyCode)> = [
        (CGEventFlags::CGEventFlagControl, KEY_CONTROL),
        (CGEventFlags::CGEventFlagAlternate, KEY_OPTION),
        (CGEventFlags::CGEventFlagShift, KEY_SHIFT),
        (CGEventFlags::CGEventFlagCommand, KEY_COMMAND),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .collect();
    if active.is_empty() {
        return post_key(keycode, flags); // no modifiers → plain key
    }

    let post = |kc: CGKeyCode, down: bool, f: CGEventFlags| -> bool {
        match CGEvent::new_keyboard_event(src.clone(), kc, down) {
            Ok(ev) => {
                ev.set_flags(f);
                ev.post(CGEventTapLocation::HID);
                true
            }
            Err(()) => false,
        }
    };

    // Every modifier that went down must come up, even when a later post
    // fails; otherwise ⌘/⌥/⇧/⌃ stays held system-wide.
    let mut acc = CGEventFlags::empty();
    let mut pressed = 0;
    let mut ok = true;
    for (flag, kc) in &active {
        if !post(*kc, true, acc | *flag) {
            ok = false;
            break;
        }
        acc |= *flag;
        pressed += 1;
    }
    if ok {
        ok = post(keycode, true, acc) && post(keycode, false, acc);
        std::thread::sleep(std::time::Duration::from_millis(40)); // let ⌘Tab register
    }
    for (flag, kc) in active[..pressed].iter().rev() {
        acc &= !*flag;
        post(*kc, false, acc);
    }
    ok
}

/// Re-export so callers (command_dispatch) can name mouse buttons without a
/// direct core-graphics dependency.
pub use core_graphics::event::CGMouseButton as MouseButton;

/// Current mouse cursor position in global display coordinates.
pub fn cursor_position() -> CGPoint {
    source()
        .and_then(|s| CGEvent::new(s).ok())
        .map(|e| e.location())
        .unwrap_or(CGPoint::new(0.0, 0.0))
}

/// Click `button` `clicks` times at the current cursor position.
pub fn mouse_click(button: CGMouseButton, clicks: u32) -> bool {
    let Some(src) = source() else {
        return false;
    };
    let pos = cursor_position();
    let (down, up) = match button {
        CGMouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
        CGMouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp),
        CGMouseButton::Center => (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
    };
    for _ in 0..clicks.max(1) {
        for ty in [down, up] {
            match CGEvent::new_mouse_event(src.clone(), ty, pos, button) {
                Ok(ev) => ev.post(CGEventTapLocation::HID),
                Err(()) => return false,
            }
        }
    }
    true
}

/// Scroll by line units. `vertical` positive scrolls up, negative down;
/// `horizontal` positive scrolls right, negative left.
pub fn scroll(vertical: i32, horizontal: i32) -> bool {
    let Some(src) = source() else {
        return false;
    };
    const LINE_UNIT: u32 = 1; // CGScrollEventUnit::LINE
    match CGEvent::new_scroll_event(src, LINE_UNIT, 2, vertical, horizontal, 0) {
        Ok(ev) => {
            ev.post(CGEventTapLocation::HID);
            true
        }
        Err(()) => false,
    }
}
