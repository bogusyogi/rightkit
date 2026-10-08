//! macOS backend for off-screen launch. Runs in the driven app's process.
//!
//! - class-scoped `constrainFrameRect:toScreen:` identity override (window class only,
//!   installed before the window is first ordered; the original IMP is restored at end).
//!   A per-instance subclass breaks tao's `sendEvent:` superclass walk, hence class scope.
//! - `NSProcessInfo` activity `NSActivityUserInitiatedAllowingIdleSystemSleep` held for
//!   the session (defeats App Nap throttling of the WebKit display link).
//! - `_setWindowOcclusionDetectionEnabled:NO` (private SPI, feature `offscreen-private-spi`).
#![allow(non_snake_case)]

use super::*;
use std::ffi::{c_char, c_void, CString};

type Id = *mut c_void;
type Sel = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
    fn object_getClass(o: Id) -> Id;
    fn class_getInstanceMethod(c: Id, s: Sel) -> Id;
    fn method_getImplementation(m: Id) -> *const c_void;
    fn method_getTypeEncoding(m: Id) -> *const c_char;
    fn class_replaceMethod(
        c: Id,
        s: Sel,
        imp: *const c_void,
        types: *const c_char,
    ) -> *const c_void;
}
#[link(name = "AppKit", kind = "framework")]
extern "C" {}
#[link(name = "Foundation", kind = "framework")]
extern "C" {}
extern "C" {
    fn pthread_main_np() -> i32;
}

unsafe fn class(n: &str) -> Id {
    let c = CString::new(n).unwrap();
    objc_getClass(c.as_ptr())
}
unsafe fn sel(n: &str) -> Sel {
    let c = CString::new(n).unwrap();
    sel_registerName(c.as_ptr())
}

macro_rules! msg {
    (NSRect; $obj:expr, $sel:expr) => {{
        #[cfg(target_arch = "x86_64")]
        {
            let mut r = std::mem::MaybeUninit::<NSRect>::uninit();
            let f: unsafe extern "C" fn(*mut NSRect, Id, Sel) =
                std::mem::transmute(objc_msgSend_stret as *const c_void);
            f(r.as_mut_ptr(), $obj, sel($sel));
            r.assume_init()
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let f: unsafe extern "C" fn(Id, Sel) -> NSRect =
                std::mem::transmute(objc_msgSend as *const c_void);
            f($obj, sel($sel))
        }
    }};
    ($ret:ty; $obj:expr, $sel:expr $(, $t:ty = $a:expr)*) => {{
        let f: unsafe extern "C" fn(Id, Sel $(, $t)*) -> $ret =
            std::mem::transmute(objc_msgSend as *const c_void);
        f($obj, sel($sel) $(, $a)*)
    }};
}

fn to_rect(r: NSRect) -> Rect {
    Rect::new(r.x, r.y, r.w, r.h)
}

extern "C" fn unconstrained(_: Id, _: Sel, r: NSRect, _: Id) -> NSRect {
    r
}

const ACTIVITY_OPTIONS: u64 = 0x00EF_FFFF; // NSActivityUserInitiatedAllowingIdleSystemSleep

/// `NSWindow` driven through the placement abstraction. Main thread only.
struct MacWindow(Id);

impl WindowOps for MacWindow {
    fn displays(&self) -> Result<Vec<Rect>, OffscreenError> {
        unsafe {
            let arr = msg!(Id; class("NSScreen"), "screens");
            if arr.is_null() {
                return Err(OffscreenError::NoDisplays);
            }
            let n = msg!(usize; arr, "count");
            Ok((0..n)
                .map(|i| to_rect(msg!(NSRect; msg!(Id; arr, "objectAtIndex:", usize = i), "frame")))
                .collect())
        }
    }
    fn frame(&self) -> Result<Rect, OffscreenError> {
        Ok(to_rect(unsafe { msg!(NSRect; self.0, "frame") }))
    }
    fn backing_scale(&self) -> Result<f64, OffscreenError> {
        Ok(unsafe { msg!(f64; self.0, "backingScaleFactor") })
    }
    fn is_visible(&self) -> Result<bool, OffscreenError> {
        Ok(unsafe { msg!(bool; self.0, "isVisible") })
    }
    fn is_miniaturized(&self) -> Result<bool, OffscreenError> {
        Ok(unsafe { msg!(bool; self.0, "isMiniaturized") })
    }
    fn set_origin(&mut self, x: f64, y: f64) -> Result<(), OffscreenError> {
        unsafe { msg!((); self.0, "setFrameOrigin:", NSPoint = NSPoint { x, y }) };
        Ok(())
    }
    fn show(&mut self) -> Result<(), OffscreenError> {
        // Visible to the window server without making it key or activating the app.
        unsafe { msg!((); self.0, "orderFrontRegardless") };
        Ok(())
    }
    fn hide_for_restore(&mut self) -> Result<(), OffscreenError> {
        unsafe { msg!((); self.0, "orderOut:", Id = std::ptr::null_mut()) };
        Ok(())
    }
}

pub struct Session {
    window: Id,
    webview: Id,
    placement: Placement,
    class: Id,
    orig_imp: *const c_void,
    encoding: *const c_char,
    activity: Id,
    spi: SpiStatus,
}

fn main_thread() -> Result<(), OffscreenError> {
    if unsafe { pthread_main_np() } == 1 {
        Ok(())
    } else {
        Err(OffscreenError::NotMainThread)
    }
}

#[cfg(feature = "offscreen-private-spi")]
unsafe fn spi_probe(wk: Id) -> SpiStatus {
    if wk.is_null()
        || !msg!(bool; wk, "respondsToSelector:", Sel = sel("_setWindowOcclusionDetectionEnabled:"))
    {
        SpiStatus::Unavailable
    } else {
        SpiStatus::Responded
    }
}
#[cfg(not(feature = "offscreen-private-spi"))]
unsafe fn spi_probe(_wk: Id) -> SpiStatus {
    SpiStatus::NotCompiled
}

#[cfg(feature = "offscreen-private-spi")]
unsafe fn spi_apply(wk: Id) {
    msg!((); wk, "_setWindowOcclusionDetectionEnabled:", bool = false);
}
#[cfg(not(feature = "offscreen-private-spi"))]
unsafe fn spi_apply(_wk: Id) {}

fn spi_error(s: SpiStatus) -> Option<OffscreenError> {
    match s {
        SpiStatus::Responded => None,
        SpiStatus::NotCompiled => Some(OffscreenError::OcclusionSpiNotCompiled),
        _ => Some(OffscreenError::OcclusionSpiUnavailable),
    }
}

unsafe fn undo_override(s: &Session) {
    class_replaceMethod(
        s.class,
        sel("constrainFrameRect:toScreen:"),
        s.orig_imp,
        s.encoding,
    );
}
unsafe fn end_activity(s: &Session) {
    if !s.activity.is_null() {
        let pi = msg!(Id; class("NSProcessInfo"), "processInfo");
        msg!((); pi, "endActivity:", Id = s.activity);
        msg!((); s.activity, "release");
    }
}

/// # Safety
/// `ns_window` / `wk_webview` must be live; main thread, before the first order.
pub(super) unsafe fn start(
    ns_window: *mut c_void,
    wk_webview: *mut c_void,
) -> Result<Session, OffscreenError> {
    main_thread()?;
    if ns_window.is_null() {
        return Err(OffscreenError::NullWindow);
    }
    // Fail loudly before touching anything.
    if let Some(e) = spi_error(spi_probe(wk_webview)) {
        return Err(e);
    }
    let wclass = object_getClass(ns_window);
    let s = sel("constrainFrameRect:toScreen:");
    let m = class_getInstanceMethod(wclass, s);
    if m.is_null() {
        return Err(OffscreenError::ConstrainOverrideUnavailable);
    }
    let orig_imp = method_getImplementation(m);
    let encoding = method_getTypeEncoding(m);

    let pi = msg!(Id; class("NSProcessInfo"), "processInfo");
    let reason = msg!(Id; class("NSString"), "stringWithUTF8String:", *const c_char = c"RightKit off-screen session".as_ptr());
    let act = msg!(Id; pi, "beginActivityWithOptions:reason:", u64 = ACTIVITY_OPTIONS, Id = reason);
    if act.is_null() {
        return Err(OffscreenError::ActivityUnavailable);
    }
    let activity = msg!(Id; act, "retain");

    spi_apply(wk_webview);
    class_replaceMethod(wclass, s, unconstrained as *const c_void, encoding);

    let mut sess = Session {
        window: ns_window,
        webview: wk_webview,
        placement: Placement {
            original: Rect::new(0.0, 0.0, 0.0, 0.0),
            placed: Rect::new(0.0, 0.0, 0.0, 0.0),
            backing_scale: 0.0,
        },
        class: wclass,
        orig_imp,
        encoding,
        activity,
        spi: SpiStatus::Responded,
    };
    let mut w = MacWindow(ns_window);
    let placed = place(&mut w).and_then(|p| {
        sess.placement = p;
        qualify(&snapshot(&w, sess.spi)?, Platform::Mac).into_result()
    });
    if let Err(e) = placed {
        // roll back: hide and restore the frame if we moved it, drop override and activity
        if sess.placement.backing_scale > 0.0 {
            let _ = restore(&mut w, &sess.placement);
        }
        undo_override(&sess);
        end_activity(&sess);
        return Err(e);
    }
    Ok(sess)
}

pub(super) fn report(s: &Session) -> SessionReport {
    SessionReport {
        pid: std::process::id(),
        window_id: unsafe { msg!(i64; s.window, "windowNumber") }.max(0) as u64,
        placed: s.placement.placed,
        original: s.placement.original,
        backing_scale: s.placement.backing_scale,
        qualified: true,
    }
}

pub(super) fn session_qualification(s: &Session) -> Result<QualificationReport, OffscreenError> {
    main_thread()?;
    let w = MacWindow(s.window);
    Ok(qualify(&snapshot(&w, s.spi)?, Platform::Mac))
}

pub(super) fn end(s: Session) -> Result<(), OffscreenError> {
    main_thread()?;
    let mut w = MacWindow(s.window);
    let r = restore(&mut w, &s.placement);
    unsafe {
        undo_override(&s);
        end_activity(&s);
    }
    let _ = s.webview;
    r
}
