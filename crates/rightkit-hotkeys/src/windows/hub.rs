//! One process-wide keyboard hook and one mouse hook shared by every feature that needs them (the
//! hook hub). Windows allows a process one `WH_KEYBOARD_LL` and one `WH_MOUSE_LL` slot here
//! ([`LowLevelHooks`]), so features register ordered handlers with the hub instead of installing
//! hooks of their own: a shortcut layer, a screenshot chord, snippet keywords, a type-ahead
//! capture, a mouse wheel menu.
//!
//! The keyboard hook runs while at least one key handler is registered, the mouse hook while at
//! least one mouse handler is; each goes away with its last handler. Handlers run on the hook
//! thread, in ascending `order` (equal orders in registration order), and must return at once.
//!
//! What the hub decides itself:
//! - Injected key events, and any event carrying the hub's own tag ([`own_tag`], put on every
//!   event a handler injects), pass untouched and reach no handler, so injected keys never loop
//!   back. Injected mouse events from other programs do reach the mouse handlers.
//! - Modifier keys (Shift, Ctrl, Alt, Windows) are tracked, never swallowed and never handed to a
//!   handler; [`KeyContext`] carries their state with each other key, and [`physically_down`] tells
//!   which side the user holds.
//! - A key-down a handler swallowed has its auto-repeats and its release swallowed too. Each repeat
//!   is offered to the handlers again with [`KeyContext::repeat`] set; when none claims it, it is
//!   still swallowed. Releases are offered to every handler for their own bookkeeping; what a
//!   handler answers for a release is ignored.
//! - The first handler that answers [`HookDecision::Swallow`] for a key-down or a mouse event
//!   stops it; later handlers do not see it.
//!
//! ```no_run
//! use rightkit_hotkeys::windows::hub;
//! use rightkit_hotkeys::windows::HookDecision;
//!
//! // Alt+Shift+4 asks for a screenshot; the key never reaches the program in front.
//! let id = hub::add_key_handler(10, |event, cx| {
//!     if event.down && event.vk == 0x34 && event.alt_down && cx.shift && !cx.ctrl {
//!         if !cx.repeat {
//!             // start the region picker here
//!         }
//!         HookDecision::Swallow
//!     } else {
//!         HookDecision::Pass
//!     }
//! });
//! // ...
//! hub::remove(id);
//! ```

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use super::{HookConfig, HookDecision, HookError, LowLevelHooks};
use crate::hook_detail::{KeyEventDetail, MouseEvent};

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

/// `dwExtraInfo` the hub ignores by default: "RKHB".
const DEFAULT_TAG: usize = 0x524B_4842;

/// The modifier state the hub keeps, handed to each key handler with the event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyContext {
    /// A Ctrl key is held.
    pub ctrl: bool,
    /// A Shift key is held.
    pub shift: bool,
    /// A Windows key is held.
    pub win: bool,
    /// The last Alt key pressed (`VK_LMENU` or `VK_RMENU`): the side the user holds.
    pub alt_vk: u32,
    /// The last Shift key pressed (`VK_LSHIFT` or `VK_RSHIFT`).
    pub shift_vk: u32,
    /// This key-down repeats a press that was swallowed (the key is held).
    pub repeat: bool,
}

/// A key handler: the event and the hub's modifier state; `Swallow` stops a key-down.
pub type KeyHandler = Arc<dyn Fn(&KeyEventDetail, &KeyContext) -> HookDecision + Send + Sync>;
/// A mouse handler; `Swallow` stops the event.
pub type MouseHandler = Arc<dyn Fn(&MouseEvent) -> HookDecision + Send + Sync>;

/// A registration, for [`remove`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HandlerId(u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static TAG: AtomicU64 = AtomicU64::new(DEFAULT_TAG as u64);

static KEY_HANDLERS: RwLock<Vec<(i32, u64, KeyHandler)>> = RwLock::new(Vec::new());
static MOUSE_HANDLERS: RwLock<Vec<(i32, u64, MouseHandler)>> = RwLock::new(Vec::new());

/// The installed hooks (each only while it has handlers).
struct Hooks {
    keyboard: Option<LowLevelHooks>,
    mouse: Option<LowLevelHooks>,
}

static HOOKS: Mutex<Hooks> = Mutex::new(Hooks {
    keyboard: None,
    mouse: None,
});

static KEYBOARD_UP: AtomicBool = AtomicBool::new(false);
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static ALT_VK: AtomicU32 = AtomicU32::new(VK_LMENU);
static SHIFT_VK: AtomicU32 = AtomicU32::new(VK_LSHIFT);
/// Physical state of `VK_LSHIFT..=VK_RMENU`, as the hook saw it.
static SIDES: [AtomicBool; 6] = [const { AtomicBool::new(false) }; 6];
/// Keys whose press was swallowed and whose release (and auto-repeat) is swallowed too.
static SWALLOWED: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];

/// The `dwExtraInfo` tag of events the hub lets through untouched: handlers put it on every event
/// they inject (`SendInput`), so their own keys never come back to them.
pub fn own_tag() -> usize {
    TAG.load(Ordering::Relaxed) as usize
}

/// Sets the tag of [`own_tag`] (for an application that already tags its injected input with its
/// own marker). Call before the first handler is registered.
pub fn set_own_tag(tag: usize) {
    TAG.store(tag as u64, Ordering::Relaxed);
}

/// Registers a key handler at `order` (lower runs first) and installs the keyboard hook when it is
/// the first. Call from any thread but the hook thread (never from inside a handler).
pub fn add_key_handler(
    order: i32,
    handler: impl Fn(&KeyEventDetail, &KeyContext) -> HookDecision + Send + Sync + 'static,
) -> HandlerId {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut list = KEY_HANDLERS.write().unwrap_or_else(PoisonError::into_inner);
        list.push((order, id, Arc::new(handler)));
        list.sort_by_key(|(order, id, _)| (*order, *id));
    }
    sync_hooks();
    HandlerId(id)
}

/// Registers a mouse handler at `order` (lower runs first) and installs the mouse hook when it is
/// the first. Call from any thread but the hook thread.
pub fn add_mouse_handler(
    order: i32,
    handler: impl Fn(&MouseEvent) -> HookDecision + Send + Sync + 'static,
) -> HandlerId {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut list = MOUSE_HANDLERS
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        list.push((order, id, Arc::new(handler)));
        list.sort_by_key(|(order, id, _)| (*order, *id));
    }
    sync_hooks();
    HandlerId(id)
}

/// Removes a handler; a hook with no handler left is taken down. Unknown ids are ignored. Call from
/// any thread but the hook thread.
pub fn remove(id: HandlerId) {
    KEY_HANDLERS
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|(_, each, _)| *each != id.0);
    MOUSE_HANDLERS
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|(_, each, _)| *each != id.0);
    sync_hooks();
}

/// Whether the keyboard hook is running.
pub fn keyboard_running() -> bool {
    KEYBOARD_UP.load(Ordering::Acquire)
}

/// Whether the user physically holds the side-specific modifier `vk` (`VK_LSHIFT` to `VK_RMENU`),
/// ignoring what was injected. `None` while the keyboard hook is not running or for another key.
pub fn physically_down(vk: u32) -> Option<bool> {
    if !keyboard_running() || !(VK_LSHIFT..=VK_RMENU).contains(&vk) {
        return None;
    }
    Some(SIDES[(vk - VK_LSHIFT) as usize].load(Ordering::Relaxed))
}

/// Puts the installed hooks in again. Windows removes a low-level hook that stalled, and a hook
/// does not survive sleep and wake, a locked session coming back or an Explorer restart without
/// being told; call this on resume, unlock and `TaskbarCreated`, never from a handler. Returns which
/// hooks were redone (keyboard, mouse) and the first install error.
pub fn reinstall() -> (bool, bool, Option<HookError>) {
    let mut hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut first_error = None;
    let keyboard = hooks.keyboard.take().is_some();
    if keyboard {
        KEYBOARD_UP.store(false, Ordering::Release);
        match install_keyboard() {
            Ok(installed) => hooks.keyboard = Some(installed),
            Err(error) => first_error = Some(error),
        }
    }
    let mouse = hooks.mouse.take().is_some();
    if mouse {
        match LowLevelHooks::install(HookConfig::new().mouse(mouse_event)) {
            Ok(installed) => hooks.mouse = Some(installed),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    (keyboard, mouse, first_error)
}

/// The last error installing a hook, kept for diagnostics until the next successful install.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// The last hook install failure, as text, if the most recent attempt failed.
pub fn last_install_error() -> Option<String> {
    LAST_ERROR
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn sync_hooks() {
    let want_keyboard = !KEY_HANDLERS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .is_empty();
    let want_mouse = !MOUSE_HANDLERS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .is_empty();
    let mut hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut error = None;
    if want_keyboard && hooks.keyboard.is_none() {
        match install_keyboard() {
            Ok(installed) => hooks.keyboard = Some(installed),
            Err(e) => error = Some(format!("keyboard: {e:?}")),
        }
    } else if !want_keyboard && hooks.keyboard.is_some() {
        hooks.keyboard = None;
        KEYBOARD_UP.store(false, Ordering::Release);
    }
    if want_mouse && hooks.mouse.is_none() {
        match LowLevelHooks::install(HookConfig::new().mouse(mouse_event)) {
            Ok(installed) => hooks.mouse = Some(installed),
            Err(e) => {
                error.get_or_insert(format!("mouse: {e:?}"));
            }
        }
    } else if !want_mouse {
        hooks.mouse = None;
    }
    *LAST_ERROR.lock().unwrap_or_else(PoisonError::into_inner) = error;
}

fn install_keyboard() -> Result<LowLevelHooks, HookError> {
    // Keys already held when the hook goes in are not seen going down: start from the system's
    // view, which is the physical one until something is injected.
    for (at, flag) in SIDES.iter().enumerate() {
        // SAFETY: a plain key-state query with no pointers.
        let state = unsafe { GetAsyncKeyState((VK_LSHIFT as usize + at) as i32) };
        flag.store((state as u16 & 0x8000) != 0, Ordering::Relaxed);
    }
    let hooks = LowLevelHooks::install(HookConfig::new().keyboard(key_event))?;
    KEYBOARD_UP.store(true, Ordering::Release);
    Ok(hooks)
}

fn modifier_flag(vk: u32) -> Option<(&'static AtomicBool, bool)> {
    match vk {
        VK_LCONTROL | VK_RCONTROL | VK_CONTROL => Some((&CTRL_DOWN, false)),
        VK_LSHIFT | VK_RSHIFT | VK_SHIFT => Some((&SHIFT_DOWN, true)),
        VK_LWIN | VK_RWIN => Some((&WIN_DOWN, false)),
        _ => None,
    }
}

/// One keyboard event, on the hook thread.
fn key_event(event: &KeyEventDetail) -> HookDecision {
    if event.injected || event.extra_info == own_tag() {
        return HookDecision::Pass;
    }
    let vk = event.vk;
    if vk >= 256 {
        return HookDecision::Pass;
    }
    let down = event.down;
    if (VK_LSHIFT..=VK_RMENU).contains(&vk) {
        SIDES[(vk - VK_LSHIFT) as usize].store(down, Ordering::Relaxed);
    }
    if matches!(vk, VK_LMENU | VK_RMENU | VK_MENU) {
        if down && vk != VK_MENU {
            ALT_VK.store(vk, Ordering::Relaxed);
        }
        return HookDecision::Pass;
    }
    if let Some((flag, is_shift)) = modifier_flag(vk) {
        flag.store(down, Ordering::Relaxed);
        if down && is_shift && vk != VK_SHIFT {
            SHIFT_VK.store(vk, Ordering::Relaxed);
        }
        return HookDecision::Pass;
    }
    let swallowed = &SWALLOWED[vk as usize];
    let context = KeyContext {
        ctrl: CTRL_DOWN.load(Ordering::Relaxed),
        shift: SHIFT_DOWN.load(Ordering::Relaxed),
        win: WIN_DOWN.load(Ordering::Relaxed),
        alt_vk: ALT_VK.load(Ordering::Relaxed),
        shift_vk: SHIFT_VK.load(Ordering::Relaxed),
        repeat: down && swallowed.load(Ordering::Relaxed),
    };
    // Held for the walk: handlers never register or remove from the hook thread.
    let handlers = KEY_HANDLERS.read().unwrap_or_else(PoisonError::into_inner);
    if !down {
        for (_, _, handler) in handlers.iter() {
            let _ = handler(event, &context);
        }
        return if swallowed.swap(false, Ordering::Relaxed) {
            HookDecision::Swallow
        } else {
            HookDecision::Pass
        };
    }
    for (_, _, handler) in handlers.iter() {
        if handler(event, &context) == HookDecision::Swallow {
            swallowed.store(true, Ordering::Relaxed);
            return HookDecision::Swallow;
        }
    }
    if context.repeat {
        HookDecision::Swallow
    } else {
        HookDecision::Pass
    }
}

/// Ends the swallowing of `vk`'s release and repeats early (for a handler that held keys for a
/// while and now lets them reach the application again).
pub fn release_swallowed(vk: u32) {
    if let Some(flag) = SWALLOWED.get(vk as usize) {
        flag.store(false, Ordering::Relaxed);
    }
}

/// One mouse event, on the hook thread.
fn mouse_event(event: &MouseEvent) -> HookDecision {
    // Only the hub's own tag: other injected mouse input (remote control tools) reaches handlers.
    if event.extra_info == own_tag() {
        return HookDecision::Pass;
    }
    let handlers = MOUSE_HANDLERS
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    for (_, _, handler) in handlers.iter() {
        if handler(event) == HookDecision::Swallow {
            return HookDecision::Swallow;
        }
    }
    HookDecision::Pass
}
