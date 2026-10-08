//! Windows fixture for the ignored `offscreen_windows` integration test. A plain Win32 top-level
//! window (never activated: `SW_SHOWNOACTIVATE`, tool window with no taskbar button) hosting one
//! WebView2 controller. It calls `start_native` on its own HWND exactly as an embedding app would,
//! writes the session report for the controller through `write_report`, and then pumps messages
//! until it is stopped or its TTL expires.
//!
//! Env (set by the test; all optional):
//! - `RK_FIXTURE_OUT`: path prefix. Writes `<prefix>.session.json` after a successful start and
//!   `<prefix>.after.json` after the stop file appears. Both carry the EcoQoS state read inside this process.
//! - `RK_FIXTURE_STOP`: when this file exists, the fixture calls `end_active` on the main thread.
//! - `RK_FIXTURE_TTL`: seconds before the fixture exits on its own (default 90).
//! - `RK_FIXTURE_MODE=minimized`: show the window minimized without activation, then report what
//!   `start_native` says (it must be refused).
//!
//! The window is created hidden (the documented embedded path); `start_native` moves it off every display
//! and shows it without activation. Right after creation, `<prefix>.created.json` carries the HWND so a
//! controller can observe the window across the start.
//!
//! Built only with the `wv2-0_38` feature (declared with `required-features` in Cargo.toml).
#[cfg(windows)]
mod fixture {
    use rightkit_control::offscreen::{self, OffscreenError};
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use webview2_com_038::Microsoft::Web::WebView2::Win32::{
        CreateCoreWebView2Environment, ICoreWebView2Controller, ICoreWebView2Environment,
    };
    use webview2_com_038::{
        CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    };
    use windows_061::core::HSTRING;
    use windows_061::Win32::Foundation::{HWND, RECT as WRECT};

    #[repr(C)]
    #[derive(Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    struct Msg {
        hwnd: *mut c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: Point,
    }
    type WndProc = unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize;
    #[repr(C)]
    struct WndClassW {
        style: u32,
        wnd_proc: WndProc,
        cls_extra: i32,
        wnd_extra: i32,
        instance: *mut c_void,
        icon: *mut c_void,
        cursor: *mut c_void,
        background: *mut c_void,
        menu_name: *const u16,
        class_name: *const u16,
    }
    #[repr(C)]
    #[derive(Default)]
    struct PowerThrottlingState {
        version: u32,
        control_mask: u32,
        state_mask: u32,
    }

    const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    const SW_SHOWMINNOACTIVE: i32 = 7;
    const WM_TIMER: u32 = 0x0113;
    const WM_DESTROY: u32 = 0x0002;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const PROCESS_POWER_THROTTLING_CLASS: i32 = 4;

    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassW(wc: *const WndClassW) -> u16;
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: *mut c_void,
            menu: *mut c_void,
            instance: *mut c_void,
            param: *mut c_void,
        ) -> *mut c_void;
        fn ShowWindow(h: *mut c_void, cmd: i32) -> i32;
        fn GetClientRect(h: *mut c_void, r: *mut Rect) -> i32;
        fn GetMessageW(msg: *mut Msg, h: *mut c_void, min: u32, max: u32) -> i32;
        fn TranslateMessage(msg: *const Msg) -> i32;
        fn DispatchMessageW(msg: *const Msg) -> isize;
        fn DefWindowProcW(h: *mut c_void, msg: u32, wp: usize, lp: isize) -> isize;
        fn SetTimer(h: *mut c_void, id: usize, ms: u32, cb: *const c_void) -> usize;
        fn PostQuitMessage(code: i32);
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn GetCurrentProcess() -> isize;
        fn GetProcessInformation(h: isize, class: i32, info: *mut c_void, size: u32) -> i32;
        fn GetLastError() -> u32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *const c_void, init: u32) -> i32;
    }

    struct State {
        started: Instant,
        ttl: Duration,
        stop: Option<PathBuf>,
        ended: bool,
    }
    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// EcoQoS state of this process: (query ok, ControlMask, StateMask, last error).
    fn throttling() -> (bool, u32, u32, u32) {
        let mut st = PowerThrottlingState {
            version: 1,
            ..Default::default()
        };
        let ok = unsafe {
            GetProcessInformation(
                GetCurrentProcess(),
                PROCESS_POWER_THROTTLING_CLASS,
                &mut st as *mut _ as *mut c_void,
                std::mem::size_of::<PowerThrottlingState>() as u32,
            )
        };
        let err = if ok == 0 { unsafe { GetLastError() } } else { 0 };
        (ok != 0, st.control_mask, st.state_mask, err)
    }

    fn write_json(path: &Path, body: &str) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    fn out_path(suffix: &str) -> Option<PathBuf> {
        let base = PathBuf::from(std::env::var_os("RK_FIXTURE_OUT")?);
        Some(PathBuf::from(format!("{}.{suffix}.json", base.display())))
    }

    fn qos_body(phase: &str, end_error: Option<&str>) -> String {
        let (q_ok, control, state, q_err) = throttling();
        let extra = end_error
            .map(|e| format!(",\"end_error\":\"{}\"", e.replace('"', "'")))
            .unwrap_or_default();
        format!(
            "{{\"phase\":\"{phase}\",\"qos_query_ok\":{q_ok},\"control_mask\":{control},\"state_mask\":{state},\"qos_error\":{q_err}{extra}}}"
        )
    }

    /// Runs on the UI thread from WM_TIMER: ends the session once the stop file appears.
    fn tick() {
        let (expired, stop_now) = STATE.with(|s| {
            let mut s = s.borrow_mut();
            let s = s.as_mut().expect("state set before the timer starts");
            let expired = s.started.elapsed() > s.ttl;
            let stop_now = !s.ended && s.stop.as_ref().is_some_and(|p| p.exists());
            if stop_now {
                s.ended = true;
            }
            (expired, stop_now)
        });
        if expired {
            unsafe { PostQuitMessage(0) };
            return;
        }
        if stop_now {
            let end = offscreen::end_active();
            let msg = end.as_ref().err().map(|e: &OffscreenError| e.to_string());
            if let Some(p) = out_path("after") {
                write_json(&p, &qos_body("after", msg.as_deref()));
            }
            if let Some(stop) = STATE.with(|s| s.borrow().as_ref().and_then(|s| s.stop.clone())) {
                let _ = std::fs::remove_file(stop);
            }
        }
    }

    unsafe extern "system" fn wndproc(h: *mut c_void, msg: u32, wp: usize, lp: isize) -> isize {
        match msg {
            WM_TIMER => {
                tick();
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(h, msg, wp, lp),
        }
    }

    fn create_webview(
        hwnd: *mut c_void,
    ) -> Result<(ICoreWebView2Environment, ICoreWebView2Controller), String> {
        let env = {
            let (tx, rx) = mpsc::channel::<Result<ICoreWebView2Environment, String>>();
            CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
                Box::new(|h| unsafe {
                    CreateCoreWebView2Environment(&h).map_err(webview2_com_038::Error::WindowsError)
                }),
                Box::new(move |err, env| {
                    let r = match (err, env) {
                        (Ok(()), Some(env)) => Ok(env),
                        (Err(e), _) => Err(e.to_string()),
                        (Ok(()), None) => Err("no environment".to_string()),
                    };
                    let _ = tx.send(r);
                    Ok(())
                }),
            )
            .map_err(|e| e.to_string())?;
            rx.try_recv().map_err(|e| e.to_string())??
        };
        let controller = {
            let (tx, rx) = mpsc::channel::<Result<ICoreWebView2Controller, String>>();
            let env2 = env.clone();
            CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
                Box::new(move |h| unsafe {
                    env2.CreateCoreWebView2Controller(HWND(hwnd), &h)
                        .map_err(webview2_com_038::Error::WindowsError)
                }),
                Box::new(move |err, c| {
                    let r = match (err, c) {
                        (Ok(()), Some(c)) => Ok(c),
                        (Err(e), _) => Err(e.to_string()),
                        (Ok(()), None) => Err("no controller".to_string()),
                    };
                    let _ = tx.send(r);
                    Ok(())
                }),
            )
            .map_err(|e| e.to_string())?;
            rx.try_recv().map_err(|e| e.to_string())??
        };
        let mut c = Rect::default();
        unsafe { GetClientRect(hwnd, &mut c) };
        let bounds = WRECT {
            left: c.left,
            top: c.top,
            right: c.right,
            bottom: c.bottom,
        };
        unsafe { controller.SetBounds(bounds) }.map_err(|e| e.to_string())?;
        let core = unsafe { controller.CoreWebView2() }.map_err(|e| e.to_string())?;
        unsafe {
            core.NavigateToString(&HSTRING::from(
                "<!doctype html><html><body><h1>rightkit offscreen fixture</h1></body></html>",
            ))
        }
        .map_err(|e| e.to_string())?;
        Ok((env, controller))
    }

    pub fn run() {
        let minimized = std::env::var("RK_FIXTURE_MODE").is_ok_and(|m| m == "minimized");
        unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED) };

        let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class = wide("RightKitOffscreenFixture");
        let wc = WndClassW {
            style: 0,
            wnd_proc: wndproc,
            cls_extra: 0,
            wnd_extra: 0,
            instance: hinst,
            icon: std::ptr::null_mut(),
            cursor: std::ptr::null_mut(),
            background: std::ptr::null_mut(),
            menu_name: std::ptr::null(),
            class_name: class.as_ptr(),
        };
        unsafe { RegisterClassW(&wc) };
        let title = wide("RightKit offscreen fixture");
        // Created at an on-screen origin: the session moves it off every display after creation.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                640,
                400,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                std::ptr::null_mut(),
            )
        };
        if hwnd.is_null() {
            std::process::exit(2);
        }
        if let Some(p) = out_path("created") {
            write_json(&p, &format!("{{\"phase\":\"created\",\"hwnd\":{}}}", hwnd as usize));
        }

        if minimized {
            // Refusal path: a minimized window must be refused before anything moves.
            unsafe { ShowWindow(hwnd, SW_SHOWMINNOACTIVE) };
            let res = unsafe { offscreen::start_native(hwnd, std::ptr::null_mut()) };
            offscreen::write_report(&res);
            return;
        }

        // Still hidden here: start_native moves it off every display before showing it.
        let (_env, _controller) = match create_webview(hwnd) {
            Ok(w) => w,
            Err(e) => {
                offscreen::write_report(&Err(OffscreenError::Native(format!("webview2: {e}"))));
                return;
            }
        };
        let res = unsafe { offscreen::start_native(hwnd, std::ptr::null_mut()) };
        offscreen::write_report(&res);
        if res.is_ok() {
            if let Some(p) = out_path("session") {
                write_json(&p, &qos_body("session", None));
            }
        }
        let ttl = std::env::var("RK_FIXTURE_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                started: Instant::now(),
                ttl: Duration::from_secs(ttl),
                stop: std::env::var_os("RK_FIXTURE_STOP").map(PathBuf::from),
                ended: false,
            });
        });
        unsafe { SetTimer(hwnd, 1, 100, std::ptr::null()) };
        loop {
            let mut msg = Msg {
                hwnd: std::ptr::null_mut(),
                message: 0,
                wparam: 0,
                lparam: 0,
                time: 0,
                pt: Point { x: 0, y: 0 },
            };
            if unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } <= 0 {
                break;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

#[cfg(windows)]
fn main() {
    fixture::run();
}

#[cfg(not(windows))]
fn main() {}
