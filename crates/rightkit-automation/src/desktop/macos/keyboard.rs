//! Synthetic keyboard input: CGEvent key posting, copy/paste chords, and
//! Unicode text injection.

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use std::cell::Cell;

pub const KEY_C: CGKeyCode = 0x08;

pub const KEY_V: CGKeyCode = 0x09;

pub const KEY_RETURN: CGKeyCode = 0x24;

pub const KEY_DELETE: CGKeyCode = 0x33;

/// `kCGEventSourceUserData`, reserved for content-free paste correlation.
pub const CG_EVENT_SOURCE_USER_DATA_FIELD: u32 = 42;

pub(super) fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

/// Prebuilt, single-use keyboard events for user-initiated input. Caller owns
/// fresh focus, security & modifier authorization immediately before sending.
/// Preparation posts nothing; normal macOS input permissions still apply.
pub struct PreparedInput {
    events: Vec<CGEvent>,
    _source: Option<CGEventSource>,
    attempted: Cell<bool>,
}

impl PreparedInput {
    fn new(count: usize) -> Option<Self> {
        if count > crate::desktop::MAX_PREPARED_INPUT_EVENTS {
            return None;
        }
        let mut events = Vec::new();
        events.try_reserve_exact(count).ok()?;
        Some(Self {
            events,
            _source: if count == 0 { None } else { Some(source()?) },
            attempted: Cell::new(false),
        })
    }

    fn push_pair(
        &mut self,
        key: CGKeyCode,
        flags: CGEventFlags,
        token: Option<i64>,
        text: Option<&[u16]>,
    ) -> Option<()> {
        let src = self._source.as_ref()?;
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(src.clone(), key, down).ok()?;
            event.set_flags(flags);
            if let Some(token) = token {
                event.set_integer_value_field(CG_EVENT_SOURCE_USER_DATA_FIELD, token);
            }
            if let Some(text) = text {
                event.set_string_from_utf16_unchecked(text);
            }
            self.events.push(event);
        }
        Some(())
    }

    /// Post prebuilt events to HID in order, without event creation, OS reads,
    /// waits or allocation. True means every post was issued; CGEventPost has
    /// no delivery acknowledgement. Repeated attempts return false.
    pub fn send_now(&self) -> bool {
        if self.attempted.replace(true) {
            return false;
        }
        for event in &self.events {
            event.post(CGEventTapLocation::HID);
        }
        true
    }
}

/// Prepare tagged Cmd+V down/up with Command stamped on both events.
pub fn prepare_paste_tagged(token: i64) -> Option<PreparedInput> {
    let mut prepared = PreparedInput::new(2)?;
    prepared.push_pair(KEY_V, CGEventFlags::CGEventFlagCommand, Some(token), None)?;
    Some(prepared)
}

/// Prepare Return down/up with empty flags stamped on both events.
pub fn prepare_return() -> Option<PreparedInput> {
    let mut prepared = PreparedInput::new(2)?;
    prepared.push_pair(KEY_RETURN, CGEventFlags::empty(), None, None)?;
    Some(prepared)
}

/// Prepare text in at most 20 UTF-16 units per down/up pair; surrogate pairs
/// stay together. Empty text prepares a no-op without creating a source.
pub fn prepare_unicode_chunked(text: &str) -> Option<PreparedInput> {
    if text.len() > crate::desktop::MAX_PREPARED_INPUT_EVENTS / 2 * UNICODE_EVENT_MAX_UNITS * 3 {
        return None;
    }
    let mut pairs = 0usize;
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > UNICODE_EVENT_MAX_UNITS {
            pairs = pairs.checked_add(1)?;
            units = 0;
        }
        units += c.len_utf16();
    }
    if units != 0 {
        pairs = pairs.checked_add(1)?;
    }
    let mut prepared = PreparedInput::new(pairs.checked_mul(2)?)?;
    let mut chunk = [0u16; UNICODE_EVENT_MAX_UNITS];
    units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > chunk.len() {
            prepared.push_pair(0, CGEventFlags::empty(), None, Some(&chunk[..units]))?;
            units = 0;
        }
        let mut encoded = [0u16; 2];
        let encoded = c.encode_utf16(&mut encoded);
        chunk[units..units + encoded.len()].copy_from_slice(encoded);
        units += encoded.len();
    }
    if units != 0 {
        prepared.push_pair(0, CGEventFlags::empty(), None, Some(&chunk[..units]))?;
    }
    Some(prepared)
}

/// Prepare `count` Backspace down/up pairs with empty flags. Zero is a no-op.
pub fn prepare_backspaces(count: usize) -> Option<PreparedInput> {
    let mut prepared = PreparedInput::new(count.checked_mul(2)?)?;
    for _ in 0..count {
        prepared.push_pair(KEY_DELETE, CGEventFlags::empty(), None, None)?;
    }
    Some(prepared)
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
