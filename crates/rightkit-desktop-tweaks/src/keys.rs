//! Global keyboard layer: Mac-style editing shortcuts and the screenshot hotkeys.
//!
//! One `WH_KEYBOARD_LL` hook on a dedicated thread with its own message loop (the hook
//! callback runs inside that thread's `GetMessageW`, so it must return quickly and never
//! block). Physical events only: anything flagged `LLKHF_INJECTED` or carrying our own
//! `dwExtraInfo` tag is passed through untouched, so the keys we send never loop back.
//!
//! * Left Alt+A / C / V / X / Z act as Ctrl+A / C / V / X / Z; Alt+Shift+Z acts as Ctrl+Y.
//!   Only the left Alt does this; with the right Alt these keys reach the application.
//!   Alt+S / F / T / W act as Ctrl+S / F / T / W, and with Shift held as Ctrl+Shift+letter
//!   (Save As, reopen a closed tab), since Shift is left as the user holds it.
//!   The physical key is swallowed and one atomic `SendInput` batch is injected: a dummy
//!   `VK_E8` press (so the Alt release that follows never opens a menu), Alt up (and Shift
//!   up when held), Ctrl+key, then Shift/Alt back down and another dummy `VK_E8` press, so
//!   the user's still-held Alt is consistent and its physical release arrives as a normal
//!   Alt-up that no longer counts as a bare Alt tap. Alt is never swallowed itself.
//! * AltGr (reported as Ctrl+Alt), any real Ctrl+Alt, and anything with the Windows key
//!   down pass through, as do Alt+Tab, Alt+F4 and Alt+Space (only the keys listed above
//!   are handled). The one exception is Alt+Space (no Shift) while the launcher has been
//!   told to take it from another program: the extra handler swallows both key-downs (the
//!   first and its auto-repeats) and the Space release, and the injected dummy key press
//!   keeps the Alt release from opening the window menu. Nothing else is swallowed for it.
//! * Alt+Shift+4 / Alt+Shift+5 request the screenshot region picker / toolbar from
//!   the screenshot service. While a screenshot session is active, Esc and Space are swallowed and
//!   forwarded to it (the overlay windows never take focus, so the hook is how they see
//!   the keyboard).
//! * Snippet keywords (the snippet service, on while `launcherSnippetExpansion` is on) see
//!   every physical key press that is not a modifier, and the mouse hook (installed only while they
//!   or the tool wheel are on, removed when the switches turn off) tells them about each left click. Neither swallows anything.
//! * `set_extra_handler` is the hook point for other Alt chords (e.g. a send shortcut): the
//!   handler runs on the hook thread for every Alt+key press nothing else claimed and
//!   returns true to swallow it.

use crate::services;
use rightkit_automation::win::input::{
    ModifierSnapshot, RawInput, RawKey, SendOptions, send_input_batch_with,
};
use rightkit_hotkeys::windows::{HookConfig, HookDecision, LowLevelHooks};
use rightkit_hotkeys::{KeyEventDetail, MouseEvent, MouseKind};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardLayout, ToUnicodeEx};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId,
};

const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_4: u32 = 0x34;
const VK_5: u32 = 0x35;
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
/// `dwExtraInfo` marker on every event the notch injects ("PULS"): this module, the paste, and the
/// snippet Backspaces all send through `send_input`, so the key hook passes them.
const INJECT_TAG: usize = 0x5055_4C53;

static MAC_SHORTCUTS: AtomicBool = AtomicBool::new(true);
static SCREENSHOT_KEYS: AtomicBool = AtomicBool::new(true);
/// Snippet keywords are on (`launcher_snippets.rs`): the hook sees every key press and left click.
static SNIPPETS_ON: AtomicBool = AtomicBool::new(false);

// Physical modifier state, maintained from non-injected events on the hook thread.
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
/// Whether the hook is installed, which is when `SIDES` can be trusted.
static HOOK_UP: AtomicBool = AtomicBool::new(false);
/// Physical state of the six side-specific modifiers (`VK_LSHIFT` to `VK_RMENU`, in that
/// order), from non-injected events only.
static SIDES: [AtomicBool; 6] = [const { AtomicBool::new(false) }; 6];
static ALT_VK: AtomicU32 = AtomicU32::new(VK_LMENU);
static SHIFT_VK: AtomicU32 = AtomicU32::new(VK_LSHIFT);
/// Keys whose press we swallowed and whose release (and auto-repeat) must be swallowed too.
static SWALLOWED: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];

/// Type-ahead for the launcher (#378). From the chord until the launcher window owns the keyboard
/// (`capture_begin` .. `capture_take`), plain typed characters are swallowed here and kept, so
/// they reach the launcher's field instead of the program that was in front. The window runs out
/// by itself after `CAPTURE_MS`, so an open that never completes cannot eat keys for long.
const CAPTURE_MS: u64 = 600;
static CAPTURE_UNTIL: AtomicU64 = AtomicU64::new(0);
static CAPTURED: Mutex<String> = Mutex::new(String::new());

fn capture_now_ms() -> u64 {
    static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
    EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
        + 1
}

/// Virtual keys of the held presses, so their swallowing ends with the capture.
static CAPTURED_VKS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Starts holding typed characters. Called when the launcher chord is pressed to open it. If the
/// launcher has not taken them by the end of the window, they are typed into the window that had
/// the focus (nothing typed behind the chord is ever lost).
pub fn capture_begin() {
    if !HOOK_UP.load(Ordering::Acquire) {
        return;
    }
    CAPTURED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    let until = capture_now_ms() + CAPTURE_MS;
    CAPTURE_UNTIL.store(until, Ordering::Release);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(CAPTURE_MS + 20));
        if CAPTURE_UNTIL
            .compare_exchange(until, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            replay(&end_capture());
        }
    });
}

/// Ends the capture: held keys stop being swallowed (releases and repeats pass) and the text
/// held so far is returned.
fn end_capture() -> String {
    CAPTURE_UNTIL.store(0, Ordering::Release);
    for vk in CAPTURED_VKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain(..)
    {
        SWALLOWED[vk as usize].store(false, Ordering::Relaxed);
    }
    std::mem::take(&mut *CAPTURED.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Stops holding and returns what was typed meanwhile, in order, for the launcher's field. Called
/// by the launcher window once it has the keyboard.
pub fn capture_take() -> String {
    end_capture()
}

/// Stops holding and types what was held into the window that has the focus now. For every path
/// that does not end with the launcher focused (no monitor, a question prompt).
pub fn capture_release() {
    replay(&end_capture());
}

/// Types `text` into the foreground window as Unicode key events, tagged so this hook passes them.
fn replay(text: &str) {
    let events: Vec<RawInput> = text
        .encode_utf16()
        .flat_map(RawInput::unicode_press)
        .collect();
    send_input(&events, "launcher_type_ahead");
}

/// The characters this physical press types in the front window's layout (shift and lock as
/// now); None for dead keys, non-text keys and control characters.
fn capture_text(vk: u32, scan: u32) -> Option<String> {
    let mut state = [0u8; 256];
    if SHIFT_DOWN.load(Ordering::Relaxed) {
        state[VK_SHIFT as usize] = 0x80;
    }
    if (unsafe { GetKeyState(0x14) } & 1) != 0 {
        state[0x14] = 1;
    }
    let thread = unsafe { GetWindowThreadProcessId(GetForegroundWindow(), None) };
    let layout = unsafe { GetKeyboardLayout(thread) };
    let mut units = [0u16; 8];
    // Flag 0x4: a pending dead key is left alone.
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

/// Whether the user physically holds the side-specific modifier `vk` (`VK_LSHIFT` to
/// `VK_RMENU`), ignoring what this program injected. None when the hook is not running (it
/// runs only while the Mac shortcuts or the screenshot keys are on) or `vk` is another key.
pub fn physically_down(vk: u32) -> Option<bool> {
    if !HOOK_UP.load(Ordering::Acquire) || !(VK_LSHIFT..=VK_RMENU).contains(&vk) {
        return None;
    }
    Some(SIDES[(vk - VK_LSHIFT) as usize].load(Ordering::Relaxed))
}

/// An Alt+key press offered to the extra handler.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // read by handlers registered through `set_extra_handler`
pub struct Chord {
    pub vk: u32,
    pub shift: bool,
    /// Auto-repeat of a key already reported (the handler decided about it before).
    pub repeat: bool,
}

pub type ExtraHandler = fn(Chord) -> bool;

static EXTRA: OnceLock<ExtraHandler> = OnceLock::new();

/// Registers the one extra Alt-chord handler (first registration wins). Must be called
/// before `start`; the handler runs on the hook thread and must return immediately.
#[allow(dead_code)] // hook point: the LocalSend/send module registers here
pub fn set_extra_handler(handler: ExtraHandler) -> bool {
    EXTRA.set(handler).is_ok()
}

/// The keyboard hook, taken down (and `HOOK_UP` cleared) when dropped.
struct HookThread(#[allow(dead_code)] LowLevelHooks);

impl Drop for HookThread {
    fn drop(&mut self) {
        HOOK_UP.store(false, Ordering::Release);
    }
}

/// What is running now: the hook (only while a feature needs it) and the screenshot thread.
struct Running {
    hook: Option<HookThread>,
    shots: Option<Box<dyn std::any::Any + Send>>,
}

static RUNNING: Mutex<Running> = Mutex::new(Running {
    hook: None,
    shots: None,
});

/// Who wants the process's one `WH_MOUSE_LL` hook (RightKit allows one): the snippet keywords
/// clear their buffer on a left click, and the tool wheel (`wheel.rs`) takes middle clicks.
const MOUSE_SNIPPETS: u8 = 1;
const MOUSE_WHEEL: u8 = 2;
static MOUSE_WANTS: AtomicU8 = AtomicU8::new(0);
/// The mouse hook, installed only while someone wants it.
static MOUSE: Mutex<Option<LowLevelHooks>> = Mutex::new(None);

/// Held by the notch for its lifetime; dropping it stops the hook, then the screenshot thread.
pub struct Keys;

impl Drop for Keys {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        // The hook stops first so it no longer posts to the screenshot thread.
        running.hook = None;
        running.shots = None;
        *MOUSE.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Puts the keyboard and mouse hooks in again (#310). Windows removes a low-level hook that
/// stalled, and a hook outlives none of: sleep and wake, a locked session coming back, an Explorer
/// restart; none of them tells the hook it is gone, so Alt+V and the middle-click wheel would
/// stay dead until a restart. Only the hooks that are installed are redone. Called from the
/// notch's window procedure on resume, unlock and `TaskbarCreated`, never from a hook.
pub fn reinstall() {
    let keyboard = {
        let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        let had = running.hook.take().is_some();
        if had {
            running.hook = start_hook();
        }
        had
    };
    let mouse = {
        let mut hook = MOUSE.lock().unwrap_or_else(PoisonError::into_inner);
        let had = hook.take().is_some();
        if had {
            match LowLevelHooks::install(HookConfig::new().mouse(mouse_event)) {
                Ok(installed) => *hook = Some(installed),
                Err(error) => services::log_info(
                    "keys_mouse_hook_unavailable",
                    &[("error", &format!("{error:?}"))],
                ),
            }
        }
        had
    };
    if keyboard || mouse {
        services::log_info(
            "hook_reinstalled",
            &[
                ("keyboard", if keyboard { "yes" } else { "no" }),
                ("mouse", if mouse { "yes" } else { "no" }),
            ],
        );
    }
}

/// Starts the layer for the stored settings; `apply` changes it later, live.
pub fn start(mac_shortcuts: bool, screenshot_keys: bool) -> Option<Keys> {
    apply(mac_shortcuts, screenshot_keys);
    Some(Keys)
}

/// The launcher takes Alt+Space through this hook (another program holds the chord and the
/// user chose the application), so the hook must run even with the Mac shortcuts and screenshots off.
static LAUNCHER_HOOK: AtomicBool = AtomicBool::new(false);

/// Keeps the hook installed for the launcher (or lets it go), whatever the other two settings
/// say. Called from the launcher's thread, never from the hook.
pub fn launcher_needs_hook(on: bool) {
    if LAUNCHER_HOOK.swap(on, Ordering::Relaxed) != on {
        apply(
            MAC_SHORTCUTS.load(Ordering::Relaxed),
            SCREENSHOT_KEYS.load(Ordering::Relaxed),
        );
    }
}

/// Keeps the hook installed for snippet keywords (`launcher_snippets.rs`) while any are set, or
/// lets it go. Called from the settings' thread, never from the hook.
pub fn snippets_need_hook(on: bool) {
    if SNIPPETS_ON.swap(on, Ordering::Relaxed) != on {
        apply(
            MAC_SHORTCUTS.load(Ordering::Relaxed),
            SCREENSHOT_KEYS.load(Ordering::Relaxed),
        );
        mouse_wanted(MOUSE_SNIPPETS, on);
    }
}

/// Installs the process's mouse hook while the snippet keywords or the tool wheel want it and
/// removes it when neither does. Called from the snippets' and the wheel's own threads, never
/// from a hook.
pub fn wheel_needs_mouse(on: bool) {
    mouse_wanted(MOUSE_WHEEL, on);
}

fn mouse_wanted(who: u8, on: bool) {
    let mut hook = MOUSE.lock().unwrap_or_else(PoisonError::into_inner);
    let wants = if on {
        MOUSE_WANTS.fetch_or(who, Ordering::Relaxed) | who
    } else {
        MOUSE_WANTS.fetch_and(!who, Ordering::Relaxed) & !who
    };
    if wants == 0 {
        *hook = None;
    } else if hook.is_none() {
        match LowLevelHooks::install(HookConfig::new().mouse(mouse_event)) {
            Ok(installed) => *hook = Some(installed),
            Err(error) => services::log_info(
                "keys_mouse_hook_unavailable",
                &[("error", &format!("{error:?}"))],
            ),
        }
    }
}

/// One mouse event, on the hook thread: a left click clears the snippet buffer (the Mac clears it
/// on a click), and the wheel decides about the buttons it takes. Only the wheel swallows.
fn mouse_event(event: &MouseEvent) -> HookDecision {
    let wants = MOUSE_WANTS.load(Ordering::Relaxed);
    if wants & MOUSE_SNIPPETS != 0 && matches!(event.kind, MouseKind::LeftDown | MouseKind::LeftUp)
    {
        services::snippets_clicked();
    }
    if wants & MOUSE_WHEEL != 0 && services::wheel_mouse_event(event) {
        return HookDecision::Swallow;
    }
    HookDecision::Pass
}

/// Whether the screenshot thread is running, so a capture asked for now will start.
pub fn screenshots_on() -> bool {
    SCREENSHOT_KEYS.load(Ordering::Relaxed)
}

/// Makes the layer match the two settings now, without a restart: the hook is installed when
/// either feature is on and removed when both are off, and the screenshot thread runs only
/// while screenshot shortcuts are on.
pub fn apply(mac_shortcuts: bool, screenshot_keys: bool) {
    let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if screenshot_keys && running.shots.is_none() {
        running.shots = services::screenshot_start();
    }
    let shots_on = screenshot_keys && running.shots.is_some();
    MAC_SHORTCUTS.store(mac_shortcuts, Ordering::Relaxed);
    SCREENSHOT_KEYS.store(shots_on, Ordering::Relaxed);
    if !shots_on && running.shots.is_some() {
        // Hook flag is already off; no new request can arrive. Ends any open session too.
        running.shots = None;
    }
    let want_hook = mac_shortcuts
        || shots_on
        || LAUNCHER_HOOK.load(Ordering::Relaxed)
        || SNIPPETS_ON.load(Ordering::Relaxed);
    if want_hook && running.hook.is_none() {
        running.hook = start_hook();
        if running.hook.is_none() {
            services::log_info("keys_hook_unavailable", &[]);
        }
    } else if !want_hook {
        running.hook = None;
    }
}

fn start_hook() -> Option<HookThread> {
    // Keys already held when the hook goes in are not seen going down: start from the system's
    // view, which is the physical one until this program injects something.
    let held = ModifierSnapshot::capture();
    for (at, flag) in SIDES.iter().enumerate() {
        flag.store(
            held.contains((VK_LSHIFT as usize + at) as u16),
            Ordering::Relaxed,
        );
    }
    match LowLevelHooks::install(HookConfig::new().keyboard(hook_event)) {
        Ok(hooks) => {
            HOOK_UP.store(true, Ordering::Release);
            Some(HookThread(hooks))
        }
        Err(error) => {
            services::log_info("keys_hook_install", &[("error", &format!("{error:?}"))]);
            None
        }
    }
}

/// One keyboard event, on the hook thread.
fn hook_event(event: &KeyEventDetail) -> HookDecision {
    if handle(event, event.down) {
        HookDecision::Swallow
    } else {
        HookDecision::Pass
    }
}

enum Plan {
    /// Inject Ctrl+<key>.
    CtrlKey(u32),
    /// Inject Ctrl+<key> and leave Shift as the user holds it (Ctrl+Shift+<key> with it down).
    CtrlKeepShift(u32),
    /// Inject a terminal's own copy or paste chord (`terminal_chord`) for Alt+C / Alt+V.
    TerminalChord(u32, u32),
    Screenshot(ScreenshotRequest),
}

fn modifier_state(vk: u32) -> Option<(&'static AtomicBool, bool)> {
    match vk {
        VK_LCONTROL | VK_RCONTROL | VK_CONTROL => Some((&CTRL_DOWN, false)),
        VK_LSHIFT | VK_RSHIFT | VK_SHIFT => Some((&SHIFT_DOWN, true)),
        VK_LWIN | VK_RWIN => Some((&WIN_DOWN, false)),
        _ => None,
    }
}

/// True to swallow the event.
fn handle(event: &KeyEventDetail, down: bool) -> bool {
    if event.injected || event.extra_info == INJECT_TAG {
        return false;
    }
    let vk = event.vk;
    if vk >= 256 {
        return false;
    }
    if (VK_LSHIFT..=VK_RMENU).contains(&vk) {
        SIDES[(vk - VK_LSHIFT) as usize].store(down, Ordering::Relaxed);
    }
    // Alt: remember which physical key to restore; never swallowed.
    if matches!(vk, VK_LMENU | VK_RMENU | VK_MENU) {
        if down && vk != VK_MENU {
            ALT_VK.store(vk, Ordering::Relaxed);
        }
        return false;
    }
    if let Some((flag, is_shift)) = modifier_state(vk) {
        flag.store(down, Ordering::Relaxed);
        if down && is_shift && vk != VK_SHIFT {
            SHIFT_VK.store(vk, Ordering::Relaxed);
        }
        return false;
    }
    // Snippet keywords see each physical key press that is not a modifier (never swallowed).
    if down && SNIPPETS_ON.load(Ordering::Relaxed) {
        services::snippets_key_down(vk, event.scan_code);
    }
    let swallowed = &SWALLOWED[vk as usize];
    // Launcher type-ahead (#378): a plain character typed between the chord and the launcher
    // owning the keyboard is held for it (its release and repeats are swallowed with it).
    if down
        && !event.alt_down
        && !CTRL_DOWN.load(Ordering::Relaxed)
        && !WIN_DOWN.load(Ordering::Relaxed)
        && CAPTURE_UNTIL.load(Ordering::Acquire) > capture_now_ms()
        && let Some(text) = capture_text(vk, event.scan_code)
    {
        CAPTURED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_str(&text);
        CAPTURED_VKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(vk);
        swallowed.store(true, Ordering::Relaxed);
        return true;
    }
    if !down {
        if vk == VK_V {
            services::burst_v_up();
        }
        return swallowed.swap(false, Ordering::Relaxed);
    }

    // Screenshot session: Esc cancels, Space switches region/window mode.
    if (vk == VK_ESCAPE || vk == VK_SPACE) && services::screenshot_active() {
        if !swallowed.swap(true, Ordering::Relaxed) {
            if vk == VK_ESCAPE {
                services::screenshot_key_escape();
            } else {
                services::screenshot_key_space();
            }
        }
        return true;
    }

    let alt = event.alt_down;
    // The copy burst (#387): a plain Ctrl+V while one is up is counted, with the clipboard already
    // holding the item it should paste, before the key reaches the app. It is never swallowed
    // except for the repeats of a held V.
    if vk == VK_V
        && !alt
        && CTRL_DOWN.load(Ordering::Relaxed)
        && !SHIFT_DOWN.load(Ordering::Relaxed)
        && !WIN_DOWN.load(Ordering::Relaxed)
        && services::burst_active()
        && !services::strip_is_foreground()
    {
        return services::burst_paste_key();
    }
    if vk == VK_V {
        // Every V press, before any early return: which check lets Alt+V through untouched.
        let flag = |b: bool| if b { "1" } else { "0" };
        let class = foreground_class();
        services::log_info(
            "keys_v_press",
            &[
                ("alt", flag(alt)),
                ("ctrl", flag(CTRL_DOWN.load(Ordering::Relaxed))),
                ("win", flag(WIN_DOWN.load(Ordering::Relaxed))),
                ("altVk", &format!("{:#x}", ALT_VK.load(Ordering::Relaxed))),
                ("mac", flag(MAC_SHORTCUTS.load(Ordering::Relaxed))),
                ("class", &class),
            ],
        );
    }
    let repeat = swallowed.load(Ordering::Relaxed);
    if !alt || CTRL_DOWN.load(Ordering::Relaxed) || WIN_DOWN.load(Ordering::Relaxed) {
        // Alt was released while a swallowed key is still auto-repeating: keep it away
        // from the application.
        return repeat;
    }
    let shift = SHIFT_DOWN.load(Ordering::Relaxed);
    // The editing remap is the left Alt's only (the thumb key); the right Alt keeps its
    // own meaning (menus, and AltGr on layouts that have it).
    let mac = MAC_SHORTCUTS.load(Ordering::Relaxed) && ALT_VK.load(Ordering::Relaxed) == VK_LMENU;
    // The editing chords stand down in a terminal and in a full-screen program (see
    // `alt_remap_guarded`); only the letters they act on are checked, so other keys cost nothing.
    let guarded =
        mac && matches!(
            vk,
            VK_A | VK_C | VK_V | VK_X | VK_Z | VK_S | VK_F | VK_T | VK_W
        ) && alt_remap_guarded();
    // In a terminal Alt+C and Alt+V still copy and paste, as Cmd+C and Cmd+V do in a Mac
    // terminal, with the terminal's own chord, never Ctrl+C (its interrupt).
    let terminal_copy_paste =
        guarded && !shift && matches!(vk, VK_C | VK_V) && services::switch_on("altAsCtrlGuards");
    let terminal_chord = if terminal_copy_paste {
        foreground_terminal().map(|class| terminal_chord(&class, vk))
    } else {
        None
    };
    if vk == VK_V && !repeat {
        // One line per Alt+V press (no content), so a paste that does nothing can be traced.
        let route = match terminal_chord {
            Some(_) => "terminal",
            None if guarded => "stood-down",
            None if mac => "ctrl",
            None => "off",
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
    let mac = mac && !guarded;
    let shots = SCREENSHOT_KEYS.load(Ordering::Relaxed);
    let plan = match vk {
        VK_C | VK_V if terminal_chord.is_some() => {
            terminal_chord.map(|(modifier, key)| Plan::TerminalChord(modifier, key))
        }
        VK_A | VK_C | VK_V | VK_X if mac && !shift => Some(Plan::CtrlKey(vk)),
        VK_Z if mac && !shift => Some(Plan::CtrlKey(VK_Z)),
        VK_Z if mac => Some(Plan::CtrlKey(VK_Y)),
        VK_S | VK_F | VK_T | VK_W if mac && services::switch_on("altRemapSaveFindTabClose") => {
            Some(Plan::CtrlKeepShift(vk))
        }
        VK_4 | VK_5 if shots && shift && services::strip_is_foreground() => None,
        VK_4 if shots && shift => Some(Plan::Screenshot(ScreenshotRequest::Region)),
        VK_5 if shots && shift => Some(Plan::Screenshot(ScreenshotRequest::Toolbar)),
        _ => None,
    };
    // Alt+V pasting too (as Ctrl+V or a terminal's chord) is a burst paste: counted first.
    let paste_chord = vk == VK_V
        && matches!(plan, Some(Plan::CtrlKey(_) | Plan::TerminalChord(..)))
        && !shift
        && services::burst_active();
    if paste_chord && services::burst_paste_key() {
        swallowed.store(true, Ordering::Relaxed);
        return true;
    }
    match plan {
        Some(Plan::CtrlKey(target)) => {
            swallowed.store(true, Ordering::Relaxed);
            send_ctrl_chord(target, shift);
            true
        }
        Some(Plan::CtrlKeepShift(target)) => {
            swallowed.store(true, Ordering::Relaxed);
            send_ctrl_chord(target, false);
            true
        }
        Some(Plan::TerminalChord(modifier, target)) => {
            swallowed.store(true, Ordering::Relaxed);
            send_terminal_chord(modifier, target);
            true
        }
        Some(Plan::Screenshot(request)) => {
            if !swallowed.swap(true, Ordering::Relaxed) {
                // Alt+Shift pressed with a key in between: no menu, no layout switch.
                send(&[key(VK_DUMMY, false), key(VK_DUMMY, true)]);
                services::screenshot_request(request);
            }
            true
        }
        None => {
            let claimed = EXTRA
                .get()
                .is_some_and(|handler| handler(Chord { vk, shift, repeat }));
            if claimed {
                swallowed.store(true, Ordering::Relaxed);
                if !repeat {
                    send(&[key(VK_DUMMY, false), key(VK_DUMMY, true)]);
                }
            }
            claimed || repeat
        }
    }
}

/// Whether Alt-as-Ctrl stands down for the foreground window: a terminal (Ctrl+C is its
/// interrupt, and Alt+letter belongs to the shell there), or a borderless window that covers its
/// whole monitor, such as a full-screen program or game. Off with `altAsCtrlGuards`. Runs on the
/// hook thread for each editing chord, so it only makes a few fast window queries.
fn alt_remap_guarded() -> bool {
    if !services::switch_on("altAsCtrlGuards") {
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
    let alt = ALT_VK.load(Ordering::Relaxed);
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

/// Sends `events` as one batch, tagged so the key hook passes them; `ctx` names the sender in the
/// log when Windows takes fewer than all of them.
pub fn send_input(events: &[RawInput], ctx: &str) {
    if send_input_batch_with(events, &SendOptions::tagged(INJECT_TAG)).is_err() {
        services::log_info("send_input_failed", &[("context", ctx)]);
    }
}

fn send(events: &[RawInput]) {
    send_input(events, "keys");
}

/// Ctrl+V with no key held: the launcher pastes a snippet into the app it gave the
/// foreground back to.
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
    let alt = ALT_VK.load(Ordering::Relaxed);
    let shift_vk = SHIFT_VK.load(Ordering::Relaxed);
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

/// Which screenshot picker an Alt+Shift+4 or Alt+Shift+5 press asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenshotRequest {
    /// The region picker.
    Region,
    /// The toolbar.
    Toolbar,
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
