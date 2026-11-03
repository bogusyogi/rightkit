//! Raw Windows input injection for callers that must control every event: arbitrary
//! virtual keys (including the unassigned `0xE8` used to mask a lone Alt or Win release),
//! scan codes, the extended-key bit, Unicode units, mouse events, one atomic `SendInput`
//! batch, a `dwExtraInfo` tag so the caller's own hooks can recognise their own injection,
//! and a held-modifier snapshot to restore after a remap.
//!
//! This is the low-level counterpart of the engine's tier-4 `foreground` path. It performs
//! no policy checks, no focus checks and no receipts: it is for a host that owns its own
//! policy (a key remapper, a hotkey layer) and needs the exact event sequence. It does not
//! touch DPI awareness or COM, and works with [`super::WinPlatform::new_minimal`] or with no
//! platform value at all.
//!
//! `SendInput` inserts one call's events into the input stream serially, so no other
//! *physical* input can fall between them. It does not stop other injectors. UIPI blocks
//! injection into a higher-integrity foreground window and the secure desktop.

use super::sys;
use crate::model::Result;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// Named virtual-key codes the helpers need (plain `u16`, the type [`RawInput`] takes).
pub mod vk {
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const LWIN: u16 = 0x5B;
    pub const RWIN: u16 = 0x5C;
    pub const LSHIFT: u16 = 0xA0;
    pub const RSHIFT: u16 = 0xA1;
    pub const LCONTROL: u16 = 0xA2;
    pub const RCONTROL: u16 = 0xA3;
    pub const LMENU: u16 = 0xA4;
    pub const RMENU: u16 = 0xA5;
    /// Unassigned virtual key. Pressing and releasing it between an Alt or Win press and
    /// its release makes Windows treat the Alt/Win as part of a chord, so releasing it does
    /// not open the menu bar (Alt) or the Start menu (Win).
    pub const DUMMY: u16 = 0xE8;
}

/// Keys whose hardware scan code carries the `E0` prefix, so `SendInput` needs
/// `KEYEVENTF_EXTENDEDKEY` for the system (and apps reading `lParam` bit 24) to tell them
/// from their non-extended twins: right Ctrl/Alt, the Windows and Apps keys, the dedicated
/// navigation cluster (PgUp/PgDn/End/Home/arrows/Insert/Delete), PrintScreen, NumLock and
/// the numpad divide. Numpad Enter shares VK_RETURN with Enter, so it cannot be inferred:
/// use [`RawInput::with_extended`].
pub fn is_extended_vk(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // PRIOR NEXT END HOME LEFT UP RIGHT DOWN
            | 0x2C..=0x2E // SNAPSHOT INSERT DELETE
            | 0x5B..=0x5D // LWIN RWIN APPS
            | 0x6F // DIVIDE
            | 0x90 // NUMLOCK
            | 0xA3 // RCONTROL
            | 0xA5 // RMENU
    )
}

/// One keyboard event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawKey {
    /// Virtual-key code. With `by_scan` it is carried but the system ignores it.
    pub vk: u16,
    /// Hardware scan code (`wScan`). 0 with a virtual key fills it from `vk` (MAPVK_VK_TO_VSC)
    /// when the item is sent.
    pub scan: u16,
    pub up: bool,
    /// `KEYEVENTF_EXTENDEDKEY`.
    pub extended: bool,
    /// `KEYEVENTF_SCANCODE`: the event is identified by `scan`, not `vk`.
    pub by_scan: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawMouseButton {
    Left,
    Right,
    Middle,
    /// First extended button (browser Back).
    X1,
    /// Second extended button (browser Forward).
    X2,
}

/// One mouse event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawMouse {
    /// Relative move in mickeys (`dx`, `dy`), subject to pointer acceleration.
    MoveBy {
        dx: i32,
        dy: i32,
    },
    /// Absolute move in normalised coordinates, 0..=65535 across the primary display, or the
    /// whole virtual desktop with `virtual_desktop`.
    MoveTo {
        x: i32,
        y: i32,
        virtual_desktop: bool,
    },
    Button {
        button: RawMouseButton,
        up: bool,
    },
    /// Vertical wheel; 120 is one notch, positive is away from the user (scroll up).
    Wheel(i32),
    /// Horizontal wheel; 120 is one notch, positive is to the right.
    HWheel(i32),
}

/// One typed `SendInput` item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawInput {
    Key(RawKey),
    /// One UTF-16 code unit (`KEYEVENTF_UNICODE`; `wVk` is 0). A character outside the BMP
    /// is two items, high surrogate then low surrogate, each with its own down and up.
    Unicode {
        unit: u16,
        up: bool,
    },
    Mouse(RawMouse),
}

impl RawInput {
    fn key(vk: u16, up: bool) -> RawInput {
        RawInput::Key(RawKey {
            vk,
            scan: 0,
            up,
            extended: is_extended_vk(vk),
            by_scan: false,
        })
    }
    /// Key down for any virtual key (including [`vk::DUMMY`]). The extended bit is set
    /// automatically for the keys in [`is_extended_vk`]; override with [`Self::with_extended`].
    pub fn key_down(vk: u16) -> RawInput {
        RawInput::key(vk, false)
    }
    /// Key up for any virtual key. See [`Self::key_down`] for the extended bit.
    pub fn key_up(vk: u16) -> RawInput {
        RawInput::key(vk, true)
    }
    /// Key down and up for `vk`, in that order.
    pub fn key_press(vk: u16) -> [RawInput; 2] {
        [RawInput::key_down(vk), RawInput::key_up(vk)]
    }
    /// Event identified by hardware scan code (`KEYEVENTF_SCANCODE`), layout independent.
    pub fn scan(scan: u16, up: bool, extended: bool) -> RawInput {
        RawInput::Key(RawKey {
            vk: 0,
            scan,
            up,
            extended,
            by_scan: true,
        })
    }
    pub fn unicode_down(unit: u16) -> RawInput {
        RawInput::Unicode { unit, up: false }
    }
    pub fn unicode_up(unit: u16) -> RawInput {
        RawInput::Unicode { unit, up: true }
    }
    /// Down and up for one UTF-16 unit.
    pub fn unicode_press(unit: u16) -> [RawInput; 2] {
        [RawInput::unicode_down(unit), RawInput::unicode_up(unit)]
    }
    pub fn mouse(m: RawMouse) -> RawInput {
        RawInput::Mouse(m)
    }
    /// The press-and-release of [`vk::DUMMY`] that stops a following Alt or Win release from
    /// opening the menu bar or Start menu.
    pub fn menu_mask() -> [RawInput; 2] {
        RawInput::key_press(vk::DUMMY)
    }
    /// Force the extended-key bit on or off for a keyboard item; other items are unchanged.
    pub fn with_extended(self, extended: bool) -> RawInput {
        match self {
            RawInput::Key(k) => RawInput::Key(RawKey { extended, ..k }),
            other => other,
        }
    }

    fn to_input(self, extra_info: usize) -> INPUT {
        match self {
            RawInput::Key(k) => {
                let mut flags = KEYBD_EVENT_FLAGS(0);
                if k.up {
                    flags |= KEYEVENTF_KEYUP;
                }
                if k.extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if k.by_scan {
                    flags |= KEYEVENTF_SCANCODE;
                }
                // SendInput does not derive wScan from wVk: a window that maps keys by scan
                // code (WezTerm) would receive 0. Fill it from the virtual key unless the
                // caller named a scan code. The extended bit stays with KEYEVENTF_EXTENDEDKEY.
                let scan = if !k.by_scan && k.scan == 0 && k.vk != 0 {
                    let vsc = unsafe { MapVirtualKeyW(u32::from(k.vk), MAPVK_VK_TO_VSC) };
                    (vsc & 0xFF) as u16
                } else {
                    k.scan
                };
                keyboard(k.vk, scan, flags, extra_info)
            }
            RawInput::Unicode { unit, up } => {
                let mut flags = KEYEVENTF_UNICODE;
                if up {
                    flags |= KEYEVENTF_KEYUP;
                }
                keyboard(0, unit, flags, extra_info)
            }
            RawInput::Mouse(m) => {
                let (dx, dy, data, flags) = match m {
                    RawMouse::MoveBy { dx, dy } => (dx, dy, 0, MOUSEEVENTF_MOVE),
                    RawMouse::MoveTo {
                        x,
                        y,
                        virtual_desktop,
                    } => {
                        let mut f = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE;
                        if virtual_desktop {
                            f |= MOUSEEVENTF_VIRTUALDESK;
                        }
                        (x, y, 0, f)
                    }
                    RawMouse::Button { button, up } => {
                        let (flags, data) = match (button, up) {
                            (RawMouseButton::Left, false) => (MOUSEEVENTF_LEFTDOWN, 0),
                            (RawMouseButton::Left, true) => (MOUSEEVENTF_LEFTUP, 0),
                            (RawMouseButton::Right, false) => (MOUSEEVENTF_RIGHTDOWN, 0),
                            (RawMouseButton::Right, true) => (MOUSEEVENTF_RIGHTUP, 0),
                            (RawMouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                            (RawMouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEUP, 0),
                            (RawMouseButton::X1, false) => (MOUSEEVENTF_XDOWN, 1),
                            (RawMouseButton::X1, true) => (MOUSEEVENTF_XUP, 1),
                            (RawMouseButton::X2, false) => (MOUSEEVENTF_XDOWN, 2),
                            (RawMouseButton::X2, true) => (MOUSEEVENTF_XUP, 2),
                        };
                        (0, 0, data, flags)
                    }
                    RawMouse::Wheel(d) => (0, 0, d, MOUSEEVENTF_WHEEL),
                    RawMouse::HWheel(d) => (0, 0, d, MOUSEEVENTF_HWHEEL),
                };
                INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx,
                            dy,
                            // Wheel deltas are signed; the field is a DWORD.
                            mouseData: data as u32,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: extra_info,
                        },
                    },
                }
            }
        }
    }
}

fn keyboard(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS, extra_info: usize) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: extra_info,
            },
        },
    }
}

/// Options for [`send_input_batch_with`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SendOptions {
    /// Written to `dwExtraInfo` of every event in the batch. A low-level keyboard or mouse
    /// hook sees it as `KBDLLHOOKSTRUCT::dwExtraInfo` / `MSLLHOOKSTRUCT::dwExtraInfo`, so a
    /// host can pass its own injected events through untouched. 0 is the system default.
    pub extra_info: usize,
}

impl SendOptions {
    pub fn tagged(extra_info: usize) -> Self {
        SendOptions { extra_info }
    }
}

/// The `INPUT` array a batch becomes (kept apart from the call so it can be inspected).
pub(crate) fn build_inputs(items: &[RawInput], opts: &SendOptions) -> Vec<INPUT> {
    items.iter().map(|i| i.to_input(opts.extra_info)).collect()
}

/// Send `items` as ONE `SendInput` call, in order, tagged `dwExtraInfo = 0`. An empty batch
/// is a no-op. If fewer events than requested were inserted (UIPI, secure desktop) the call
/// returns `Error::Platform(inserted, ..)`; the inserted prefix is not rolled back, so a
/// caller that pressed a modifier should build the release into the same batch.
pub fn send_input_batch(items: &[RawInput]) -> Result<()> {
    send_input_batch_with(items, &SendOptions::default())
}

/// [`send_input_batch`] with options (the `dwExtraInfo` tag).
pub fn send_input_batch_with(items: &[RawInput], opts: &SendOptions) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    sys::send(&build_inputs(items, opts))
}

/// The modifier keys that were down at one instant, so a caller that releases them (the
/// engine's foreground path does, to keep a held push-to-talk chord out of a synthetic
/// chord) can put them back.
///
/// Only the specific left and right keys are tracked, never the generic Shift/Ctrl/Alt
/// codes, so restoring presses each physical key once.
///
/// `capture` reads `GetAsyncKeyState`, which reflects the system key table *including
/// injected events*: after your own injected key-up the key reads as up even while the user
/// still holds it. Capture BEFORE injecting.
///
/// Restoring re-presses unconditionally. If the user physically released a key between
/// capture and restore, the key would stay logically down. Keep that window small: put
/// [`Self::restore_inputs`] at the end of the same batch as the release instead of calling
/// [`Self::restore`] later. Releasing then restoring Alt or Win can open the menu bar or
/// Start menu; add [`RawInput::menu_mask`] after the release.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModifierSnapshot {
    held: Vec<u16>,
}

impl ModifierSnapshot {
    /// The keys tracked, in the order they are released and restored: Shift, Ctrl, Alt, Win,
    /// left before right.
    pub const TRACKED: [u16; 8] = [
        vk::LSHIFT,
        vk::RSHIFT,
        vk::LCONTROL,
        vk::RCONTROL,
        vk::LMENU,
        vk::RMENU,
        vk::LWIN,
        vk::RWIN,
    ];

    /// Snapshot the modifiers down right now.
    pub fn capture() -> Self {
        Self::capture_with(|k| (unsafe { GetAsyncKeyState(i32::from(k)) } as u16) & 0x8000 != 0)
    }

    /// Snapshot using a caller-supplied key-state probe (`true` = down). `capture` is this
    /// with `GetAsyncKeyState`; hosts that track keys in their own hook can pass that state.
    pub fn capture_with(is_down: impl Fn(u16) -> bool) -> Self {
        ModifierSnapshot {
            held: Self::TRACKED.into_iter().filter(|k| is_down(*k)).collect(),
        }
    }

    /// A snapshot of exactly these keys. Codes outside [`Self::TRACKED`] are dropped and
    /// duplicates collapse, so a generic `VK_MENU` cannot sneak in.
    pub fn from_held(keys: impl IntoIterator<Item = u16>) -> Self {
        let keys: Vec<u16> = keys.into_iter().collect();
        Self::capture_with(|k| keys.contains(&k))
    }

    /// The keys that were down, in [`Self::TRACKED`] order.
    pub fn held(&self) -> &[u16] {
        &self.held
    }
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
    pub fn contains(&self, vk: u16) -> bool {
        self.held.contains(&vk)
    }

    /// Key-up for every captured key, in reverse of the tracked order.
    pub fn release_inputs(&self) -> Vec<RawInput> {
        self.held
            .iter()
            .rev()
            .map(|k| RawInput::key_up(*k))
            .collect()
    }

    /// Key-down for every captured key, in tracked order (modifiers go down in the order
    /// they would be pressed).
    pub fn restore_inputs(&self) -> Vec<RawInput> {
        self.held.iter().map(|k| RawInput::key_down(*k)).collect()
    }

    /// Re-press the captured keys in one batch, tagged 0. No-op for an empty snapshot.
    pub fn restore(&self) -> Result<()> {
        self.restore_with(&SendOptions::default())
    }

    /// [`Self::restore`] with the `dwExtraInfo` tag.
    pub fn restore_with(&self, opts: &SendOptions) -> Result<()> {
        send_input_batch_with(&self.restore_inputs(), opts)
    }
}
