//! RK2 Spike A fixture: Tauri/WebView window with step id, frame counter and animated region, driven through
//! rightkit-control's embedded path. Adds spike-only native placement/visibility commands (arm64 macOS) and an
//! admission/dispatch/completion timestamp journal (RK2_LOG, JSONL, CLOCK_UPTIME_RAW ns).
use rightkit_control::admission::{AdmissionDecision, EffectGate, EffectRequest};
use rightkit_control::embedded::Control;
use rightkit_control::events::ControlEvent;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

extern "C" {
    fn clock_gettime_nsec_np(clock_id: u32) -> u64;
}
fn uptime_ns() -> u64 {
    unsafe { clock_gettime_nsec_np(8) } // CLOCK_UPTIME_RAW == mach_absolute_time domain used by SCStream host clock
}
static LOG: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
fn jl(line: String) {
    if let Some(f) = LOG.get() {
        let _ = writeln!(f.lock().unwrap(), "{line}");
    }
}

#[cfg(target_os = "macos")]
mod nat {
    use std::ffi::{c_char, c_void};
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct Pt { pub x: f64, pub y: f64 }
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct Rect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }
    #[link(name = "objc")]
    extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }
    extern "C" {
        fn object_getClass(o: *mut c_void) -> *mut c_void;
        fn object_setClass(o: *mut c_void, c: *mut c_void) -> *mut c_void;
        fn objc_allocateClassPair(sup: *mut c_void, name: *const c_char, extra: usize) -> *mut c_void;
        fn objc_registerClassPair(c: *mut c_void);
        fn class_addMethod(c: *mut c_void, s: *const c_void, imp: *const c_void, types: *const c_char) -> bool;
        fn class_replaceMethod(c: *mut c_void, s: *const c_void, imp: *const c_void, types: *const c_char) -> *mut c_void;
        fn class_getInstanceMethod(c: *mut c_void, s: *const c_void) -> *mut c_void;
        fn method_getTypeEncoding(m: *mut c_void) -> *const c_char;
    }
    extern "C" fn unconstrained(_: *mut c_void, _: *const c_void, r: Rect, _: *mut c_void) -> Rect { r }
    /// Per-window subclass whose constrainFrameRect:toScreen: is the identity (same technique as rightkit-control set_viewport),
    /// kept for the window's lifetime so orderFront does not pull an off-screen window back onto a display.
    pub unsafe fn install_unconstrained(w: *mut c_void) -> *mut c_void {
        let base = object_getClass(w);
        let name = std::ffi::CString::new(format!("Rk2Unconstrained_{:x}", base as usize)).unwrap();
        let mut sub = objc_getClass(name.as_ptr());
        if sub.is_null() {
            let s = sel(c"constrainFrameRect:toScreen:");
            let m = class_getInstanceMethod(base, s);
            if m.is_null() { return std::ptr::null_mut(); }
            sub = objc_allocateClassPair(base, name.as_ptr(), 0);
            if sub.is_null() || !class_addMethod(sub, s, unconstrained as *const c_void, method_getTypeEncoding(m)) { return std::ptr::null_mut(); }
            objc_registerClassPair(sub);
        }
        object_setClass(w, sub);
        base
    }
    /// Permanent, class-scoped (window class only) identity override. A per-instance subclass breaks tao's sendEvent
    /// (superclass walk, SIGSEGV), so the override is installed on the window's own class instead.
    pub unsafe fn constrain_override_class(w: *mut c_void) -> bool {
        let base = object_getClass(w);
        let s = sel(c"constrainFrameRect:toScreen:");
        let m = class_getInstanceMethod(base, s);
        if m.is_null() { return false; }
        class_replaceMethod(base, s, unconstrained as *const c_void, method_getTypeEncoding(m));
        true
    }
    /// tao's sendEvent: walks the window class's superclass, so the override must be scoped: restore the original class.
    pub unsafe fn restore_class(w: *mut c_void, base: *mut c_void) { if !base.is_null() { object_setClass(w, base); } }
    pub fn msg_ptr() -> *const c_void { objc_msgSend as *const c_void }
    pub unsafe fn sel(n: &std::ffi::CStr) -> *const c_void { sel_registerName(n.as_ptr()) }
    pub unsafe fn class(n: &std::ffi::CStr) -> *mut c_void { objc_getClass(n.as_ptr()) }
    pub unsafe fn obj(o: *mut c_void, s: &std::ffi::CStr) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void = std::mem::transmute(objc_msgSend as *const c_void);
        f(o, sel(s))
    }
    pub unsafe fn boolean(o: *mut c_void, s: &std::ffi::CStr) -> bool {
        let f: unsafe extern "C" fn(*mut c_void, *const c_void) -> bool = std::mem::transmute(objc_msgSend as *const c_void);
        f(o, sel(s))
    }
    pub unsafe fn int(o: *mut c_void, s: &std::ffi::CStr) -> i64 {
        let f: unsafe extern "C" fn(*mut c_void, *const c_void) -> i64 = std::mem::transmute(objc_msgSend as *const c_void);
        f(o, sel(s))
    }
    pub unsafe fn dbl(o: *mut c_void, s: &std::ffi::CStr) -> f64 {
        let f: unsafe extern "C" fn(*mut c_void, *const c_void) -> f64 = std::mem::transmute(objc_msgSend as *const c_void);
        f(o, sel(s))
    }
    pub unsafe fn rect(o: *mut c_void, s: &std::ffi::CStr) -> Rect {
        let f: unsafe extern "C" fn(*mut c_void, *const c_void) -> Rect = std::mem::transmute(objc_msgSend as *const c_void);
        f(o, sel(s))
    }
    pub unsafe fn send_nil(o: *mut c_void, s: &std::ffi::CStr) {
        let f: unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) = std::mem::transmute(objc_msgSend as *const c_void);
        f(o, sel(s), std::ptr::null_mut());
    }
    pub unsafe fn set_origin(o: *mut c_void, p: Pt) {
        let f: unsafe extern "C" fn(*mut c_void, *const c_void, Pt) = std::mem::transmute(objc_msgSend as *const c_void);
        f(o, sel(c"setFrameOrigin:"), p);
    }
    pub unsafe fn set_size(o: *mut c_void, w: f64, h: f64) {
        #[repr(C)] struct Sz { w: f64, h: f64 }
        let f: unsafe extern "C" fn(*mut c_void, *const c_void, Sz, bool) = std::mem::transmute(objc_msgSend as *const c_void);
        f(o, sel(c"setContentSize:"), Sz { w, h }, false);
    }
    pub unsafe fn constrain(w: *mut c_void, r: Rect) -> Rect {
        let f: unsafe extern "C" fn(*mut c_void, *const c_void, Rect, *mut c_void) -> Rect = std::mem::transmute(objc_msgSend as *const c_void);
        let scr = obj(w, c"screen");
        f(w, sel(c"constrainFrameRect:toScreen:"), r, scr)
    }
    pub unsafe fn screens_json() -> String {
        let arr = {
            let f: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void = std::mem::transmute(objc_msgSend as *const c_void);
            f(class(c"NSScreen"), sel(c"screens"))
        };
        let n = int(arr, c"count");
        let mut out = vec![];
        for i in 0..n {
            let f: unsafe extern "C" fn(*mut c_void, *const c_void, u64) -> *mut c_void = std::mem::transmute(objc_msgSend as *const c_void);
            let s = f(arr, sel(c"objectAtIndex:"), i as u64);
            let r = rect(s, c"frame");
            out.push(format!(r#"{{"x":{},"y":{},"w":{},"h":{},"scale":{}}}"#, r.x, r.y, r.w, r.h, dbl(s, c"backingScaleFactor")));
        }
        format!("[{}]", out.join(","))
    }
}

#[cfg(target_os = "macos")]
fn on_main<T: Send + 'static>(app: &tauri::AppHandle, f: impl FnOnce(*mut std::ffi::c_void) -> T + Send + 'static) -> Result<T, String> {
    use tauri::Manager;
    let win = app.get_webview_window("main").ok_or("no main window")?;
    let nsw = win.ns_window().map_err(|e| e.to_string())? as usize;
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || { let _ = tx.send(f(nsw as *mut std::ffi::c_void)); }).map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn win_info(app: &tauri::AppHandle) -> Result<String, String> {
    on_main(app, |w| unsafe {
        let f = nat::rect(w, c"frame");
        let cl = nat::rect(w, c"contentLayoutRect");
        let scr = nat::obj(w, c"screen");
        let nsapp = nat::obj(nat::class(c"NSApplication"), c"sharedApplication");
        format!(
            r#"{{"pid":{},"windowNumber":{},"frame":{{"x":{},"y":{},"w":{},"h":{}}},"contentLayout":{{"x":{},"y":{},"w":{},"h":{}}},"backingScale":{},"onAnyScreen":{},"isVisible":{},"isMiniaturized":{},"occlusionState":{},"appHidden":{},"screens":{}}}"#,
            std::process::id(), nat::int(w, c"windowNumber"), f.x, f.y, f.w, f.h, cl.x, cl.y, cl.w, cl.h,
            nat::dbl(w, c"backingScaleFactor"), !scr.is_null(), nat::boolean(w, c"isVisible"), nat::boolean(w, c"isMiniaturized"),
            nat::int(w, c"occlusionState"), nat::boolean(nsapp, c"isHidden"), nat::screens_json()
        )
    })
}

fn num(body: &str, key: &str) -> Option<f64> {
    let i = body.find(&format!("\"{key}\""))?;
    let rest = &body[i + key.len() + 2..];
    let rest = rest.trim_start_matches([':', ' ']);
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    fn _exit(code: i32) -> !;
}
extern "C" fn quiet_exit(sig: i32) { unsafe { _exit(128 + sig) } }

#[cfg(target_os = "macos")]
fn apply_spi(win: &tauri::WebviewWindow) {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = win.with_webview(move |pv| unsafe {
        use std::ffi::c_void;
        let wk = pv.inner();
        let setb: unsafe extern "C" fn(*mut c_void, *const c_void, bool) = std::mem::transmute(nat::msg_ptr());
        setb(wk, nat::sel(c"_setWindowOcclusionDetectionEnabled:"), false);
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
}

fn main() {
    // Spike fixture only: never raise the macOS "quit unexpectedly" dialog on a shared desktop. A fatal signal exits
    // silently (no ReportCrash). Process-scoped; no global CrashReporter setting is touched.
    unsafe { for s in [11, 10, 6, 4, 8] { signal(s, quiet_exit); } }
    if let Ok(p) = std::env::var("RK2_LOG") {
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = LOG.set(Mutex::new(f)); }
    }
    let mut builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    { builder = builder.activate_ignoring_other_apps(false); }

    // EFF-001 gate: the approval hook is the admission timestamp; EffectStarted is the dispatch boundary; EffectStopped is completion.
    let gate = Arc::new(
        EffectGate::new(|r: &EffectRequest| {
            jl(format!(r#"{{"ev":"admit","id":{},"method":"{}","kind":"{}","uptime_ns":{}}}"#, r.id, r.method, r.kind.label(), uptime_ns()));
            AdmissionDecision::Allow
        })
        .with_events(Arc::new(|e: &ControlEvent| match e {
            ControlEvent::EffectStarted { id, method, .. } => jl(format!(r#"{{"ev":"dispatch","id":{id},"method":"{method}","uptime_ns":{}}}"#, uptime_ns())),
            ControlEvent::EffectStopped { id, method, outcome, executed, .. } => jl(format!(r#"{{"ev":"complete","id":{id},"method":"{method}","outcome":"{}","executed":{executed},"uptime_ns":{}}}"#, outcome.label(), uptime_ns())),
            ControlEvent::EffectDenied { id, method, reason, .. } => jl(format!(r#"{{"ev":"denied","id":{id},"method":"{method}","reason":"{}","uptime_ns":{}}}"#, reason.replace('"', "'"), uptime_ns())),
            _ => {}
        })),
    );

    let control = Control::<tauri::Wry>::new()
        .gate(gate)
        .command("clock", |_a, _b| {
            let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0;
            Ok(format!(r#"{{"epoch_ms":{ms},"uptime_ns":{}}}"#, uptime_ns()))
        });
    #[cfg(target_os = "macos")]
    let control = control
        .command("win_info", |a, _| win_info(a))
        // place {x,y}: AppKit global coords (bottom-left origin). setFrameOrigin: is not constrained to screens; orderFrontRegardless
        // makes the window visible to the window server without making it key or activating the app.
        .command("place", |a, b| {
            let (x, y) = (num(b, "x").ok_or("x")?, num(b, "y").ok_or("y")?);
            let (w, h) = (num(b, "w"), num(b, "h"));
            on_main(a, move |w_| unsafe {
                if let (Some(w), Some(h)) = (w, h) { nat::set_size(w_, w, h); }
                nat::set_origin(w_, nat::Pt { x, y });
                // Override only around ordering: setting the origin under the override corrupted the frame (0x0).
                nat::constrain_override_class(w_);
                nat::send_nil(w_, c"orderFrontRegardless");
            })?;
            win_info(a)
        })
        .command("x_constrain", |a, _| {
            let r = on_main(a, |w| unsafe {
                let r0 = nat::Rect { x: 5000.0, y: 300.0, w: 800.0, h: 600.0 };
                let plain = nat::constrain(w, r0);
                let base = nat::install_unconstrained(w);
                let over = nat::constrain(w, r0);
                nat::restore_class(w, base);
                let after = nat::rect(w, c"frame");
                format!("{{\"plain\":{plain:?},\"override\":{over:?},\"frame_after\":{after:?}}}")
            })?;
            Ok(format!("\"{}\"", r.replace('"', "'")))
        })
        .command("x_origin", |a, b| {
            let (x, y) = (num(b, "x").ok_or("x")?, num(b, "y").ok_or("y")?);
            on_main(a, move |w| unsafe { nat::set_origin(w, nat::Pt { x, y }) })?; win_info(a)
        })
        .command("x_front_ov", |a, _| {
            on_main(a, |w| unsafe { let base = nat::install_unconstrained(w); nat::send_nil(w, c"orderFrontRegardless"); nat::restore_class(w, base); })?; win_info(a)
        })
        .command("x_front_ov_info", |a, _| {
            let r = on_main(a, |w| unsafe { let base = nat::install_unconstrained(w); nat::send_nil(w, c"orderFrontRegardless"); let f = nat::rect(w, c"frame"); nat::restore_class(w, base); format!("{f:?} {:?}", nat::rect(w, c"frame")) })?;
            Ok(format!("\"{r}\""))
        })
        // vis_force {mode}: try keeping the page "visible" while its window is off-screen.
        //  spi    -> -[WKWebView _setWindowOcclusionDetectionEnabled:NO] (private SPI, if present)
        //  policy -> configuration.preferences.inactiveSchedulingPolicy = .none (public, macOS 14+)
        .command("vis_force", |a, b| {
            use tauri::Manager;
            let mode = rightkit_control::json::str_field(b, "mode").unwrap_or_default();
            let win = a.get_webview_window("main").ok_or("no window")?;
            let (tx, rx) = std::sync::mpsc::channel();
            win.with_webview(move |pv| unsafe {
                use std::ffi::c_void;
                let wk = pv.inner();
                let responds: unsafe extern "C" fn(*mut c_void, *const c_void, *const c_void) -> bool = std::mem::transmute(nat::msg_ptr());
                let setb: unsafe extern "C" fn(*mut c_void, *const c_void, bool) = std::mem::transmute(nat::msg_ptr());
                let seti: unsafe extern "C" fn(*mut c_void, *const c_void, i64) = std::mem::transmute(nat::msg_ptr());
                let r = if mode == "spi" {
                    let s = nat::sel(c"_setWindowOcclusionDetectionEnabled:");
                    if responds(wk, nat::sel(c"respondsToSelector:"), s) { setb(wk, s, false); "spi applied".to_string() } else { "spi selector missing".to_string() }
                } else {
                    let cfg = nat::obj(wk, c"configuration"); let prefs = nat::obj(cfg, c"preferences");
                    let s = nat::sel(c"setInactiveSchedulingPolicy:");
                    if responds(prefs, nat::sel(c"respondsToSelector:"), s) { seti(prefs, s, 2); format!("policy set; now {}", nat::int(prefs, c"inactiveSchedulingPolicy")) } else { "policy selector missing".to_string() }
                };
                let _ = tx.send(r);
            }).map_err(|e| e.to_string())?;
            let r = rx.recv_timeout(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?;
            Ok(format!("\"{r}\""))
        })
        // nap_off: hold an NSProcessInfo activity (user-initiated + latency-critical) so App Nap / timer coalescing cannot throttle the page.
        .command("nap_off", |a, _| {
            on_main(a, |_| unsafe {
                use std::ffi::c_void;
                let pi = nat::obj(nat::class(c"NSProcessInfo"), c"processInfo");
                let mk: unsafe extern "C" fn(*mut c_void, *const c_void, *const std::ffi::c_char) -> *mut c_void = std::mem::transmute(nat::msg_ptr());
                let reason = mk(nat::class(c"NSString"), nat::sel(c"stringWithUTF8String:"), c"rk2 spike capture".as_ptr());
                let begin: unsafe extern "C" fn(*mut c_void, *const c_void, u64, *mut c_void) -> *mut c_void = std::mem::transmute(nat::msg_ptr());
                let act = begin(pi, nat::sel(c"beginActivityWithOptions:reason:"), 0x00EF_FFFF | 0xFF_0000_0000, reason);
                let retain: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void = std::mem::transmute(nat::msg_ptr());
                retain(act, nat::sel(c"retain"));
            })?;
            Ok("\"activity held\"".into())
        })
        .command("order_front", |a, _| { on_main(a, |w| unsafe { nat::send_nil(w, c"orderFrontRegardless") })?; win_info(a) })
        .command("order_out", |a, _| { on_main(a, |w| unsafe { nat::send_nil(w, c"orderOut:") })?; win_info(a) })
        .command("miniaturize", |a, _| { on_main(a, |w| unsafe { nat::send_nil(w, c"miniaturize:") })?; std::thread::sleep(std::time::Duration::from_millis(1200)); win_info(a) })
        .command("deminiaturize", |a, _| { on_main(a, |w| unsafe { nat::send_nil(w, c"deminiaturize:") })?; std::thread::sleep(std::time::Duration::from_millis(800)); win_info(a) })
        .command("app_hide", |a, _| {
            on_main(a, |_| unsafe { nat::send_nil(nat::obj(nat::class(c"NSApplication"), c"sharedApplication"), c"hide:") })?;
            std::thread::sleep(std::time::Duration::from_millis(600)); win_info(a)
        })
        .command("app_unhide", |a, _| {
            on_main(a, |_| unsafe { nat::send_nil(nat::obj(nat::class(c"NSApplication"), c"sharedApplication"), c"unhideWithoutActivation") })?;
            std::thread::sleep(std::time::Duration::from_millis(600)); win_info(a)
        });
    if let Some(plugin) = control.build_if_enabled() { builder = builder.plugin(plugin); }

    builder
        .setup(|app| {
            // Occluder role: separate instance without control; places a magenta animated window at RK2_OCCLUDER="x,y,w,h".
            if let Ok(spec) = std::env::var("RK2_OCCLUDER") {
                #[cfg(target_os = "macos")]
                {
                    let h = app.handle().clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(1500));
                        { use tauri::Manager; if let Some(win) = h.get_webview_window("main") { apply_spi(&win); } }
                        let v: Vec<f64> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                        let _ = on_main(&h, move |w| unsafe {
                            nat::set_size(w, v[2], v[3]);
                            nat::set_origin(w, nat::Pt { x: v[0], y: v[1] });
                            nat::constrain_override_class(w);
                            // Floating level (3) so the occluder sits above the target (same-level ordering put it behind).
                            let setl: unsafe extern "C" fn(*mut std::ffi::c_void, *const std::ffi::c_void, i64) = std::mem::transmute(nat::msg_ptr());
                            setl(w, nat::sel(c"setLevel:"), 3);
                            nat::send_nil(w, c"orderFrontRegardless");
                        });
                        use tauri::Manager;
                        if let Some(win) = h.get_webview_window("main") { let _ = win.eval("window.__occ=true;document.getElementById('occ').style.display='block'"); }
                    });
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("run rk2-fixture");
}
