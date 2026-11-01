//! Synthetic keyboard input: CGEvent key posting, copy/paste chords, and
//! Unicode text injection.

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

pub const KEY_C: CGKeyCode = 0x08;

pub const KEY_V: CGKeyCode = 0x09;

pub const KEY_RETURN: CGKeyCode = 0x24;

pub const KEY_DELETE: CGKeyCode = 0x33;

/// `kCGEventSourceUserData`, reserved for content-free paste correlation.
pub const CG_EVENT_SOURCE_USER_DATA_FIELD: u32 = 42;

pub(super) fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

pub fn post_key(keycode: CGKeyCode, flags: CGEventFlags) -> bool {
    post_key_tagged(keycode, flags, None)
}

/// Post one key down/up pair, optionally carrying a content-free correlation
/// token in `kCGEventSourceUserData`. Tag does not change routing or payload.
pub fn post_key_tagged(keycode: CGKeyCode, flags: CGEventFlags, user_data: Option<i64>) -> bool {
    let Some(src) = source() else {
        return false;
    };
    let mut ok = true;
    for down in [true, false] {
        match CGEvent::new_keyboard_event(src.clone(), keycode, down) {
            Ok(ev) => {
                // Stamp even an empty flag set. HIDSystemState can otherwise
                // carry Command from a preceding synthetic Cmd+V into Return.
                ev.set_flags(flags);
                if let Some(user_data) = user_data {
                    ev.set_integer_value_field(CG_EVENT_SOURCE_USER_DATA_FIELD, user_data);
                }
                ev.post(CGEventTapLocation::HID);
            }
            Err(()) => ok = false,
        }
    }
    ok
}

pub fn paste_combo() -> bool {
    post_key(KEY_V, CGEventFlags::CGEventFlagCommand)
}

/// Tagged Cmd+V for bounded destination-event confirmation.
pub fn paste_combo_tagged(token: i64) -> bool {
    post_key_tagged(KEY_V, CGEventFlags::CGEventFlagCommand, Some(token))
}

pub fn copy_combo() -> bool {
    post_key(KEY_C, CGEventFlags::CGEventFlagCommand)
}

pub fn press_return() -> bool {
    post_key(KEY_RETURN, CGEventFlags::empty())
}

pub fn type_unicode(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let Some(src) = source() else {
        return false;
    };
    let mut ok = true;
    for down in [true, false] {
        match CGEvent::new_keyboard_event(src.clone(), 0, down) {
            Ok(ev) => {
                ev.set_flags(CGEventFlags::empty());
                ev.set_string(text);
                ev.post(CGEventTapLocation::HID);
            }
            Err(()) => ok = false,
        }
    }
    ok
}

/// `CGEventKeyboardSetUnicodeString` truncates payloads past 20 UTF-16 units.
const UNICODE_EVENT_MAX_UNITS: usize = 20;

/// Streaming insertion: type arbitrary-length text in event-sized chunks,
/// never splitting a surrogate pair.
pub fn type_unicode_chunked(text: &str) -> bool {
    let mut chunk = String::new();
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > UNICODE_EVENT_MAX_UNITS {
            if !type_unicode(&chunk) {
                return false;
            }
            chunk.clear();
            units = 0;
        }
        units += c.len_utf16();
        chunk.push(c);
    }
    type_unicode(&chunk)
}

/// Streaming insertion: delete `count` characters before the caret.
pub fn send_backspaces(count: usize) -> bool {
    (0..count).all(|_| post_key(KEY_DELETE, CGEventFlags::empty()))
}
