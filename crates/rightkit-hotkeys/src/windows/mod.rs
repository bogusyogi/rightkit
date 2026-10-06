//! Windows primitives: a low-level keyboard hook and `RegisterHotKey` registration.
//!
//! This module is compiled only on Windows and has not been built on the macOS authoring host (no
//! Windows target is installed); it is covered by the cross-platform contract in `API.md` and must be
//! verified by the Windows agent (`win` host) before an app adopts it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    HC_ACTION, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_QUIT,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

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
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let message = wparam.0 as u32;
        let down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
        let up = message == WM_KEYUP || message == WM_SYSKEYUP;
        if down || up {
            let handler = slot().lock().ok().and_then(|g| g.clone());
            if let Some(handler) = handler {
                let event = KeyEvent {
                    vk: info.vkCode,
                    down,
                    injected: info.flags.0 & LLKHF_INJECTED != 0,
                };
                if handler(&event) == HookDecision::Swallow {
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
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
                let module = GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).ok();
                match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) {
                    Ok(hook) => {
                        let _ = tx.send(Ok(GetCurrentThreadId()));
                        let mut msg = MSG::default();
                        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
                        let _ = UnhookWindowsHookEx(hook);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                    }
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
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
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
                        let mut mods = MOD_NOREPEAT;
                        for m in &chord.modifiers {
                            mods |= match m {
                                Modifier::Ctrl => MOD_CONTROL,
                                Modifier::Alt => MOD_ALT,
                                Modifier::Shift => MOD_SHIFT,
                                _ => MOD_WIN,
                            };
                        }
                        let id = next_id;
                        let result = match RegisterHotKey(None, id, HOT_KEY_MODIFIERS(mods.0), chord.key.windows_vk() as u32) {
                            Ok(()) => {
                                next_id += 1;
                                ids.push(id);
                                Ok(id)
                            }
                            Err(e) if e.code().0 as u32 & 0xFFFF == 1409 => {
                                Err(RegisterError::Conflict { chord: chord.canonical() })
                            }
                            Err(e) => Err(RegisterError::Os { chord: chord.canonical(), message: e.to_string() }),
                        };
                        let _ = reply.send(result);
                    }
                    if stop2.load(Ordering::SeqCst) || !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        break;
                    }
                    if msg.message == WM_HOTKEY {
                        on_hotkey(msg.wParam.0 as i32);
                    }
                }
                for id in ids {
                    let _ = UnregisterHotKey(None, id);
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
            let _ = PostThreadMessageW(self.thread_id, 0x0400, WPARAM(0), LPARAM(0));
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
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
