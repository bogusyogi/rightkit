//! Detecting the owner's own keyboard and mouse use (AG-12, RK-13).
//!
//! An agent that takes the foreground must stop the moment the owner touches the keyboard or the mouse.
//! [`InputWatch`] is a count that rises with every keyboard or mouse event the owner's hardware made and
//! that the agent's own synthetic events do not raise, where the OS can tell them apart:
//!
//! * macOS: the HID system state counters (`CGEventSourceCounterForEventType`). Events the agent posts to
//!   one process (`CGEventPostToPid`) never reach them. Events posted at the HID tap (the foreground tier)
//!   do, so [`InputWatch::counts_own_events`] is true there and [`OwnerInput`] re-bases after each of the
//!   agent's own events instead of reading them as the owner's.
//! * Windows: a low-level keyboard and mouse hook on a dedicated thread that counts an event only when its
//!   `injected` flag (`LLKHF_INJECTED`, `LLMHF_INJECTED`) is clear. The hook reads one flags word per event
//!   and counts it; it never reads or keeps a key code, a position or a character.
//! * Anywhere else: [`InputWatch::count`] is `None`, which means unknown, never "no input".
//!
//! Off unless the caller turns it on with `Automation::set_input_watch`. Whether an agent may watch
//! the owner's input at all, and whether a low-level hook is acceptable on Windows, is a decision for
//! the owner; this module only provides the mechanism.

use std::cell::Cell;
use std::sync::Arc;

/// A count of the owner's own input events.
pub trait InputWatch: Send + Sync {
    /// Events the owner's own keyboard and mouse have produced since the watch began, or `None` when
    /// this platform or this watch cannot tell. The value only means something compared with an earlier
    /// reading from the same watch.
    fn count(&self) -> Option<u64>;

    /// True when the agent's own foreground events also raise [`InputWatch::count`] (macOS), so a
    /// caller must re-base the count after sending one.
    fn counts_own_events(&self) -> bool {
        false
    }
}

/// One act's view of the owner's input: the count when the act began, and whether the owner used the
/// keyboard or mouse since.
pub struct OwnerInput {
    watch: Option<Arc<dyn InputWatch>>,
    base: Cell<Option<u64>>,
}

impl OwnerInput {
    /// Take the starting count. With no watch, [`OwnerInput::used`] is always false.
    pub fn begin(watch: Option<Arc<dyn InputWatch>>) -> Self {
        let base = watch.as_ref().and_then(|w| w.count());
        Self {
            watch,
            base: Cell::new(base),
        }
    }

    /// True when the count rose since the act began (or since the last [`OwnerInput::own_event_sent`]).
    /// An unknown count is not claimed to be input.
    pub fn used(&self) -> bool {
        match (self.base.get(), self.watch.as_ref().and_then(|w| w.count())) {
            (Some(before), Some(now)) => now != before,
            _ => false,
        }
    }

    /// The agent sent an event of its own: where such events raise the count, take the count again so
    /// they are not read as the owner's.
    pub fn own_event_sent(&self) {
        if let Some(watch) = &self.watch {
            if watch.counts_own_events() {
                self.base.set(watch.count());
            }
        }
    }
}

/// The system's watch: see the module notes. At most one is live per process on Windows (the hooks are
/// process-wide); a second `start` while one lives reports unknown.
pub struct SystemInputWatch {
    live: bool,
    #[cfg(windows)]
    hook: Option<windows_hook::Hook>,
}

impl SystemInputWatch {
    pub fn start() -> Self {
        #[cfg(windows)]
        {
            let hook = windows_hook::Hook::start();
            Self {
                live: hook.is_some(),
                hook,
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                live: cfg!(target_os = "macos"),
            }
        }
    }
}

impl InputWatch for SystemInputWatch {
    fn count(&self) -> Option<u64> {
        if !self.live {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            Some(mac::counter())
        }
        #[cfg(windows)]
        {
            self.hook.as_ref().map(|_| windows_hook::owner_events())
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            None
        }
    }

    fn counts_own_events(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// `state`: CGEventSourceStateID (int32). `event_type`: CGEventType (uint32).
        fn CGEventSourceCounterForEventType(state: i32, event_type: u32) -> u32;
    }

    /// kCGEventSourceStateHIDSystemState.
    const HID_SYSTEM_STATE: i32 = 1;
    /// Mouse buttons down and up, moves and drags, scroll, key down and up, modifier changes.
    const TYPES: [u32; 13] = [1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 22, 25, 26];

    /// The sum of the HID system state counters of the owner's input event types.
    pub fn counter() -> u64 {
        TYPES
            .iter()
            .map(|t| {
                // SAFETY: a counter query with plain integers; no pointers, nothing is read or posted.
                u64::from(unsafe { CGEventSourceCounterForEventType(HID_SYSTEM_STATE, *t) })
            })
            .sum()
    }
}

#[cfg(windows)]
mod windows_hook {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
        MSG, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_QUIT,
    };

    /// `HC_ACTION`: the hook procedure's `code` for an event to process.
    const HC_ACTION: i32 = 0;
    /// `LLKHF_INJECTED` in `KBDLLHOOKSTRUCT::flags`.
    const KEY_INJECTED: u32 = 0x10;
    /// `LLMHF_INJECTED` in `MSLLHOOKSTRUCT::flags`.
    const MOUSE_INJECTED: u32 = 0x01;
    /// Index of the `flags` word as a `u32`: `KBDLLHOOKSTRUCT { vkCode, scanCode, flags, .. }`.
    const KEY_FLAGS_INDEX: usize = 2;
    /// Index of the `flags` word as a `u32`: `MSLLHOOKSTRUCT { pt (two i32), mouseData, flags, .. }`.
    const MOUSE_FLAGS_INDEX: usize = 3;

    static OWNER_EVENTS: AtomicU64 = AtomicU64::new(0);
    static ACTIVE: AtomicBool = AtomicBool::new(false);

    pub fn owner_events() -> u64 {
        OWNER_EVENTS.load(Ordering::Relaxed)
    }

    /// Counts the event when its injected bit is clear. Only the flags word is read.
    fn count_if_owner(lparam: LPARAM, index: usize, injected: u32) {
        if lparam.0 == 0 {
            return;
        }
        // SAFETY: for a low-level hook with `code == HC_ACTION`, `lparam` points to a
        // KBDLLHOOKSTRUCT or MSLLHOOKSTRUCT that lives for the call; both start with 32-bit words and
        // the indexed word is inside the struct. An unaligned read cannot fault on alignment.
        let flags = unsafe { std::ptr::read_unaligned((lparam.0 as *const u32).add(index)) };
        if flags & injected == 0 {
            OWNER_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    unsafe extern "system" fn on_keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION {
            count_if_owner(lparam, KEY_FLAGS_INDEX, KEY_INJECTED);
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION {
            count_if_owner(lparam, MOUSE_FLAGS_INDEX, MOUSE_INJECTED);
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// The hook thread. Dropping it ends the message loop, removes both hooks and joins.
    pub struct Hook {
        thread_id: u32,
        join: Option<JoinHandle<()>>,
    }

    impl Hook {
        /// `None` when another hook of this module is live, the thread cannot start, or a hook is refused.
        pub fn start() -> Option<Hook> {
            if ACTIVE
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return None;
            }
            let (tx, rx) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("rightkit-input-watch".into())
                .spawn(move || {
                    // SAFETY: both hooks are installed and removed on this thread, which runs the
                    // message loop low-level hooks need; the procedures above are `extern "system"`.
                    unsafe {
                        let thread_id = GetCurrentThreadId();
                        let keyboard =
                            SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_keyboard), None, 0);
                        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), None, 0);
                        let both = keyboard.is_ok() && mouse.is_ok();
                        let _ = tx.send(both.then_some(thread_id));
                        if both {
                            let mut message = MSG::default();
                            while GetMessageW(&mut message, None, 0, 0).0 > 0 {}
                        }
                        if let Ok(hook) = keyboard {
                            let _ = UnhookWindowsHookEx(hook);
                        }
                        if let Ok(hook) = mouse {
                            let _ = UnhookWindowsHookEx(hook);
                        }
                    }
                });
            let Ok(join) = spawned else {
                ACTIVE.store(false, Ordering::SeqCst);
                return None;
            };
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Some(thread_id)) => Some(Hook {
                    thread_id,
                    join: Some(join),
                }),
                _ => {
                    let _ = join.join();
                    ACTIVE.store(false, Ordering::SeqCst);
                    None
                }
            }
        }
    }

    impl Drop for Hook {
        fn drop(&mut self) {
            // SAFETY: posts WM_QUIT to the hook thread's own queue, which ends its message loop.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
            ACTIVE.store(false, Ordering::SeqCst);
        }
    }
}
