//! Foreground events are delivered on their registering thread, whose idle wait blocks
//! on either Windows input or an explicit shutdown event. No periodic timer is installed.
use std::cell::RefCell;
use std::sync::Arc;
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, INFINITE};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, MsgWaitForMultipleObjects, PeekMessageW,
    EVENT_SYSTEM_FOREGROUND, MSG, PM_NOREMOVE, PM_REMOVE, QS_ALLINPUT, WINEVENT_OUTOFCONTEXT,
};

type Observer = Box<dyn Fn(HWND)>;
thread_local! {
    static OBSERVER: RefCell<Option<Observer>> = RefCell::new(None);
}

unsafe extern "system" fn foreground_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    // Use the event's window, not GetForegroundWindow: focus may already have returned.
    OBSERVER.with(|observer| {
        if let Some(record) = observer.borrow().as_ref() {
            record(hwnd);
        }
    });
}

struct StopEvent(usize);
impl StopEvent {
    fn handle(&self) -> HANDLE {
        self.0 as HANDLE
    }
}
impl Drop for StopEvent {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle());
        }
    }
}

pub(super) struct Monitor {
    stop: Arc<StopEvent>,
    thread: Option<JoinHandle<()>>,
    failed: Arc<dyn Fn() + Send + Sync>,
}

impl Monitor {
    pub(super) fn start(
        record: impl Fn(HWND) + Send + 'static,
        failed: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() {
            failed();
            return None;
        }
        let failed: Arc<dyn Fn() + Send + Sync> = Arc::new(failed);
        let thread_failed = failed.clone();
        let stop = Arc::new(StopEvent(handle as usize));
        let thread_stop = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let thread = std::thread::spawn(move || unsafe {
            let failed = thread_failed;
            OBSERVER.with(|observer| *observer.borrow_mut() = Some(Box::new(record)));
            let mut message: MSG = std::mem::zeroed();
            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
            let hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                std::ptr::null_mut(),
                Some(foreground_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            if hook.is_null() {
                failed();
                OBSERVER.with(|observer| observer.borrow_mut().take());
                let _ = ready_tx.send(());
                return;
            }
            // Register before the initial snapshot & release launch only after both exist.
            foreground_event(
                hook,
                EVENT_SYSTEM_FOREGROUND,
                GetForegroundWindow(),
                0,
                0,
                0,
                0,
            );
            let _ = ready_tx.send(());
            let stop_handle = thread_stop.handle();
            loop {
                let wake = MsgWaitForMultipleObjects(1, &stop_handle, 0, INFINITE, QS_ALLINPUT);
                if wake != WAIT_OBJECT_0 && wake != WAIT_OBJECT_0 + 1 {
                    failed();
                    break;
                }
                // Drain events already queued before shutdown, preserving transient steals.
                while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&message);
                }
                if wake == WAIT_OBJECT_0 {
                    break;
                }
            }
            if UnhookWinEvent(hook) == 0 {
                failed();
            }
            foreground_event(
                hook,
                EVENT_SYSTEM_FOREGROUND,
                GetForegroundWindow(),
                0,
                0,
                0,
                0,
            );
            OBSERVER.with(|observer| observer.borrow_mut().take());
        });
        // A disconnected startup channel is never evidence of a working observer.
        let monitor = Self {
            stop,
            thread: Some(thread),
            failed,
        };
        if ready_rx.recv().is_err() {
            drop(monitor);
            return None;
        }
        Some(monitor)
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        unsafe {
            SetEvent(self.stop.handle());
        }
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                (self.failed)();
            }
        }
    }
}
