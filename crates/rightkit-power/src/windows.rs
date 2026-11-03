//! Windows: suspend and resume, power settings and session lock.
//!
//! - `PowerRegisterSuspendResumeNotification` (callback form) reports sleep and resume. The
//!   callback runs on a system thread and, for suspend, waits up to [`SUSPEND_ACK`] for the app.
//! - `PowerSettingRegisterNotification` (callback form) reports AC or battery, the console
//!   display state and battery saver.
//! - A hidden top-level window on a thread of its own receives `WM_WTSSESSION_CHANGE` (lock and
//!   unlock) and `WM_ENDSESSION`. It is never shown. A message-only window would not receive the
//!   end-of-session broadcast, so a hidden top-level one is used.
//!
//! Every callback gets a raw pointer to the `Shared` the [`Reader`] holds an `Arc` of. The
//! reader removes every registration and joins the window thread before it lets go of the `Arc`.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{
    PowerRegisterSuspendResumeNotification, PowerSettingRegisterNotification,
    PowerSettingUnregisterNotification, PowerUnregisterSuspendResumeNotification,
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY, POWERBROADCAST_SETTING,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows_sys::Win32::System::SystemServices::{
    GUID_ACDC_POWER_SOURCE, GUID_CONSOLE_DISPLAY_STATE, GUID_POWER_SAVING_STATUS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, PostMessageW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW,
    DEVICE_NOTIFY_CALLBACK, GWLP_USERDATA, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND,
    PBT_POWERSETTINGCHANGE, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WM_WTSSESSION_CHANGE, WNDCLASSEXW,
    WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

use crate::{Event, Shared, END_SESSION_ACK, SUSPEND_ACK};

const CLASS_NAME: &str = "RightKitPowerSession";

/// Everything registered for one `Power`, removed on drop.
pub(crate) struct Reader {
    shared: Arc<Shared>,
    registrations: Vec<Registration>,
    session: Option<SessionWindow>,
}

// SAFETY: the raw pointers inside are OS registration handles and a window handle, used only
// from `drop` and from the window thread's own join; the `Shared` they point at is `Sync`.
unsafe impl Send for Reader {}
// SAFETY: as above; a shared reference gives no access to the handles.
unsafe impl Sync for Reader {}

impl Reader {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self, String> {
        let context = Arc::as_ptr(&shared) as *mut c_void;
        let mut registrations = Vec::new();

        match Registration::suspend_resume(context) {
            Ok(registration) => registrations.push(registration),
            Err(error) => log::warn!("sleep and resume are not followed: {error}"),
        }
        for (name, setting) in [
            ("AC or battery", GUID_ACDC_POWER_SOURCE),
            ("display state", GUID_CONSOLE_DISPLAY_STATE),
            ("battery saver", GUID_POWER_SAVING_STATUS),
        ] {
            match Registration::setting(&setting, context) {
                Ok(registration) => registrations.push(registration),
                Err(error) => log::warn!("{name} is not followed: {error}"),
            }
        }

        let session = match SessionWindow::start(context) {
            Ok(window) => Some(window),
            Err(error) => {
                log::warn!("session lock is not followed: {error}");
                None
            }
        };

        if registrations.is_empty() && session.is_none() {
            return Err("no power or session notification could be registered".into());
        }
        Ok(Self {
            shared,
            registrations,
            session,
        })
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // Remove the registrations (the OS stops calling back) and end the window thread, and
        // only then let the `Arc<Shared>` go.
        self.registrations.clear();
        self.session.take();
        let _ = &self.shared;
    }
}

enum Kind {
    SuspendResume,
    Setting,
}

struct Registration {
    kind: Kind,
    handle: *mut c_void,
    // The registration refers to these parameters until it is removed.
    _parameters: Box<DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS>,
}

impl Registration {
    fn suspend_resume(context: *mut c_void) -> Result<Self, String> {
        let mut parameters = Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(suspend_resume_callback),
            Context: context,
        });
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: `parameters` is a live subscribe-parameters struct that outlives the
        // registration (the box is kept in the returned value), `handle` is a valid out pointer.
        let status = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                &mut *parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as HANDLE,
                &mut handle,
            )
        };
        if status != 0 {
            return Err(format!(
                "PowerRegisterSuspendResumeNotification error {status}"
            ));
        }
        Ok(Self {
            kind: Kind::SuspendResume,
            handle,
            _parameters: parameters,
        })
    }

    fn setting(setting: &GUID, context: *mut c_void) -> Result<Self, String> {
        let mut parameters = Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(setting_callback),
            Context: context,
        });
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: as in `suspend_resume`; `setting` points at a live GUID for the call.
        let status = unsafe {
            PowerSettingRegisterNotification(
                setting,
                DEVICE_NOTIFY_CALLBACK,
                &mut *parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as HANDLE,
                &mut handle,
            )
        };
        if status != 0 {
            return Err(format!("PowerSettingRegisterNotification error {status}"));
        }
        Ok(Self {
            kind: Kind::Setting,
            handle,
            _parameters: parameters,
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let handle = self.handle as HPOWERNOTIFY;
        // SAFETY: `handle` came from the matching register call and is removed exactly once.
        unsafe {
            match self.kind {
                Kind::SuspendResume => PowerUnregisterSuspendResumeNotification(handle),
                Kind::Setting => PowerSettingUnregisterNotification(handle),
            };
        }
    }
}

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

/// # Safety
/// `context` is the `Arc::as_ptr` of a `Shared` that the `Reader` keeps alive until after every
/// registration is removed.
unsafe fn shared<'a>(context: *const c_void) -> &'a Shared {
    &*(context as *const Shared)
}

/// Runs `body`, never unwinding into the system.
fn guarded(body: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
        log::warn!("power and session: a callback panicked");
    }
}

unsafe extern "system" fn suspend_resume_callback(
    context: *const c_void,
    kind: u32,
    _setting: *const c_void,
) -> u32 {
    // SAFETY: see `shared`.
    let shared = unsafe { shared(context) };
    guarded(|| match kind {
        PBT_APMSUSPEND => shared.push_and_wait(Event::Suspending, SUSPEND_ACK),
        PBT_APMRESUMEAUTOMATIC => shared.push(Event::Resumed),
        _ => {}
    });
    0
}

unsafe extern "system" fn setting_callback(
    context: *const c_void,
    kind: u32,
    setting: *const c_void,
) -> u32 {
    if kind != PBT_POWERSETTINGCHANGE || setting.is_null() {
        return 0;
    }
    // SAFETY: see `shared`; for PBT_POWERSETTINGCHANGE the system passes a POWERBROADCAST_SETTING
    // whose `Data` holds `DataLength` bytes.
    let (shared, guid, value) = unsafe {
        let broadcast = setting as *const POWERBROADCAST_SETTING;
        let length = (*broadcast).DataLength as usize;
        let data = std::ptr::addr_of!((*broadcast).Data) as *const u8;
        let value = if length >= 4 {
            Some(std::ptr::read_unaligned(data as *const u32))
        } else {
            None
        };
        (self::shared(context), (*broadcast).PowerSetting, value)
    };
    let Some(value) = value else { return 0 };
    guarded(|| {
        if guid_eq(&guid, &GUID_ACDC_POWER_SOURCE) {
            // 0 AC, 1 battery, 2 short-term battery (UPS).
            shared.update(|conditions| conditions.on_battery = value != 0);
        } else if guid_eq(&guid, &GUID_CONSOLE_DISPLAY_STATE) {
            // 0 off, 1 on, 2 dimmed.
            shared.update(|conditions| conditions.display_off = value == 0);
        } else if guid_eq(&guid, &GUID_POWER_SAVING_STATUS) {
            shared.update(|conditions| conditions.saver = value != 0);
        }
    });
    0
}

/// The hidden session window and the thread that pumps its messages.
struct SessionWindow {
    hwnd: usize,
    thread: Option<JoinHandle<()>>,
}

impl SessionWindow {
    fn start(context: *mut c_void) -> Result<Self, String> {
        let context = context as usize;
        let (ready_tx, ready_rx) = mpsc::channel::<Result<usize, String>>();
        let thread = std::thread::Builder::new()
            .name("rightkit-power-session".into())
            .spawn(move || run_window(context, ready_tx))
            .map_err(|error| format!("session thread: {error}"))?;
        match ready_rx.recv() {
            Ok(Ok(hwnd)) => Ok(Self {
                hwnd,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err("session thread ended before it created its window".into())
            }
        }
    }
}

impl Drop for SessionWindow {
    fn drop(&mut self) {
        // SAFETY: `hwnd` is the window the thread created; posting a message to a window that
        // is already gone is harmless.
        unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn run_window(context: usize, ready: mpsc::Sender<Result<usize, String>>) {
    let class = wide(CLASS_NAME);
    // SAFETY: plain Win32 window creation on this thread with live, NUL-terminated strings;
    // the window procedure only reads the `Shared` pointer stored in the window's user data.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let window_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        // A second `Power` in the process finds the class already registered; that is fine.
        RegisterClassExW(&window_class);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            let _ = ready.send(Err("CreateWindowExW failed".into()));
            return;
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, context as isize);
        if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) == 0 {
            DestroyWindow(hwnd);
            let _ = ready.send(Err("WTSRegisterSessionNotification failed".into()));
            return;
        }
        let _ = ready.send(Ok(hwnd as usize));

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_WTSSESSION_CHANGE => {
            // SAFETY: the user data is the `Shared` pointer set before the window could receive
            // this message, or null before that.
            let context = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const c_void;
            if !context.is_null() {
                // SAFETY: see `shared`.
                let shared = unsafe { shared(context) };
                guarded(|| match wparam as u32 {
                    WTS_SESSION_LOCK => {
                        shared.update(|conditions| conditions.locked = true);
                        shared.push(Event::Locked);
                    }
                    WTS_SESSION_UNLOCK => {
                        shared.update(|conditions| conditions.locked = false);
                        shared.push(Event::Unlocked);
                    }
                    _ => {}
                });
            }
            0
        }
        WM_ENDSESSION => {
            // wparam is true when the session is actually ending.
            // SAFETY: as above.
            let context = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const c_void;
            if wparam != 0 && !context.is_null() {
                // SAFETY: see `shared`.
                let shared = unsafe { shared(context) };
                guarded(|| shared.push_and_wait(Event::EndingSession, END_SESSION_ACK));
            }
            0
        }
        WM_CLOSE => {
            // SAFETY: `hwnd` is this window.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // SAFETY: `hwnd` is this window; unregistering and quitting end the thread's loop.
            unsafe {
                WTSUnRegisterSessionNotification(hwnd);
                PostQuitMessage(0);
            }
            0
        }
        // SAFETY: default handling of a message this window does not own.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
