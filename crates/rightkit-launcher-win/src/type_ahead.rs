//! Type-ahead for the launcher (#378). From the chord until the launcher window owns the keyboard
//! ([`begin`] .. [`take`]), plain typed characters are swallowed and kept, so they reach the
//! launcher's field instead of the program that was in front. The window runs out by itself after
//! `CAPTURE_MS`, and what was held is then typed into the window that has the focus, so an open
//! that never completes cannot eat keys for long and nothing typed behind the chord is lost.
//!
//! One handler on the shared hook hub (`rightkit_hotkeys::windows::hub`), registered by
//! [`install`] when the panel starts; it costs one atomic load per key while no capture is open.

use rightkit_automation::win::input::{RawInput, SendOptions, send_input_batch_with};
use rightkit_hotkeys::KeyEventDetail;
use rightkit_hotkeys::windows::HookDecision;
use rightkit_hotkeys::windows::hub::{self, HandlerId, KeyContext};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardLayout, ToUnicodeEx};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

const VK_SHIFT: u32 = 0x10;
const CAPTURE_MS: u64 = 600;
/// The hub order: before the application's other key handlers, so a held character reaches none.
const ORDER: i32 = -100;

static CAPTURE_UNTIL: AtomicU64 = AtomicU64::new(0);
static CAPTURED: Mutex<String> = Mutex::new(String::new());
/// Virtual keys of the held presses, so their swallowing ends with the capture.
static CAPTURED_VKS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static HANDLER: OnceLock<HandlerId> = OnceLock::new();

fn now_ms() -> u64 {
    static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
    EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
        + 1
}

/// Registers the type-ahead handler on the hook hub (once). Call from the panel's start, never
/// from a hook handler.
pub fn install() {
    HANDLER.get_or_init(|| hub::add_key_handler(ORDER, handle));
}

/// Starts holding typed characters. Called when the launcher chord is pressed to open it (it may
/// run on the hook thread). Nothing is held while the keyboard hook is not running.
pub fn begin() {
    if !hub::keyboard_running() {
        return;
    }
    CAPTURED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    let until = now_ms() + CAPTURE_MS;
    CAPTURE_UNTIL.store(until, Ordering::Release);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(CAPTURE_MS + 20));
        if CAPTURE_UNTIL
            .compare_exchange(until, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            replay(&end());
        }
    });
}

/// Ends the capture: held keys stop being swallowed (releases and repeats pass) and the text held
/// so far is returned.
fn end() -> String {
    CAPTURE_UNTIL.store(0, Ordering::Release);
    for vk in CAPTURED_VKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain(..)
    {
        hub::release_swallowed(vk);
    }
    std::mem::take(&mut *CAPTURED.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Stops holding and returns what was typed meanwhile, in order, for the launcher's field. Called
/// by the launcher window once it has the keyboard.
pub fn take() -> String {
    end()
}

/// Stops holding and types what was held into the window that has the focus now. For every path
/// that does not end with the launcher focused (no monitor, a question prompt).
pub fn release() {
    replay(&end());
}

/// Types `text` into the foreground window as Unicode key events, tagged so the hub passes them.
fn replay(text: &str) {
    if text.is_empty() {
        return;
    }
    let events: Vec<RawInput> = text
        .encode_utf16()
        .flat_map(RawInput::unicode_press)
        .collect();
    if send_input_batch_with(&events, &SendOptions::tagged(hub::own_tag())).is_err() {
        crate::host::log_info("send_input_failed", &[("context", "launcher_type_ahead")]);
    }
}

/// One key event from the hub: a plain character typed during a capture is held.
fn handle(event: &KeyEventDetail, cx: &KeyContext) -> HookDecision {
    if !event.down
        || event.alt_down
        || cx.ctrl
        || cx.win
        || CAPTURE_UNTIL.load(Ordering::Acquire) <= now_ms()
    {
        return HookDecision::Pass;
    }
    let Some(text) = capture_text(event.vk, event.scan_code, cx.shift) else {
        return HookDecision::Pass;
    };
    CAPTURED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push_str(&text);
    CAPTURED_VKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(event.vk);
    HookDecision::Swallow
}

/// The characters this physical press types in the front window's layout (shift and lock as
/// now); None for dead keys, non-text keys and control characters.
fn capture_text(vk: u32, scan: u32, shift: bool) -> Option<String> {
    let mut state = [0u8; 256];
    if shift {
        state[VK_SHIFT as usize] = 0x80;
    }
    // SAFETY: plain key-state and window queries with no pointers kept.
    if (unsafe { GetKeyState(0x14) } & 1) != 0 {
        state[0x14] = 1;
    }
    let thread = unsafe { GetWindowThreadProcessId(GetForegroundWindow(), None) };
    let layout = unsafe { GetKeyboardLayout(thread) };
    let mut units = [0u16; 8];
    // Flag 0x4: a pending dead key is left alone.
    // SAFETY: `state` and `units` outlive the call.
    let written = unsafe { ToUnicodeEx(vk, scan, &state, &mut units, 0x4, Some(layout)) };
    if written <= 0 {
        return None;
    }
    let typed = String::from_utf16(&units[..(written as usize).min(units.len())]).ok()?;
    if typed.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7F) {
        None
    } else {
        Some(typed)
    }
}
