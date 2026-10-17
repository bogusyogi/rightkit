//! Raw detail of low-level hook events, as plain data: the scan code, `dwExtraInfo` and the
//! Alt-down flag of a keyboard event, and a classified mouse event. Pure, so the flag and
//! message decoding is tested on every host; the Windows hook in `windows` fills these in.
//!
//! [`KeyEventDetail`] wraps the existing `windows::KeyEvent` fields instead of adding to that
//! struct: `KeyEvent` is a public struct with public fields and no `#[non_exhaustive]`, so a new
//! field would break every caller that builds or destructures one. The types here are
//! `#[non_exhaustive]` so they can grow.

/// `KBDLLHOOKSTRUCT.flags` bit 0: the key is an extended key (right Ctrl/Alt, arrows, ...).
pub const LLKHF_EXTENDED: u32 = 0x01;
/// `KBDLLHOOKSTRUCT.flags` bit 4: injected by `SendInput`.
pub const LLKHF_INJECTED: u32 = 0x10;
/// `KBDLLHOOKSTRUCT.flags` bit 5: the Alt key is down (set for `WM_SYSKEYDOWN` / `WM_SYSKEYUP`
/// events made while Alt is held).
pub const LLKHF_ALTDOWN: u32 = 0x20;
/// `MSLLHOOKSTRUCT.flags` bit 0: injected by `SendInput`.
pub const LLMHF_INJECTED: u32 = 0x01;

/// First message number a caller may post to the hook thread (`WM_APP`).
pub const THREAD_MESSAGE_MIN: u32 = 0x8000;
/// Last message number a caller may post to the hook thread (`0xBFFF`, the end of the
/// application range).
pub const THREAD_MESSAGE_MAX: u32 = 0xBFFF;

/// Whether `msg` is in the application range (`WM_APP..=0xBFFF`) a caller may post to the hook
/// thread. System and `WM_USER` messages are refused: the thread uses `WM_QUIT` to stop.
pub fn is_thread_message(msg: u32) -> bool {
    (THREAD_MESSAGE_MIN..=THREAD_MESSAGE_MAX).contains(&msg)
}

/// One keyboard event from the low-level hook, with the fields `KeyEvent` does not carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyEventDetail {
    /// Virtual-key code.
    pub vk: u32,
    /// `true` for a key-down (`WM_KEYDOWN`, `WM_SYSKEYDOWN`), `false` for a key-up.
    pub down: bool,
    /// Hardware scan code (`KBDLLHOOKSTRUCT.scanCode`).
    pub scan_code: u32,
    /// `dwExtraInfo`: the tag a `SendInput` caller put on the event (0 for real hardware).
    /// Compare it with the marker your own posts carry.
    pub extra_info: usize,
    /// `LLKHF_INJECTED`: the event came from `SendInput`.
    pub injected: bool,
    /// `LLKHF_ALTDOWN`: Alt was down when the event was made.
    pub alt_down: bool,
    /// `LLKHF_EXTENDED`: an extended key.
    pub extended: bool,
}

impl KeyEventDetail {
    /// Decode the fields of a `KBDLLHOOKSTRUCT`.
    pub fn from_raw(vk: u32, down: bool, scan_code: u32, flags: u32, extra_info: usize) -> Self {
        Self {
            vk,
            down,
            scan_code,
            extra_info,
            injected: flags & LLKHF_INJECTED != 0,
            alt_down: flags & LLKHF_ALTDOWN != 0,
            extended: flags & LLKHF_EXTENDED != 0,
        }
    }
}

/// What a low-level mouse message reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MouseKind {
    Move,
    LeftDown,
    LeftUp,
    RightDown,
    RightUp,
    MiddleDown,
    MiddleUp,
    /// `XBUTTON1` (usually "back") pressed.
    X1Down,
    X1Up,
    /// `XBUTTON2` (usually "forward").
    X2Down,
    X2Up,
    /// Vertical wheel; see `MouseEvent::wheel_delta`.
    Wheel,
    /// Horizontal wheel; see `MouseEvent::wheel_delta`.
    HWheel,
    /// A message this crate does not classify; the raw message number.
    Other(u32),
}

/// One mouse event from the low-level mouse hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// Screen position in physical pixels.
    pub x: i32,
    pub y: i32,
    /// Wheel movement in units of `WHEEL_DELTA` (120 per notch), signed; 0 for other kinds.
    pub wheel_delta: i16,
    /// `dwExtraInfo` (tag of a `SendInput` caller; 0 for real hardware).
    pub extra_info: usize,
    /// `LLMHF_INJECTED`.
    pub injected: bool,
}

impl MouseEvent {
    /// Decode an `MSLLHOOKSTRUCT` and its window message.
    pub fn from_raw(
        message: u32,
        x: i32,
        y: i32,
        mouse_data: u32,
        flags: u32,
        extra_info: usize,
    ) -> Self {
        let kind = classify_mouse_message(message, mouse_data);
        let wheel_delta = match kind {
            MouseKind::Wheel | MouseKind::HWheel => (mouse_data >> 16) as u16 as i16,
            _ => 0,
        };
        Self {
            kind,
            x,
            y,
            wheel_delta,
            extra_info,
            injected: flags & LLMHF_INJECTED != 0,
        }
    }
}

/// Classify a low-level mouse window message. `mouse_data` is `MSLLHOOKSTRUCT.mouseData`; its
/// high word says which X button a `WM_XBUTTON*` message is about.
pub fn classify_mouse_message(message: u32, mouse_data: u32) -> MouseKind {
    let x_button = (mouse_data >> 16) as u16;
    match message {
        0x0200 => MouseKind::Move,
        0x0201 => MouseKind::LeftDown,
        0x0202 => MouseKind::LeftUp,
        0x0204 => MouseKind::RightDown,
        0x0205 => MouseKind::RightUp,
        0x0207 => MouseKind::MiddleDown,
        0x0208 => MouseKind::MiddleUp,
        0x020A => MouseKind::Wheel,
        0x020E => MouseKind::HWheel,
        0x020B | 0x020C => {
            let down = message == 0x020B;
            match (x_button, down) {
                (1, true) => MouseKind::X1Down,
                (1, false) => MouseKind::X1Up,
                (2, true) => MouseKind::X2Down,
                (2, false) => MouseKind::X2Up,
                _ => MouseKind::Other(message),
            }
        }
        m => MouseKind::Other(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_flags_decode() {
        let d = KeyEventDetail::from_raw(0x20, true, 0x39, LLKHF_ALTDOWN, 0xABCD);
        assert_eq!(
            (d.vk, d.down, d.scan_code, d.extra_info),
            (0x20, true, 0x39, 0xABCD)
        );
        assert!(d.alt_down && !d.injected && !d.extended);

        let d = KeyEventDetail::from_raw(0x41, false, 0x1E, LLKHF_INJECTED | LLKHF_EXTENDED, 0);
        assert!(d.injected && d.extended && !d.alt_down && !d.down);

        let d = KeyEventDetail::from_raw(0x41, true, 0x1E, 0, 0);
        assert!(!d.injected && !d.extended && !d.alt_down);
    }

    #[test]
    fn mouse_messages_classify() {
        assert_eq!(classify_mouse_message(0x0200, 0), MouseKind::Move);
        assert_eq!(classify_mouse_message(0x0201, 0), MouseKind::LeftDown);
        assert_eq!(classify_mouse_message(0x0202, 0), MouseKind::LeftUp);
        assert_eq!(classify_mouse_message(0x0204, 0), MouseKind::RightDown);
        assert_eq!(classify_mouse_message(0x0205, 0), MouseKind::RightUp);
        assert_eq!(classify_mouse_message(0x0207, 0), MouseKind::MiddleDown);
        assert_eq!(classify_mouse_message(0x0208, 0), MouseKind::MiddleUp);
        assert_eq!(classify_mouse_message(0x020A, 0), MouseKind::Wheel);
        assert_eq!(classify_mouse_message(0x020E, 0), MouseKind::HWheel);
        assert_eq!(classify_mouse_message(0x020B, 1 << 16), MouseKind::X1Down);
        assert_eq!(classify_mouse_message(0x020C, 1 << 16), MouseKind::X1Up);
        assert_eq!(classify_mouse_message(0x020B, 2 << 16), MouseKind::X2Down);
        assert_eq!(classify_mouse_message(0x020C, 2 << 16), MouseKind::X2Up);
        assert_eq!(
            classify_mouse_message(0x020B, 3 << 16),
            MouseKind::Other(0x020B)
        );
        assert_eq!(classify_mouse_message(0x0203, 0), MouseKind::Other(0x0203));
    }

    #[test]
    fn wheel_delta_is_signed_high_word() {
        let up = MouseEvent::from_raw(0x020A, 10, 20, 120u32 << 16, 0, 7);
        assert_eq!(
            (up.kind, up.wheel_delta, up.x, up.y, up.extra_info),
            (MouseKind::Wheel, 120, 10, 20, 7)
        );
        let down = MouseEvent::from_raw(
            0x020A,
            0,
            0,
            ((-120i16) as u16 as u32) << 16,
            LLMHF_INJECTED,
            0,
        );
        assert_eq!(down.wheel_delta, -120);
        assert!(down.injected);
        let click = MouseEvent::from_raw(0x0201, 0, 0, 0xFFFF_0000, 0, 0);
        assert_eq!(click.wheel_delta, 0);
        assert!(!click.injected);
    }

    #[test]
    fn thread_message_range() {
        assert!(is_thread_message(0x8000));
        assert!(is_thread_message(0xBFFF));
        assert!(!is_thread_message(0x7FFF));
        assert!(!is_thread_message(0xC000));
        assert!(!is_thread_message(0x0012)); // WM_QUIT
        assert!(!is_thread_message(0x0400)); // WM_USER
    }
}
