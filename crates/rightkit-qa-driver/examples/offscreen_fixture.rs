//! Tiny macOS fixture for the ignored `offscreen_macos` integration test: an Accessory-policy
//! (no Dock icon, never activated) NSApplication with one unordered NSWindow hosting a WKWebView.
//! It calls `start_if_requested` exactly as an embedding app would at startup, writes the session
//! report for the controller, then idles until killed (or `RK_FIXTURE_TTL` seconds, default 90).
//! Fatal signals exit silently so no crash dialog can appear on a shared desktop (process-scoped).
#[cfg(target_os = "macos")]
mod fixture {
    use rightkit_qa_driver::offscreen;
    use std::ffi::c_void;
    type Id = *mut c_void;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(n: *const std::ffi::c_char) -> Id;
        fn sel_registerName(n: *const std::ffi::c_char) -> *const c_void;
        fn objc_msgSend();
    }
    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}
    #[link(name = "WebKit", kind = "framework")]
    extern "C" {}
    extern "C" {
        fn signal(sig: i32, h: extern "C" fn(i32)) -> usize;
        fn _exit(code: i32) -> !;
    }
    extern "C" fn quiet_exit(sig: i32) {
        unsafe { _exit(128 + sig) }
    }
    macro_rules! m {
        ($ret:ty; $o:expr, $s:expr $(, $t:ty = $a:expr)*) => {{
            let f: unsafe extern "C" fn(Id, *const c_void $(, $t)*) -> $ret =
                std::mem::transmute(objc_msgSend as *const c_void);
            f($o, sel_registerName(concat!($s, "\0").as_ptr().cast()) $(, $a)*)
        }};
    }
    pub fn run() {
        unsafe {
            for s in [11, 10, 6, 4, 8] {
                signal(s, quiet_exit);
            }
            let ttl: u64 = std::env::var("RK_FIXTURE_TTL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(90);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(ttl));
                _exit(0)
            });
            let cls = |n: &[u8]| objc_getClass(n.as_ptr().cast());
            let app = m!(Id; cls(b"NSApplication\0"), "sharedApplication");
            m!(bool; app, "setActivationPolicy:", i64 = 1); // Accessory
            let r = Rect {
                x: 200.0,
                y: 200.0,
                w: 640.0,
                h: 400.0,
            };
            let win = m!(Id; cls(b"NSWindow\0"), "alloc");
            let win = m!(Id; win, "initWithContentRect:styleMask:backing:defer:", Rect = r, u64 = 11, u64 = 2, bool = false);
            m!((); win, "setReleasedWhenClosed:", bool = false);
            let cfg = m!(Id; cls(b"WKWebViewConfiguration\0"), "new");
            let wk = m!(Id; cls(b"WKWebView\0"), "alloc");
            let wk = m!(Id; wk, "initWithFrame:configuration:", Rect = Rect { x: 0.0, y: 0.0, w: 640.0, h: 400.0 }, Id = cfg);
            m!((); win, "setContentView:", Id = wk);
            let res = if offscreen::offscreen_requested() {
                let res = offscreen::start_native(win, wk);
                offscreen::write_report(&res);
                res.is_ok()
            } else {
                false
            };
            let _ = res;
            m!((); app, "run");
        }
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    fixture::run();
}
