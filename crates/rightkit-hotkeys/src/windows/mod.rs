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
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    HC_ACTION, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_QUIT,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// `ERROR_HOTKEY_ALREADY_REGISTERED`.
const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

use crate::chord::{Chord, Modifier};

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
            Ok(Ok(thread_id)) => Ok(Self { thread_id, thread: Some(thread) }),
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
pub enum RegisterError {
    /// `ERROR_HOTKEY_ALREADY_REGISTERED` (1409).
    Conflict { chord: String },
    Os { chord: String, message: String },
}

impl RegisterError {
    pub fn message(&self) -> String {
        match self {
            RegisterError::Conflict { chord } => {
                format!("register {chord} failed: shortcut already registered")
            }
            RegisterError::Os { chord, message } => format!("register {chord} failed: {message}"),
        }
    }
}

/// Thread-bound `RegisterHotKey` registrations: the registering thread owns a message loop and
/// receives `WM_HOTKEY`. Dropping unregisters everything and ends the loop.
pub struct GlobalHotkeys {
    thread_id: u32,
    thread: Option<thread::JoinHandle<()>>,
    cmd: mpsc::Sender<(Chord, mpsc::Sender<Result<i32, RegisterError>>)>,
    stop: Arc<AtomicBool>,
}

impl GlobalHotkeys {
    pub fn new(on_hotkey: impl Fn(i32) + Send + 'static) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<(Chord, mpsc::Sender<Result<i32, RegisterError>>)>();
        let (ready_tx, ready_rx) = mpsc::channel::<u32>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = thread::Builder::new()
            .name("rightkit-hotkey-loop".into())
            .spawn(move || unsafe {
                let _ = ready_tx.send(GetCurrentThreadId());
                let mut next_id = 1i32;
                let mut ids: Vec<i32> = Vec::new();
                let mut msg = MSG::default();
                loop {
                    while let Ok((chord, reply)) = cmd_rx.try_recv() {
                        let mut mods: HOT_KEY_MODIFIERS = MOD_NOREPEAT;
                        for m in &chord.modifiers {
                            mods |= match m {
                                Modifier::Ctrl => MOD_CONTROL,
                                Modifier::Alt => MOD_ALT,
                                Modifier::Shift => MOD_SHIFT,
                                _ => MOD_WIN,
                            };
                        }
                        let id = next_id;
                        let registered =
                            RegisterHotKey(null_mut(), id, mods, chord.key.windows_vk() as u32) != 0;
                        let result = if registered {
                            next_id += 1;
                            ids.push(id);
                            Ok(id)
                        } else {
                            // Read the error immediately after the failing call.
                            let code = GetLastError();
                            if code == ERROR_HOTKEY_ALREADY_REGISTERED {
                                Err(RegisterError::Conflict { chord: chord.canonical() })
                            } else {
                                Err(RegisterError::Os {
                                    chord: chord.canonical(),
                                    message: os_error_message(code),
                                })
                            }
                        };
                        let _ = reply.send(result);
                    }
                    if stop2.load(Ordering::SeqCst) || GetMessageW(&mut msg, null_mut(), 0, 0) == 0 {
                        break;
                    }
                    if msg.message == WM_HOTKEY {
                        on_hotkey(msg.wParam as i32);
                    }
                }
                for id in ids {
                    let _ = UnregisterHotKey(null_mut(), id);
                }
            })
            .map_err(|e| e.to_string())?;
        let thread_id = ready_rx.recv().map_err(|e| e.to_string())?;
        Ok(Self { thread_id, thread: Some(thread), cmd: cmd_tx, stop })
    }

    pub fn register(&self, chord: &Chord) -> Result<i32, RegisterError> {
        let (tx, rx) = mpsc::channel();
        self.cmd
            .send((chord.clone(), tx))
            .map_err(|_| RegisterError::Os { chord: chord.canonical(), message: "loop stopped".into() })?;
        // Wake the loop so it drains the command queue.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, 0x0400, 0, 0);
        }
        rx.recv().unwrap_or_else(|_| {
            Err(RegisterError::Os { chord: chord.canonical(), message: "loop stopped".into() })
        })
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
