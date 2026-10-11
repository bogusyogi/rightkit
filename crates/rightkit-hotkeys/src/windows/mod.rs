//! Windows primitives: a low-level keyboard hook and `RegisterHotKey` registration.
//!
//! This module is compiled only on Windows and has not been built on the macOS authoring host (no
//! Windows target is installed); it is covered by the cross-platform contract in `API.md` and must be
//! verified by the Windows agent (`win` host) before an app adopts it.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use windows_sys::Win32::Foundation::{GetLastError, HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KillTimer, PostThreadMessageW, SetTimer, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYDOWN,
    WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
};

use crate::chord::Chord;
use crate::hold::{HoldTracker, KeyStateSource};
use crate::hook_chord::{AlreadyBound, HookChords};
use crate::register_status::RegisterFailure;
use crate::registration::{modifier_bits, HotkeyApi, Registrations, UnregisterFailure};

pub use crate::hold::HotkeyState;

/// Poll period for release detection. Thread timers tick at the system clock resolution
/// (about 15.6 ms by default), so a release is reported within roughly 15 to 30 ms of the key-up.
pub const RELEASE_POLL_MS: u32 = 15;

/// Physical key state from `GetAsyncKeyState` (high bit = down).
struct AsyncKeyState;

impl KeyStateSource for AsyncKeyState {
    fn is_down(&self, vk: u16) -> bool {
        (unsafe { GetAsyncKeyState(vk as i32) } as u16) & 0x8000 != 0
    }
}

// The portable flag values `Registrations` registers with are the ones Windows defines.
const _: () = assert!(
    modifier_bits::ALT == MOD_ALT
        && modifier_bits::CONTROL == MOD_CONTROL
        && modifier_bits::SHIFT == MOD_SHIFT
        && modifier_bits::WIN == MOD_WIN
        && modifier_bits::NOREPEAT == MOD_NOREPEAT
);

/// `RegisterHotKey` and `UnregisterHotKey` for the calling thread (no window).
struct ThreadHotkeys;

impl HotkeyApi for ThreadHotkeys {
    fn register(&mut self, id: i32, modifiers: u32, vk: u32) -> Result<(), u32> {
        // SAFETY: a null window registers for the calling thread; the call has no other
        // preconditions. The error is read immediately after the failing call.
        unsafe {
            if RegisterHotKey(null_mut(), id, modifiers, vk) != 0 {
                Ok(())
            } else {
                Err(GetLastError())
            }
        }
    }

    fn unregister(&mut self, id: i32) -> Result<(), u32> {
        // SAFETY: as for `register`.
        unsafe {
            if UnregisterHotKey(null_mut(), id) != 0 {
                Ok(())
            } else {
                Err(GetLastError())
            }
        }
    }
}

/// `KBDLLHOOKSTRUCT.flags` bit 4: injected by `SendInput`. Synthetic events (command dispatch) must
/// pass through without changing push-to-talk state.
pub const LLKHF_INJECTED: u32 = 0x10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub vk: u32,
    pub down: bool,
    pub injected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookDecision {
    Pass,
    Swallow,
}

type Handler = Arc<dyn Fn(&KeyEvent) -> HookDecision + Send + Sync + 'static>;

static HANDLER: OnceLock<Mutex<Option<Handler>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Handler>> {
    HANDLER.get_or_init(|| Mutex::new(None))
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(lparam as *const KBDLLHOOKSTRUCT);
        let message = wparam as u32;
        let down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
        let up = message == WM_KEYUP || message == WM_SYSKEYUP;
        if down || up {
            let handler = slot().lock().ok().and_then(|g| g.clone());
            if let Some(handler) = handler {
                let event = KeyEvent {
                    vk: info.vkCode,
                    down,
                    injected: info.flags & LLKHF_INJECTED != 0,
                };
                if handler(&event) == HookDecision::Swallow {
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

/// Formats a Win32 error code the way `std` renders OS errors.
fn os_error_message(code: u32) -> String {
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

/// Formats the calling thread's last Win32 error. Call only immediately after a failed Win32 call.
fn last_os_error_message() -> String {
    os_error_message(unsafe { GetLastError() })
}

#[derive(Debug)]
pub enum HookError {
    AlreadyInstalled,
    Install(String),
}

/// Process-wide `WH_KEYBOARD_LL` hook on its own message-loop thread. The handler runs inside the
/// hook procedure: it must be fast (Windows enforces a system-wide responsiveness budget), so do the
/// real work on another thread.
pub struct LowLevelKeyboardHook {
    thread_id: u32,
    thread: Option<thread::JoinHandle<()>>,
}

impl LowLevelKeyboardHook {
    pub fn install(
        handler: impl Fn(&KeyEvent) -> HookDecision + Send + Sync + 'static,
    ) -> Result<Self, HookError> {
        {
            let mut guard = slot().lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_some() {
                return Err(HookError::AlreadyInstalled);
            }
            *guard = Some(Arc::new(handler));
        }
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let thread = thread::Builder::new()
            .name("rightkit-ll-keyboard-hook".into())
            .spawn(move || unsafe {
                // A null module handle is passed through unchanged, matching the previous `None` fallback.
                let module: HINSTANCE = GetModuleHandleW(null());
                let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0);
                if hook.is_null() {
                    let _ = tx.send(Err(last_os_error_message()));
                } else {
                    let _ = tx.send(Ok(GetCurrentThreadId()));
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, null_mut(), 0, 0) != 0 {}
                    let _ = UnhookWindowsHookEx(hook);
                }
            })
            .map_err(|e| HookError::Install(e.to_string()))?;
        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                *slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
                let _ = thread.join();
                Err(HookError::Install(e))
            }
            Err(_) => Err(HookError::Install("hook thread exited early".into())),
        }
    }
}

impl Drop for LowLevelKeyboardHook {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        *slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegisterError {
    /// `ERROR_HOTKEY_ALREADY_REGISTERED` (1409).
    Conflict {
        chord: String,
    },
    /// `ERROR_INVALID_PARAMETER` (87) or `ERROR_INVALID_FLAGS` (1004): the chord cannot be a
    /// system hotkey.
    Unsupported {
        chord: String,
    },
    /// `ERROR_ACCESS_DENIED` (5).
    PermissionDenied {
        chord: String,
        message: String,
    },
    /// `ERROR_NOT_ENOUGH_MEMORY` (8), `ERROR_OUTOFMEMORY` (14) or `ERROR_NO_SYSTEM_RESOURCES` (1450).
    ResourceExhausted {
        chord: String,
        message: String,
    },
    Os {
        chord: String,
        message: String,
    },
}

impl RegisterError {
    /// Map a `GetLastError` value from a failed `RegisterHotKey` (see [`RegisterFailure`]).
    pub fn from_win32(chord: String, code: u32) -> Self {
        let message = os_error_message(code);
        match RegisterFailure::from_win32(code) {
            RegisterFailure::Conflict => RegisterError::Conflict { chord },
            RegisterFailure::Unsupported => RegisterError::Unsupported { chord },
            RegisterFailure::PermissionDenied => RegisterError::PermissionDenied { chord, message },
            RegisterFailure::ResourceExhausted => {
                RegisterError::ResourceExhausted { chord, message }
            }
            RegisterFailure::Other => RegisterError::Os { chord, message },
        }
    }

    pub fn message(&self) -> String {
        match self {
            RegisterError::Conflict { chord } => {
                format!("register {chord} failed: shortcut already registered")
            }
            RegisterError::Unsupported { chord } => {
                format!("register {chord} failed: chord not expressible as a system hotkey")
            }
            RegisterError::PermissionDenied { chord, message }
            | RegisterError::ResourceExhausted { chord, message }
            | RegisterError::Os { chord, message } => {
                format!("register {chord} failed: {message}")
            }
        }
    }
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for RegisterError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnregisterError {
    /// No registration for this chord exists on this instance (or `ERROR_HOTKEY_NOT_REGISTERED`).
    NotRegistered {
        chord: String,
    },
    Os {
        chord: String,
        message: String,
    },
}

impl std::fmt::Display for UnregisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnregisterError::NotRegistered { chord } => {
                write!(f, "unregister {chord} failed: not registered")
            }
            UnregisterError::Os { chord, message } => {
                write!(f, "unregister {chord} failed: {message}")
            }
        }
    }
}

impl std::error::Error for UnregisterError {}

enum Command {
    /// The chord, whether auto-repeat is delivered, and where the result goes.
    Register(Chord, bool, mpsc::Sender<Result<i32, RegisterError>>),
    Unregister(Chord, mpsc::Sender<Result<i32, UnregisterError>>),
}

/// Thread-bound `RegisterHotKey` registrations: the registering thread owns a message loop and
/// receives `WM_HOTKEY`. Dropping unregisters everything and ends the loop.
///
/// # Key-up (hold-to-talk)
///
/// `RegisterHotKey` posts `WM_HOTKEY` on press only; Windows never reports the release. With
/// [`GlobalHotkeys::with_state`], after a `WM_HOTKEY` the loop thread starts a thread timer
/// ([`RELEASE_POLL_MS`]) that polls `GetAsyncKeyState` for the chord's non-modifier key and
/// required modifiers. The first poll that finds the chord no longer held delivers `Released`,
/// and when no chord is held the timer is killed. **No global hook is ever installed**, and
/// nothing runs while nothing is held.
///
/// Guarantees: exactly one `Released` per `Pressed` (a repeated `WM_HOTKEY` for a held id is
/// ignored); `Released` is also delivered when the chord is unregistered while held and when the
/// instance is dropped while held.
///
/// Limits:
/// * Release latency is the timer period, about 15 to 30 ms. A tap shorter than one period still
///   yields `Pressed` then `Released`, in order.
/// * The chord counts as released as soon as the key **or any required modifier** goes up.
/// * `GetAsyncKeyState` is system-wide and unaffected by focus changes, so a focus change alone does
///   not end a hold. When the input desktop changes (lock screen, UAC secure desktop, fast user
///   switch) Windows reports every key up to this process, so the next poll delivers `Released`.
///   A key released while a different desktop is active is therefore reported at the switch.
/// * Input injected with `SendInput` that holds the key down counts as held.
/// * If the poll timer cannot be created, `Released` is delivered immediately after `Pressed`
///   rather than leaving the hold unterminated.
pub struct GlobalHotkeys {
    thread_id: u32,
    thread: Option<thread::JoinHandle<()>>,
    cmd: mpsc::Sender<Command>,
    stop: Arc<AtomicBool>,
}

impl GlobalHotkeys {
    /// Press-only delivery: `on_hotkey(id)` on each `WM_HOTKEY`. Starts no timer.
    pub fn new(on_hotkey: impl Fn(i32) + Send + 'static) -> Result<Self, String> {
        Self::spawn(
            move |id, state| {
                if state == HotkeyState::Pressed {
                    on_hotkey(id);
                }
            },
            false,
        )
    }

    /// Press and release delivery for hold-to-talk. `on_event` runs on the hotkey loop thread, once
    /// with `Pressed` and once with `Released` per hold (see the type docs for the mechanism).
    pub fn with_state(
        on_event: impl Fn(i32, HotkeyState) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn(on_event, true)
    }

    fn spawn(
        on_event: impl Fn(i32, HotkeyState) + Send + 'static,
        track_release: bool,
    ) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (ready_tx, ready_rx) = mpsc::channel::<u32>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = thread::Builder::new()
            .name("rightkit-hotkey-loop".into())
            .spawn(move || unsafe {
                let _ = ready_tx.send(GetCurrentThreadId());
                let mut registrations = Registrations::new();
                let mut os = ThreadHotkeys;
                let mut holds = HoldTracker::new();
                let mut timer: usize = 0;
                let mut msg = MSG::default();
                loop {
                    while let Ok(command) = cmd_rx.try_recv() {
                        match command {
                            Command::Register(chord, repeat, reply) => {
                                let result =
                                    registrations.register(&mut os, &chord, repeat).map_err(
                                        |code| RegisterError::from_win32(chord.canonical(), code),
                                    );
                                let _ = reply.send(result);
                            }
                            Command::Unregister(chord, reply) => {
                                let name = chord.canonical();
                                // A held chord ends before it disappears.
                                if let Some(id) = registrations.id_of(&chord) {
                                    if holds.release(id) {
                                        on_event(id, HotkeyState::Released);
                                        if holds.is_idle() && timer != 0 {
                                            let _ = KillTimer(null_mut(), timer);
                                            timer = 0;
                                        }
                                    }
                                }
                                let result =
                                    registrations
                                        .unregister(&mut os, &chord)
                                        .map_err(|failure| match failure {
                                            UnregisterFailure::NotRegistered => {
                                                UnregisterError::NotRegistered { chord: name }
                                            }
                                            UnregisterFailure::Os(code) => UnregisterError::Os {
                                                chord: name,
                                                message: os_error_message(code),
                                            },
                                        });
                                let _ = reply.send(result);
                            }
                        }
                    }
                    if stop2.load(Ordering::SeqCst) || GetMessageW(&mut msg, null_mut(), 0, 0) == 0
                    {
                        break;
                    }
                    if msg.message == WM_HOTKEY {
                        let id = msg.wParam as i32;
                        if !track_release {
                            on_event(id, HotkeyState::Pressed);
                            continue;
                        }
                        let Some(keys) = registrations.hold_keys(id).cloned() else {
                            continue;
                        };
                        if holds.press(id, keys) {
                            on_event(id, HotkeyState::Pressed);
                            if timer == 0 {
                                timer = SetTimer(null_mut(), 0, RELEASE_POLL_MS, None);
                            }
                            if timer == 0 {
                                // Cannot watch for the release: end the hold now, never leak it.
                                for released in holds.release_all() {
                                    on_event(released, HotkeyState::Released);
                                }
                            }
                        }
                    } else if msg.message == WM_TIMER && timer != 0 && msg.wParam == timer {
                        for released in holds.poll(&AsyncKeyState) {
                            on_event(released, HotkeyState::Released);
                        }
                        if holds.is_idle() {
                            let _ = KillTimer(null_mut(), timer);
                            timer = 0;
                        }
                    }
                }
                // Shutdown: exactly one Released for anything still held, no timer left behind.
                for released in holds.release_all() {
                    on_event(released, HotkeyState::Released);
                }
                if timer != 0 {
                    let _ = KillTimer(null_mut(), timer);
                }
                registrations.clear(&mut os);
            })
            .map_err(|e| e.to_string())?;
        let thread_id = ready_rx.recv().map_err(|e| e.to_string())?;
        Ok(Self {
            thread_id,
            thread: Some(thread),
            cmd: cmd_tx,
            stop,
        })
    }

    fn wake(&self) {
        // Wake the loop so it drains the command queue.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, 0x0400, 0, 0);
        }
    }

    /// Register `chord` without auto-repeat: one `WM_HOTKEY` per press, however long it is held.
    ///
    /// A chord Windows or another program already owns fails with [`RegisterError::Conflict`]
    /// (`ERROR_HOTKEY_ALREADY_REGISTERED`, 1409); an app that still needs it can bind it through
    /// [`HookHotkeys`]. To change a shortcut at run time, [`unregister`](Self::unregister) the old
    /// chord and register the new one: no restart is needed.
    pub fn register(&self, chord: &Chord) -> Result<i32, RegisterError> {
        self.register_inner(chord, false)
    }

    /// Register `chord` with auto-repeat: while it is held, Windows posts `WM_HOTKEY` at the
    /// keyboard repeat rate (for a nudge or step action). Only press-only delivery
    /// ([`GlobalHotkeys::new`]) repeats; with [`GlobalHotkeys::with_state`] a hold is one
    /// `Pressed` and one `Released` either way.
    pub fn register_repeating(&self, chord: &Chord) -> Result<i32, RegisterError> {
        self.register_inner(chord, true)
    }

    fn register_inner(&self, chord: &Chord, repeat: bool) -> Result<i32, RegisterError> {
        let (tx, rx) = mpsc::channel();
        self.cmd
            .send(Command::Register(chord.clone(), repeat, tx))
            .map_err(|_| RegisterError::Os {
                chord: chord.canonical(),
                message: "loop stopped".into(),
            })?;
        self.wake();
        rx.recv().unwrap_or_else(|_| {
            Err(RegisterError::Os {
                chord: chord.canonical(),
                message: "loop stopped".into(),
            })
        })
    }

    /// Unregister the registration made for `chord` (matched by canonical spelling, so modifier
    /// order and aliases do not matter). Returns the id that was released. A chord held at this
    /// moment receives its `Released` first.
    pub fn unregister(&self, chord: &Chord) -> Result<i32, UnregisterError> {
        let loop_stopped = || UnregisterError::Os {
            chord: chord.canonical(),
            message: "loop stopped".into(),
        };
        let (tx, rx) = mpsc::channel();
        self.cmd
            .send(Command::Unregister(chord.clone(), tx))
            .map_err(|_| loop_stopped())?;
        self.wake();
        rx.recv().unwrap_or_else(|_| Err(loop_stopped()))
    }
}

impl Drop for GlobalHotkeys {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Chords matched through the low-level keyboard hook: the opt-in fallback for a chord that
/// [`GlobalHotkeys::register`] refuses with [`RegisterError::Conflict`] (Alt+Space when another
/// program holds it).
///
/// This installs the process-wide `WH_KEYBOARD_LL` hook, so every key press on the desktop
/// passes through this process while it is installed: use it only for the chords that cannot
/// be registered, and only when the user asked for them. One instance per process (the hook
/// slot is shared with [`LowLevelKeyboardHook`]).
///
/// A matched chord's key is swallowed, so the program that owned the chord no longer sees it.
/// `on_event` runs inside the hook procedure, once with `Pressed` and once with `Released`
/// per hold: it must return quickly (hand the work to another thread), or Windows drops the
/// hook. The matching rules are those of [`HookChords`].
pub struct HookHotkeys {
    hook: Option<LowLevelKeyboardHook>,
    chords: Arc<Mutex<HookChords>>,
    on_event: Arc<dyn Fn(i32, HotkeyState) + Send + Sync + 'static>,
}

impl HookHotkeys {
    pub fn install(
        on_event: impl Fn(i32, HotkeyState) + Send + Sync + 'static,
    ) -> Result<Self, HookError> {
        let chords = Arc::new(Mutex::new(HookChords::new()));
        let on_event: Arc<dyn Fn(i32, HotkeyState) + Send + Sync + 'static> = Arc::new(on_event);
        let (hook_chords, hook_event) = (chords.clone(), on_event.clone());
        let hook = LowLevelKeyboardHook::install(move |event| {
            // The lock is released before the callback runs.
            let outcome = hook_chords
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .on_key(event.vk, event.down, event.injected);
            for (id, state) in &outcome.events {
                hook_event(*id, *state);
            }
            if outcome.swallow {
                HookDecision::Swallow
            } else {
                HookDecision::Pass
            }
        })?;
        Ok(Self {
            hook: Some(hook),
            chords,
            on_event,
        })
    }

    /// Bind `chord` and return its id. Ids are not reused, and they are separate from the ids
    /// of [`GlobalHotkeys`].
    pub fn bind(&self, chord: &Chord) -> Result<i32, AlreadyBound> {
        self.chords
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .bind(chord)
    }

    /// Remove the binding for `chord` and return its id, or `None` when it was not bound. A
    /// chord held at this moment receives its `Released` first.
    pub fn unbind(&self, chord: &Chord) -> Option<i32> {
        let removed = self
            .chords
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unbind(chord);
        let (id, held) = removed?;
        if held {
            (self.on_event)(id, HotkeyState::Released);
        }
        Some(id)
    }
}

impl Drop for HookHotkeys {
    fn drop(&mut self) {
        // Remove the hook first, so no key event races the final releases.
        drop(self.hook.take());
        let held = self
            .chords
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .release_all();
        for id in held {
            (self.on_event)(id, HotkeyState::Released);
        }
    }
}
