//! Hold and service the `tauri-plugin-single-instance` claim for a legacy identifier, so an OLD
//! build launched while a NEW build (on this crate) is running finds a live holder, forwards its
//! launch, and exits, instead of starting a second instance.
//!
//! Wire formats (plugin source read at 2.4.2 and 2.4.3, where `windows.rs` is identical; 2.4.5 was
//! not available to read and is unverified here):
//!
//! * Windows: the named mutex `<id>-sim`, and a window of class `<id>-sic` titled `<id>-siw`. A launch
//!   is `WM_COPYDATA` with `dwData == 1542` and a NUL-terminated `"{cwd}|{arg0}|{arg1}..."` in
//!   `lpData` (`cbData` counts the NUL). `wParam` is 0. A `|` inside the cwd or an argument cannot be
//!   told apart from a separator: the plugin's own receiver splits the same way.
//! * macOS: a unix socket `/tmp/<id with . and - as _>_si.sock`. A launch is a connection that writes
//!   `cwd`, two NULs, then the argv joined by NUL, and closes.
//! * Linux: the plugin uses a D-Bus name. NOT IMPLEMENTED here: on Linux the hold is a no-op.
//!
//! Everything received comes from another process, so payloads are bounded ([`MAX_PAYLOAD`]) and
//! parsed without panicking.

#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use std::sync::Arc;

use crate::single_instance::InstanceMessage;

/// Largest launch payload accepted, in bytes.
pub(crate) const MAX_PAYLOAD: usize = 64 * 1024;
/// Most arguments accepted in one launch.
pub(crate) const MAX_ARGS: usize = 4096;

pub(crate) type OnMessage = Arc<dyn Fn(InstanceMessage) + Send + Sync>;

/// Parse the Windows `WM_COPYDATA` payload: `cwd|arg|arg...`, ended by the first NUL.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn parse_window_payload(bytes: &[u8]) -> Option<(String, Vec<String>)> {
    if bytes.len() > MAX_PAYLOAD {
        return None;
    }
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut parts = text.split('|');
    let cwd = parts.next()?.to_owned();
    let argv: Vec<String> = parts.take(MAX_ARGS + 1).map(str::to_owned).collect();
    if argv.len() > MAX_ARGS {
        return None;
    }
    Some((cwd, argv))
}

/// Parse the macOS socket payload: `cwd`, `\0\0`, then the argv joined by `\0`.
// Used by the Unix socket path only.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn parse_socket_payload(bytes: &[u8]) -> Option<(String, Vec<String>)> {
    if bytes.len() > MAX_PAYLOAD {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let (cwd, rest) = text.split_once("\0\0")?;
    let argv: Vec<String> = rest
        .split('\0')
        .take(MAX_ARGS + 1)
        .map(str::to_owned)
        .collect();
    if argv.len() > MAX_ARGS {
        return None;
    }
    Some((cwd.to_owned(), argv))
}

fn message(cwd: String, argv: Vec<String>) -> InstanceMessage {
    InstanceMessage {
        argv,
        cwd,
        action: None,
        args: Default::default(),
    }
}

/// What taking the legacy claim found.
pub(crate) enum Acquired {
    /// This process now holds and services the claim until the value is dropped.
    Held(LegacyHold),
    /// A live legacy holder existed; this launch was delivered to it.
    Forwarded,
}

/// A held legacy claim. Dropping it stops the service thread, joins it and frees the name.
pub(crate) struct LegacyHold(#[allow(dead_code)] imp::Hold);

/// Take the legacy claim for `identifier`, or deliver this launch (`argv`, `cwd`) to its live holder.
/// Never blocks longer than a few seconds. An `Err` means the claim state could not be decided.
pub(crate) fn acquire(
    identifier: &str,
    argv: &[String],
    cwd: &str,
    on_message: OnMessage,
) -> Result<Acquired, String> {
    imp::acquire(identifier, argv, cwd, on_message)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::single_instance::LegacySingleInstanceNames;

    const WM_CLOSE: u32 = 0x0010;
    const WM_DESTROY: u32 = 0x0002;
    const WM_COPYDATA: u32 = 0x004A;
    const WMCOPYDATA_SINGLE_INSTANCE_DATA: usize = 1542;
    const ERROR_ALREADY_EXISTS: u32 = 183;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_LAYERED: u32 = 0x0008_0000;
    const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const READY_TIMEOUT: Duration = Duration::from_secs(10);

    type Wndproc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

    #[repr(C)]
    struct WndClassExW {
        cb_size: u32,
        style: u32,
        wnd_proc: Option<Wndproc>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        instance: isize,
        icon: isize,
        cursor: isize,
        background: isize,
        menu_name: *const u16,
        class_name: *const u16,
        icon_sm: isize,
    }

    #[repr(C)]
    struct Msg {
        hwnd: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt_x: i32,
        pt_y: i32,
        private: u32,
    }

    #[repr(C)]
    struct CopyDataStruct {
        dw_data: usize,
        cb_data: u32,
        lp_data: *const u8,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *const c_void, initial_owner: i32, name: *const u16) -> isize;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: isize) -> i32;
        fn GetModuleHandleW(name: *const u16) -> isize;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
        fn GetCurrentProcessId() -> u32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassExW(class: *const WndClassExW) -> u16;
        fn UnregisterClassW(class_name: *const u16, instance: isize) -> i32;
        #[allow(clippy::too_many_arguments)]
        fn CreateWindowExW(
            ex_style: u32,
            class_name: *const u16,
            window_name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: isize,
            menu: isize,
            instance: isize,
            param: *const c_void,
        ) -> isize;
        fn DestroyWindow(hwnd: isize) -> i32;
        fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
        fn DispatchMessageW(msg: *const Msg) -> isize;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
        fn PostQuitMessage(code: i32);
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    }

    thread_local! {
        /// The service thread's callback; the window procedure runs on this thread.
        static CALLBACK: RefCell<Option<OnMessage>> = const { RefCell::new(None) };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) struct Hold {
        hwnd: isize,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            // SAFETY: posting to a window from another thread is allowed; a dead window is a no-op.
            unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
            if let Some(thread) = self.thread.take() {
                // Dropped from inside the callback (destroy() called there): joining would deadlock.
                if thread.thread().id() != std::thread::current().id() {
                    let _ = thread.join();
                }
            }
        }
    }

    enum Ready {
        Held(isize),
        Live,
        Failed(String),
    }

    pub(super) fn acquire(
        identifier: &str,
        argv: &[String],
        cwd: &str,
        on_message: OnMessage,
    ) -> Result<Acquired, String> {
        for _ in 0..2 {
            let names = LegacySingleInstanceNames::for_identifier(identifier);
            let (tx, rx) = mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("rk-legacy-instance".into())
                .spawn({
                    let on_message = on_message.clone();
                    move || serve(names, on_message, tx)
                })
                .map_err(|e| format!("spawn the legacy service thread: {e}"))?;
            match rx.recv_timeout(READY_TIMEOUT) {
                Ok(Ready::Held(hwnd)) => {
                    return Ok(Acquired::Held(LegacyHold(Hold {
                        hwnd,
                        thread: Some(thread),
                    })))
                }
                Ok(Ready::Live) => {
                    let _ = thread.join();
                    match crate::legacy_instance::forward(identifier, argv, cwd)? {
                        Some(()) => return Ok(Acquired::Forwarded),
                        // The holder went away between the two looks: take the claim.
                        None => continue,
                    }
                }
                Ok(Ready::Failed(e)) => {
                    let _ = thread.join();
                    return Err(e);
                }
                Err(_) => return Err("the legacy service thread did not report".into()),
            }
        }
        Err("the legacy claim kept changing hands".into())
    }

    fn serve(names: LegacySingleInstanceNames, on_message: OnMessage, ready: mpsc::Sender<Ready>) {
        let mutex_name = wide(&names.windows_mutex);
        let class_name = wide(&names.windows_window_class);
        let window_name = wide(&names.windows_window_title);
        // The mutex is the claim and is taken first: an existing mutex means a live holder, and no
        // window of ours exists yet for a forward to find by mistake.
        // SAFETY: NUL-terminated name; read the error immediately after the call.
        let (mutex, last_error) = unsafe {
            let h = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
            (h, GetLastError())
        };
        if mutex == 0 {
            let _ = ready.send(Ready::Failed(format!(
                "CreateMutexW failed: error {last_error}"
            )));
            return;
        }
        if last_error == ERROR_ALREADY_EXISTS {
            // SAFETY: handle from CreateMutexW, closed once.
            unsafe { CloseHandle(mutex) };
            let _ = ready.send(Ready::Live);
            return;
        }
        CALLBACK.with(|c| *c.borrow_mut() = Some(on_message));
        // SAFETY: standard window class and window creation with NUL-terminated names that outlive
        // the calls; the window procedure is an `extern "system"` fn with the matching signature.
        let (instance, hwnd) = unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = WndClassExW {
                cb_size: std::mem::size_of::<WndClassExW>() as u32,
                style: 0,
                wnd_proc: Some(window_proc),
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                instance,
                icon: 0,
                cursor: 0,
                background: 0,
                menu_name: std::ptr::null(),
                class_name: class_name.as_ptr(),
                icon_sm: 0,
            };
            RegisterClassExW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
                class_name.as_ptr(),
                window_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                instance,
                std::ptr::null(),
            );
            (instance, hwnd)
        };
        if hwnd == 0 {
            // SAFETY: handle from CreateMutexW, closed once.
            unsafe {
                CloseHandle(mutex);
                UnregisterClassW(class_name.as_ptr(), instance);
            }
            let _ = ready.send(Ready::Failed(
                "CreateWindowExW failed for the legacy window".into(),
            ));
            return;
        }
        let _ = ready.send(Ready::Held(hwnd));
        // SAFETY: a plain message loop on the thread that owns the window; it ends on WM_QUIT, which
        // the window procedure posts when the window is destroyed.
        unsafe {
            let mut msg = std::mem::zeroed::<Msg>();
            while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
            CloseHandle(mutex);
            UnregisterClassW(class_name.as_ptr(), instance);
        }
        CALLBACK.with(|c| *c.borrow_mut() = None);
    }

    /// True when the sending window belongs to a process in this session. `wParam` is 0 in the
    /// plugin's own messages, which cannot be checked and are accepted: the window and mutex names
    /// are session-local, so only a process in this session can reach this window at all.
    fn sender_allowed(sender: usize) -> bool {
        if sender == 0 {
            return true;
        }
        // SAFETY: queries on a window handle and a process; every handle opened is closed.
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(sender as isize, &mut pid);
            if pid == 0 {
                return false;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process == 0 {
                return false;
            }
            CloseHandle(process);
            let (mut theirs, mut ours) = (u32::MAX, u32::MAX - 1);
            let ok = ProcessIdToSessionId(pid, &mut theirs) != 0
                && ProcessIdToSessionId(GetCurrentProcessId(), &mut ours) != 0;
            ok && theirs == ours
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        match msg {
            WM_COPYDATA => {
                let data = lparam as *const CopyDataStruct;
                if data.is_null() || !sender_allowed(wparam) {
                    return 0;
                }
                // SAFETY: WM_COPYDATA's lParam points to a COPYDATASTRUCT valid for this call.
                let (tag, len, ptr) =
                    unsafe { ((*data).dw_data, (*data).cb_data as usize, (*data).lp_data) };
                if tag != WMCOPYDATA_SINGLE_INSTANCE_DATA
                    || ptr.is_null()
                    || len == 0
                    || len > MAX_PAYLOAD
                {
                    return 0;
                }
                // SAFETY: the sender guarantees `len` readable bytes at `ptr` for this call.
                let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
                let Some((cwd, argv)) = parse_window_payload(bytes) else {
                    return 0;
                };
                let callback = CALLBACK.with(|c| c.borrow().clone());
                if let Some(callback) = callback {
                    // A panic must not unwind through the window procedure.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        callback(message(cwd, argv))
                    }));
                }
                1
            }
            WM_CLOSE => {
                // SAFETY: destroying our own window on its own thread.
                unsafe { DestroyWindow(hwnd) };
                0
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                0
            }
            // SAFETY: default handling of a message we do not use.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use crate::single_instance::LegacySingleInstanceNames;

    const READ_TIMEOUT: Duration = Duration::from_secs(2);

    pub(super) struct Hold {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Wake the blocked accept; the loop sees `stop` and ends.
            let _ = UnixStream::connect(&self.path);
            if let Some(thread) = self.thread.take() {
                if thread.thread().id() != std::thread::current().id() {
                    let _ = thread.join();
                }
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub(super) fn acquire(
        identifier: &str,
        argv: &[String],
        cwd: &str,
        on_message: OnMessage,
    ) -> Result<Acquired, String> {
        let path = LegacySingleInstanceNames::for_identifier(identifier).macos_socket;
        for _ in 0..3 {
            match UnixListener::bind(&path) {
                Ok(listener) => {
                    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                    let stop = Arc::new(AtomicBool::new(false));
                    let thread = std::thread::Builder::new()
                        .name("rk-legacy-instance".into())
                        .spawn({
                            let stop = stop.clone();
                            move || serve(listener, stop, on_message)
                        })
                        .map_err(|e| format!("spawn the legacy service thread: {e}"))?;
                    return Ok(Acquired::Held(LegacyHold(Hold {
                        path,
                        stop,
                        thread: Some(thread),
                    })));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    match crate::legacy_instance::forward(identifier, argv, cwd)? {
                        Some(()) => return Ok(Acquired::Forwarded),
                        // A socket file with nothing listening is a crashed holder's leftover.
                        None => {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
                Err(e) => return Err(format!("bind {}: {e}", path.display())),
            }
        }
        Err("the legacy claim kept changing hands".into())
    }

    fn serve(listener: UnixListener, stop: Arc<AtomicBool>, on_message: OnMessage) {
        for stream in listener.incoming() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
            let mut bytes = Vec::new();
            // One byte past the bound tells an oversized payload from one that fits exactly.
            if (&mut stream)
                .take(MAX_PAYLOAD as u64 + 1)
                .read_to_end(&mut bytes)
                .is_err()
            {
                continue;
            }
            if let Some((cwd, argv)) = parse_socket_payload(&bytes) {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    on_message(message(cwd, argv))
                }));
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::*;

    /// Linux has no hold: the plugin's D-Bus name is not implemented here.
    pub(super) struct Hold;

    pub(super) fn acquire(
        _identifier: &str,
        _argv: &[String],
        _cwd: &str,
        _on_message: OnMessage,
    ) -> Result<Acquired, String> {
        Ok(Acquired::Held(LegacyHold(Hold)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_payload_splits_cwd_and_args_and_stops_at_the_nul() {
        let (cwd, argv) = parse_window_payload(b"C:\\work|C:\\app.exe|--section|x\0junk").unwrap();
        assert_eq!(cwd, "C:\\work");
        assert_eq!(argv, ["C:\\app.exe", "--section", "x"]);
        // No NUL: the whole buffer is the text.
        assert_eq!(parse_window_payload(b"/w|/a").unwrap().1, ["/a"]);
        // An empty cwd and no args is still a launch.
        assert_eq!(
            parse_window_payload(b"\0").unwrap(),
            (String::new(), Vec::new())
        );
    }

    #[test]
    fn socket_payload_needs_the_double_nul_and_splits_the_argv() {
        let (cwd, argv) = parse_socket_payload(b"/w\0\0/a\0--x\0y").unwrap();
        assert_eq!(cwd, "/w");
        assert_eq!(argv, ["/a", "--x", "y"]);
        assert!(parse_socket_payload(b"no separator").is_none());
    }

    #[test]
    fn oversized_and_malformed_payloads_are_refused_not_panicked_on() {
        let big = vec![b'a'; MAX_PAYLOAD + 1];
        assert!(parse_window_payload(&big).is_none());
        assert!(parse_socket_payload(&big).is_none());
        // Invalid UTF-8 is replaced, not an error or a panic.
        assert!(parse_window_payload(&[b'/', 0xFF, b'|', 0xFE, 0]).is_some());
        // Too many arguments.
        let many = format!("/w{}", "|a".repeat(MAX_ARGS + 1));
        assert!(parse_window_payload(many.as_bytes()).is_none());
        let many = format!("/w\0\0{}", "a\0".repeat(MAX_ARGS + 1));
        assert!(parse_socket_payload(many.as_bytes()).is_none());
    }
}
