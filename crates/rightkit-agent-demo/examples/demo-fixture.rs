//! Demo fixture (Windows): one window with a text field, a Submit button and a result label, the
//! target for the agent-demo acceptance run. Control ids are the UIA AutomationIds: 1001 is the
//! field "fixture-input", 1002 the button "fixture-submit", 1003 the label "fixture-result". Each
//! has a preceding static caption with the same name so UIA reads it as the control's name.
//! Dev/demo builds only; raw FFI so there is no dependency.

#[cfg(not(windows))]
fn main() {
    eprintln!("demo-fixture.exe is Windows only; on macOS build main.swift with build.sh");
}

#[cfg(windows)]
#[allow(non_snake_case, clippy::upper_case_acronyms)]
mod win {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    type HWND = *mut c_void;
    type LRESULT = isize;
    type WPARAM = usize;
    type LPARAM = isize;

    #[repr(C)]
    struct POINT {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    struct MSG {
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        time: u32,
        pt: POINT,
    }
    #[repr(C)]
    struct WNDCLASSW {
        style: u32,
        proc_: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
        cls_extra: i32,
        wnd_extra: i32,
        instance: *mut c_void,
        icon: *mut c_void,
        cursor: *mut c_void,
        background: *mut c_void,
        menu: *const u16,
        class_name: *const u16,
    }

    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassW(c: *const WNDCLASSW) -> u16;
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: HWND,
            menu: isize,
            inst: *mut c_void,
            param: *mut c_void,
        ) -> HWND;
        fn DefWindowProcW(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT;
        fn GetMessageW(m: *mut MSG, h: HWND, a: u32, b: u32) -> i32;
        fn TranslateMessage(m: *const MSG) -> i32;
        fn DispatchMessageW(m: *const MSG) -> LRESULT;
        fn PostQuitMessage(code: i32);
        fn ShowWindow(h: HWND, cmd: i32) -> i32;
        fn GetDlgItem(h: HWND, id: i32) -> HWND;
        fn GetWindowTextLengthW(h: HWND) -> i32;
        fn GetWindowTextW(h: HWND, buf: *mut u16, max: i32) -> i32;
        fn SetWindowTextW(h: HWND, text: *const u16) -> i32;
        fn LoadCursorW(inst: *mut c_void, name: usize) -> *mut c_void;
        fn SetFocus(h: HWND) -> HWND;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_TABSTOP: u32 = 0x0001_0000;
    const WS_BORDER: u32 = 0x0080_0000;
    const WM_DESTROY: u32 = 0x0002;
    const WM_COMMAND: u32 = 0x0111;
    const BS_PUSHBUTTON: u32 = 0;
    const COLOR_WINDOW_PLUS_1: usize = 6;
    const ID_INPUT: i32 = 1001;
    const ID_SUBMIT: i32 = 1002;
    const ID_RESULT: i32 = 1003;
    const SW_SHOW: i32 = 5;
    const IDC_ARROW: usize = 32512;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    unsafe fn child(
        parent: HWND,
        class: &str,
        text: &str,
        style: u32,
        rect: (i32, i32, i32, i32),
        id: i32,
    ) -> HWND {
        CreateWindowExW(
            0,
            w(class).as_ptr(),
            w(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            parent,
            id as isize,
            null_mut(),
            null_mut(),
        )
    }

    unsafe extern "system" fn proc_(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match m {
            WM_COMMAND if (wp & 0xFFFF) as i32 == ID_SUBMIT => {
                let field = GetDlgItem(h, ID_INPUT);
                let n = GetWindowTextLengthW(field).max(0) as usize;
                let mut buf = vec![0u16; n + 1];
                let got = GetWindowTextW(field, buf.as_mut_ptr(), (n + 1) as i32).max(0) as usize;
                let text = String::from_utf16_lossy(&buf[..got]);
                SetWindowTextW(
                    GetDlgItem(h, ID_RESULT),
                    w(&format!("Submitted: {text}")).as_ptr(),
                );
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(h, m, wp, lp),
        }
    }

    pub fn run() {
        unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let class = w("OrthicDemoFixture");
            let wc = WNDCLASSW {
                style: 0,
                proc_,
                cls_extra: 0,
                wnd_extra: 0,
                instance: inst,
                icon: null_mut(),
                cursor: LoadCursorW(null_mut(), IDC_ARROW),
                background: COLOR_WINDOW_PLUS_1 as *mut c_void,
                menu: std::ptr::null(),
                class_name: class.as_ptr(),
            };
            RegisterClassW(&wc);
            let win = CreateWindowExW(
                0,
                class.as_ptr(),
                w("Demo fixture").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                200,
                200,
                360,
                240,
                null_mut(),
                0,
                inst,
                null_mut(),
            );
            child(win, "STATIC", "fixture-input", 0, (20, 10, 300, 18), 0);
            let input = child(
                win,
                "EDIT",
                "",
                WS_BORDER | WS_TABSTOP,
                (20, 30, 300, 24),
                ID_INPUT,
            );
            child(win, "STATIC", "fixture-submit", 0, (20, 64, 300, 18), 0);
            child(
                win,
                "BUTTON",
                "Submit",
                BS_PUSHBUTTON | WS_TABSTOP,
                (20, 84, 100, 28),
                ID_SUBMIT,
            );
            child(win, "STATIC", "fixture-result", 0, (20, 122, 300, 18), 0);
            child(win, "STATIC", "Waiting", 0, (20, 142, 300, 24), ID_RESULT);
            ShowWindow(win, SW_SHOW);
            SetFocus(input);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

#[cfg(windows)]
fn main() {
    win::run();
}
