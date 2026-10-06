//! Synthetic input. Tier 3 posts to one pid (`CGEventPostToPid`); tier 4 posts
//! to the HID tap. Mouse events carry window-number fields so AppKit routes
//! them to the intended window of a background app.

use super::ffi::*;
use crate::keys::{self, Chord};
use crate::model::*;
use crate::platform::InputEvent;
use std::os::raw::c_ulong;
use std::time::Duration;

#[derive(Clone, Copy)]
pub enum Dest {
    Pid(i32),
    Hid,
}

fn post(dest: Dest, ev: &Cf) {
    unsafe {
        match dest {
            Dest::Pid(pid) => CGEventPostToPid(pid, ev.0),
            Dest::Hid => CGEventPost(0, ev.0),
        }
    }
}

fn source(dest: Dest) -> Result<Cf> {
    // Private state for pid-addressed events (ignores the user's held modifiers);
    // HID system state for the foreground path (what the OS treats as the keyboard).
    let state = match dest {
        Dest::Pid(_) => -1,
        Dest::Hid => 1,
    };
    Cf::new(unsafe { CGEventSourceCreate(state) }).ok_or(Error::Unsupported("event source"))
}

fn key_pair(dest: Dest, src: &Cf, code: u16, flags: u64, unicode: Option<&[u16]>) -> Result<()> {
    for down in [1u8, 0u8] {
        let ev = Cf::new(unsafe { CGEventCreateKeyboardEvent(src.0, code, down) }).ok_or(Error::Unsupported("key event"))?;
        unsafe {
            if let Some(u) = unicode {
                CGEventKeyboardSetUnicodeString(ev.0, u.len() as c_ulong, u.as_ptr());
            }
            // Stamp even an empty set: a held modifier must not leak into the next key.
            CGEventSetFlags(ev.0, flags);
        }
        post(dest, &ev);
    }
    Ok(())
}

fn key_chord(dest: Dest, c: Chord) -> Result<()> {
    let code = keys::mac_keycode(c.key).ok_or_else(|| Error::Invalid(format!("no mac keycode for {:?}", c.key)))?;
    let src = source(dest)?;
    key_pair(dest, &src, code, keys::mac_flags(c.mods), None)
}

fn type_text(dest: Dest, text: &str) -> Result<()> {
    let src = source(dest)?;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            key_pair(dest, &src, 36, 0, None)?;
        } else {
            let mut buf = [0u16; 2];
            let u = ch.encode_utf16(&mut buf);
            key_pair(dest, &src, 0, 0, Some(u))?;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}

fn window_under(pid: i32, x: f64, y: f64) -> Option<u32> {
    super::list_windows(true)
        .into_iter()
        .find(|w| w.pid == pid && w.layer == 0 && w.bounds.contains(x, y))
        .map(|w| w.window_id as u32)
}

fn click(dest: Dest, window: Option<u64>, x: f64, y: f64, button: Button, count: u32) -> Result<()> {
    let src = source(dest)?;
    let pt = CGPoint { x, y };
    let (down_t, up_t, btn) = match button {
        Button::Left => (1u32, 2u32, 0u32),
        Button::Right => (3, 4, 1),
    };
    let wid = match (dest, window) {
        (_, Some(w)) => Some(w as u32),
        (Dest::Pid(pid), None) => window_under(pid, x, y),
        _ => None,
    };
    let saved = matches!(dest, Dest::Hid).then(|| Cf::new(unsafe { CGEventCreate(std::ptr::null()) }).map(|e| unsafe { CGEventGetLocation(e.0) })).flatten();
    let mk = |ty: u32, state: i64| -> Result<Cf> {
        let ev = Cf::new(unsafe { CGEventCreateMouseEvent(src.0, ty, pt, btn) }).ok_or(Error::Unsupported("mouse event"))?;
        unsafe {
            CGEventSetIntegerValueField(ev.0, 1, state); // click state
            if let Some(w) = wid {
                CGEventSetIntegerValueField(ev.0, 91, i64::from(w)); // window under pointer
                CGEventSetIntegerValueField(ev.0, 92, i64::from(w)); // ...that can handle this event
            }
            if let Dest::Pid(pid) = dest {
                CGEventSetIntegerValueField(ev.0, 40, i64::from(pid)); // target unix pid
            }
        }
        Ok(ev)
    };
    post(dest, &mk(5, 0)?); // mouse moved (hover) so tracking areas see the pointer
    std::thread::sleep(Duration::from_millis(30));
    for n in 1..=count.max(1) {
        post(dest, &mk(down_t, i64::from(n))?);
        std::thread::sleep(Duration::from_millis(40));
        post(dest, &mk(up_t, i64::from(n))?);
        std::thread::sleep(Duration::from_millis(40));
    }
    if let Some(p) = saved {
        unsafe { CGWarpMouseCursorPosition(p) };
    }
    Ok(())
}

fn scroll(dest: Dest, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
    let src = source(dest)?;
    // Our convention: positive dy scrolls content down (like DOM deltaY); CG wheel is inverse.
    let ev = Cf::new(unsafe { CGEventCreateScrollWheelEvent2(src.0, 1, 2, (-dy).round() as i32, (-dx).round() as i32, 0) })
        .ok_or(Error::Unsupported("scroll event"))?;
    unsafe { CGEventSetLocation(ev.0, CGPoint { x, y }) };
    post(dest, &ev);
    Ok(())
}

pub fn send(dest: Dest, window: Option<u64>, ev: &InputEvent) -> Result<()> {
    if unsafe { super::ffi::AXIsProcessTrusted() } == 0 {
        return Err(Error::PermissionDenied("Accessibility"));
    }
    match ev {
        InputEvent::Text(t) => type_text(dest, t),
        InputEvent::Key(c) => key_chord(dest, *c),
        InputEvent::Click { x, y, button, count } => click(dest, window, *x, *y, *button, *count),
        InputEvent::Scroll { x, y, dx, dy } => scroll(dest, *x, *y, *dx, *dy),
    }
}

