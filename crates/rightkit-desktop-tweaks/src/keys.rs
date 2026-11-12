//! Mac-style editing shortcuts on Windows: the left Alt acts as Ctrl for the editing keys, as the
//! Command key does on a Mac. One handler on the shared hook hub
//! (`rightkit_hotkeys::windows::hub`); the hub owns the hook, the modifier state and the
//! swallowing of a handled key's repeats and release. Each auto-repeat of a handled chord injects
//! the chord again (a held Alt+V pastes again, as a held Ctrl+V does) and is swallowed.
//!
//! * Left Alt+A / C / V / X / Z act as Ctrl+A / C / V / X / Z; Alt+Shift+Z acts as Ctrl+Y.
//!   Only the left Alt does this; with the right Alt these keys reach the application.
//!   Alt+S / F / T / W act as Ctrl+S / F / T / W, and with Shift held as Ctrl+Shift+letter
//!   (Save As, reopen a closed tab), since Shift is left as the user holds it
//!   ([`Shortcuts::save_find_tab_close`]).
//!   The physical key is swallowed and one atomic `SendInput` batch is injected: a dummy
//!   `VK_E8` press (so the Alt release that follows never opens a menu), Alt up (and Shift
//!   up when held), Ctrl+key, then Shift/Alt back down and another dummy `VK_E8` press, so
//!   the user's still-held Alt is consistent and its physical release arrives as a normal
//!   Alt-up that no longer counts as a bare Alt tap. Alt is never swallowed itself.
//! * AltGr (reported as Ctrl+Alt), any real Ctrl+Alt, and anything with the Windows key
//!   down pass through, as do Alt+Tab, Alt+F4 and Alt+Space (only the keys listed above
//!   are handled).
//! * With [`Shortcuts::guards`] the editing chords stand down in a terminal and in a full-screen
//!   borderless program; in a terminal Alt+C and Alt+V still copy and paste with the terminal's own
//!   chord (never Ctrl+C, its interrupt).
//!
//! Other chords (a screenshot chord, a clipboard burst, snippet keywords) are the application's:
//! it registers its own hub handlers, before this one (`order` below [`ORDER`]) when it must see
//! a chord first. Key injection for them is [`send_input`] and [`paste`], tagged so the hub passes
//! the injected keys.

use crate::services;
use rightkit_automation::win::input::{RawInput, RawKey, SendOptions, send_input_batch_with};
use rightkit_hotkeys::KeyEventDetail;
use rightkit_hotkeys::windows::HookDecision;
use rightkit_hotkeys::windows::hub::{self, HandlerId, KeyContext};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
};

const VK_LSHIFT: u32 = 0xA0;
const VK_LCONTROL: u32 = 0xA2;
const VK_LMENU: u32 = 0xA4;
const VK_A: u32 = 0x41;
const VK_C: u32 = 0x43;
const VK_F: u32 = 0x46;
const VK_S: u32 = 0x53;
const VK_T: u32 = 0x54;
const VK_W: u32 = 0x57;
const VK_V: u32 = 0x56;
const VK_X: u32 = 0x58;
const VK_INSERT: u32 = 0x2D;
const VK_Y: u32 = 0x59;
const VK_Z: u32 = 0x5A;
/// Unassigned virtual key used to break the "Alt pressed and released alone" sequence.
const VK_DUMMY: u32 = 0xE8;

/// The hub order of the shortcut handler. Handlers that must see an Alt chord first (an Alt+V
/// that pastes a clipboard burst item) register with a lower order.
pub const ORDER: i32 = 100;

/// Which shortcuts are on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shortcuts {
    /// Left Alt acts as Ctrl for A, C, V, X and Z (Alt+Shift+Z as Ctrl+Y).
    pub mac: bool,
    /// Also S, F, T and W (Save, Find, new Tab, close Window), keeping Shift.
    pub save_find_tab_close: bool,
    /// Stand down in terminals and full-screen programs (terminal copy and paste kept).
    pub guards: bool,
}

static ON: Mutex<(Shortcuts, Option<HandlerId>)> = Mutex::new((
    Shortcuts {
        mac: false,
        save_find_tab_close: false,
        guards: false,
    },
    None,
));
static SAVE_FIND_TAB_CLOSE: AtomicBool = AtomicBool::new(false);
static GUARDS: AtomicBool = AtomicBool::new(false);
/// The Alt and Shift keys the user held at the last handled chord, restored after an injection.
static LAST_ALT: AtomicU32 = AtomicU32::new(VK_LMENU);
static LAST_SHIFT: AtomicU32 = AtomicU32::new(VK_LSHIFT);

/// Makes the layer match `shortcuts` now, live: the hub handler is registered while
/// [`Shortcuts::mac`] is on and removed when it is off. Call from any thread but the hook thread.
pub fn apply(shortcuts: Shortcuts) {
    let mut on = ON.lock().unwrap_or_else(PoisonError::into_inner);
    SAVE_FIND_TAB_CLOSE.store(shortcuts.save_find_tab_close, Ordering::Relaxed);
    GUARDS.store(shortcuts.guards, Ordering::Relaxed);
    if shortcuts.mac && on.1.is_none() {
        on.1 = Some(hub::add_key_handler(ORDER, handle));
        if !hub::keyboard_running() {
            let error = hub::last_install_error().unwrap_or_default();
            services::log_info("keys_hook_unavailable", &[("error", &error)]);
        }
    } else if !shortcuts.mac
        && let Some(id) = on.1.take()
    {
        hub::remove(id);
    }
    on.0 = shortcuts;
}

/// The shortcuts in force.
pub fn current() -> Shortcuts {
    ON.lock().unwrap_or_else(PoisonError::into_inner).0
}

enum Plan {
    /// Inject Ctrl+<key>.
    CtrlKey(u32),
    /// Inject Ctrl+<key> and leave Shift as the user holds it (Ctrl+Shift+<key> with it down).
    CtrlKeepShift(u32),
    /// Inject a terminal's own copy or paste chord (`terminal_chord`) for Alt+C / Alt+V.
    TerminalChord(u32, u32),
}

/// One key event from the hub: swallow to replace an Alt chord with its Ctrl chord.
fn handle(event: &KeyEventDetail, cx: &KeyContext) -> HookDecision {
    let vk = event.vk;
    if !event.down || !event.alt_down || cx.ctrl || cx.win {
        return HookDecision::Pass;
    }
    // The editing remap is the left Alt's only (the thumb key); the right Alt keeps its own
    // meaning (menus, and AltGr on layouts that have it).
    if cx.alt_vk != VK_LMENU {
        return HookDecision::Pass;
    }
    let editing = matches!(vk, VK_A | VK_C | VK_V | VK_X | VK_Z)
        || (matches!(vk, VK_S | VK_F | VK_T | VK_W) && SAVE_FIND_TAB_CLOSE.load(Ordering::Relaxed));
    if !editing {
        return HookDecision::Pass;
    }
    // An auto-repeat goes through the same plan as the press, so a held Alt+V pastes again and
    // a held Alt+Z keeps undoing, as Ctrl+V and Ctrl+Z do: each repeat injects the chord once
    // more and is swallowed. Only the per-press trace line below is skipped for repeats.
    let repeat = cx.repeat;
    LAST_ALT.store(cx.alt_vk, Ordering::Relaxed);
    LAST_SHIFT.store(cx.shift_vk, Ordering::Relaxed);
    let shift = cx.shift;
    // The editing chords stand down in a terminal and in a full-screen program (see
    // `alt_remap_guarded`); in a terminal Alt+C and Alt+V still copy and paste, as Cmd+C and
    // Cmd+V do in a Mac terminal, with the terminal's own chord, never Ctrl+C (its interrupt).
    let guarded = alt_remap_guarded();
    let terminal_chord = if guarded && !shift && matches!(vk, VK_C | VK_V) {
        foreground_terminal().map(|class| terminal_chord(&class, vk))
    } else {
        None
    };
    if vk == VK_V && !repeat {
        // One line per Alt+V press (no content), so a paste that does nothing can be traced.
        let route = match terminal_chord {
            Some(_) => "terminal",
            None if guarded => "stood-down",
            None => "ctrl",
        };
        let class = foreground_class();
        services::log_info(
            "keys_alt_v",
            &[
                ("route", route),
                ("shift", if shift { "1" } else { "0" }),
                ("class", &class),
            ],
        );
    }
    let plan = match vk {
        VK_C | VK_V if terminal_chord.is_some() => {
            terminal_chord.map(|(modifier, key)| Plan::TerminalChord(modifier, key))
        }
        _ if guarded => None,
        VK_A | VK_C | VK_V | VK_X if !shift => Some(Plan::CtrlKey(vk)),
        VK_Z if !shift => Some(Plan::CtrlKey(VK_Z)),
        VK_Z => Some(Plan::CtrlKey(VK_Y)),
        VK_S | VK_F | VK_T | VK_W => Some(Plan::CtrlKeepShift(vk)),
        _ => None,
    };
    match plan {
        Some(Plan::CtrlKey(target)) => {
            send_ctrl_chord(target, shift);
            HookDecision::Swallow
        }
        Some(Plan::CtrlKeepShift(target)) => {
            send_ctrl_chord(target, false);
            HookDecision::Swallow
        }
        Some(Plan::TerminalChord(modifier, target)) => {
            send_terminal_chord(modifier, target);
            HookDecision::Swallow
        }
        None => HookDecision::Pass,
    }
}

/// Whether Alt-as-Ctrl stands down for the foreground window: a terminal (Ctrl+C is its
/// interrupt, and Alt+letter belongs to the shell there), or a borderless window that covers its
/// whole monitor, such as a full-screen program or game. Off without `Shortcuts::guards`. Runs on the
/// hook thread for each editing chord, so it only makes a few fast window queries.
fn alt_remap_guarded() -> bool {
    if !GUARDS.load(Ordering::Relaxed) {
        return false;
    }
    if foreground_terminal().is_some() {
        return true;
    }
    // This rule, not RightKit's `style_is_borderless`: a lone WS_BORDER or WS_DLGFRAME is not
    // a caption, and a window larger than its monitor (overhang) still covers it.
    // SAFETY: plain queries; a null window is "no foreground window".
    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return false;
    }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(window, &mut rect) }.is_err() {
        return false;
    }
    // SAFETY: a plain query; the style is read as its low 32 bits, which is all WS_* uses.
    let style = unsafe { GetWindowLongPtrW(window, GWL_STYLE) } as u32;
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return false;
    }
    is_fullscreen_geometry(rect, info.rcMonitor) && is_borderless_style(style)
}

/// The foreground window's class name; empty when there is no foreground window.
fn foreground_class() -> String {
    // SAFETY: plain queries; a null window is "no foreground window".
    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return String::new();
    }
    let mut class = [0u16; 256];
    let length = unsafe { GetClassNameW(window, &mut class) };
    String::from_utf16_lossy(&class[..length.max(0) as usize])
}

/// The foreground window's class when it is a terminal (`is_terminal_class`).
fn foreground_terminal() -> Option<String> {
    let name = foreground_class();
    is_terminal_class(&name).then_some(name)
}

/// A terminal's own copy (for Alt+C) or paste (for Alt+V) chord, as (modifier, key):
/// Ctrl+Shift+C / V in Windows Terminal, the console host, WezTerm, Alacritty and ConEmu;
/// mintty and PuTTY don't take Ctrl+Shift+letter by default, so Ctrl+Insert / Shift+Insert.
fn terminal_chord(class: &str, vk: u32) -> (u32, u32) {
    let copy = vk == VK_C;
    match class {
        "mintty" | "PuTTY" => {
            if copy {
                (VK_LCONTROL, VK_INSERT)
            } else {
                (VK_LSHIFT, VK_INSERT)
            }
        }
        _ => (VK_LCONTROL, vk),
    }
}

/// A terminal's copy or paste chord while the user physically holds Alt: like
/// `send_ctrl_chord`, with Shift added to Ctrl for the Ctrl+Shift+letter chords.
fn send_terminal_chord(modifier: u32, target: u32) {
    let alt = LAST_ALT.load(Ordering::Relaxed);
    let with_shift = modifier == VK_LCONTROL && target != VK_INSERT;
    let mut events = Vec::with_capacity(12);
    events.push(key(VK_DUMMY, false));
    events.push(key(VK_DUMMY, true));
    events.push(key(alt, true));
    events.push(key(modifier, false));
    if with_shift {
        events.push(key(VK_LSHIFT, false));
    }
    events.push(key(target, false));
    events.push(key(target, true));
    if with_shift {
        events.push(key(VK_LSHIFT, true));
    }
    events.push(key(modifier, true));
    events.push(key(alt, false));
    events.push(key(VK_DUMMY, false));
    events.push(key(VK_DUMMY, true));
    send(&events);
    let chord = format!(
        "{modifier:#x}{}+{target:#x}",
        if with_shift { "+shift" } else { "" }
    );
    services::log_info("keys_terminal_chord", &[("sent", &chord)]);
}

/// Window classes of terminal programs: Windows Terminal, the console host (cmd, PowerShell and
/// other classic console programs), mintty (Git Bash), Alacritty, WezTerm, ConEmu and PuTTY.
fn is_terminal_class(name: &str) -> bool {
    matches!(
        name,
        "CASCADIA_HOSTING_WINDOW_CLASS"
            | "ConsoleWindowClass"
            | "mintty"
            | "Alacritty"
            | "org.wezfurlong.wezterm"
            | "VirtualConsoleClass"
            | "PuTTY"
    )
}

fn key(vk: u32, up: bool) -> RawInput {
    let event = if up {
        RawInput::key_up(vk as u16)
    } else {
        RawInput::key_down(vk as u16)
    };
    // The hardware scan code too: SendInput leaves it 0 otherwise, and terminals that read keys by scan
    // code (WezTerm) then miss the chord and type the letter (Alt+V printed "v" in WezTerm, 2026-10-18).
    // Until rightkit-automation fills it (bogusyogi/rightkit#689), this module sets `wScan` on the key itself.
    // SAFETY: a plain table lookup with no pointers.
    let scan = unsafe {
        windows::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW(
            vk,
            windows::Win32::UI::Input::KeyboardAndMouse::MAPVK_VK_TO_VSC,
        )
    } as u16;
    match event {
        RawInput::Key(key) => RawInput::Key(RawKey { scan, ..key }),
        other => other,
    }
}

/// Sends `events` as one batch, tagged with the hook hub's own tag (`hub::own_tag`) so no hub
/// handler sees them; `ctx` names the sender in the log when Windows takes fewer than all of them.
pub fn send_input(events: &[RawInput], ctx: &str) {
    if send_input_batch_with(events, &SendOptions::tagged(hub::own_tag())).is_err() {
        services::log_info("send_input_failed", &[("context", ctx)]);
    }
}

fn send(events: &[RawInput]) {
    send_input(events, "keys");
}

/// Ctrl+V with no key held (a launcher pastes a snippet into the app it gave the foreground
/// back to).
pub fn paste() {
    send(&[
        key(VK_LCONTROL, false),
        key(VK_V, false),
        key(VK_V, true),
        key(VK_LCONTROL, true),
    ]);
}

/// Ctrl+`target` while the user physically holds Alt (and possibly Shift). Order matters:
/// see the module comment.
fn send_ctrl_chord(target: u32, shift: bool) {
    let alt = LAST_ALT.load(Ordering::Relaxed);
    let shift_vk = LAST_SHIFT.load(Ordering::Relaxed);
    let mut events = Vec::with_capacity(14);
    events.push(key(VK_DUMMY, false));
    events.push(key(VK_DUMMY, true));
    events.push(key(alt, true));
    if shift {
        events.push(key(shift_vk, true));
    }
    events.push(key(VK_LCONTROL, false));
    events.push(key(target, false));
    events.push(key(target, true));
    events.push(key(VK_LCONTROL, true));
    if shift {
        events.push(key(shift_vk, false));
    }
    events.push(key(alt, false));
    events.push(key(VK_DUMMY, false));
    events.push(key(VK_DUMMY, true));
    send(&events);
}

fn is_fullscreen_geometry(window: RECT, monitor: RECT) -> bool {
    window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// `WS_CAPTION` is `WS_BORDER | WS_DLGFRAME`; only the full combination is a title bar. A resize
/// frame (`WS_THICKFRAME`) always blocks.
fn is_borderless_style(style: u32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{WS_CAPTION, WS_THICKFRAME};
    let has_caption = style & WS_CAPTION.0 == WS_CAPTION.0;
    let has_resize_frame = style & WS_THICKFRAME.0 != 0;
    !has_caption && !has_resize_frame
}
