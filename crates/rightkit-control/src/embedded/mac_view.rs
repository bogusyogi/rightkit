//! macOS in-process input: synthesized `NSEvent`s dispatched straight to the
//! `WKWebView` responder methods. Nothing here activates the app or raises the
//! window, and AppKit's inactive-window first-mouse swallow is bypassed
//! because the window's `sendEvent:` is never involved. Raw Objective-C
//! runtime FFI keeps the crate free of extra dependencies.
#![allow(non_snake_case, clippy::missing_safety_doc)]

use super::{Button, Mouse};
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;
use tauri::{Runtime, WebviewWindow};

type Id = *mut c_void;
type Sel = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    static _NSConcreteStackBlock: *const c_void;
}
#[link(name = "AppKit", kind = "framework")]
extern "C" {}
#[link(name = "Foundation", kind = "framework")]
extern "C" {}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventCreateScrollWheelEvent2(
        src: *const c_void,
        unit: u32,
        n: u32,
        a: i32,
        b: i32,
        c: i32,
    ) -> *const c_void;
    fn CGEventSetLocation(e: *const c_void, p: NSPoint);
    fn CGMainDisplayID() -> u32;
    fn CGDisplayPixelsHigh(d: u32) -> usize;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(c: *const c_void);
}

unsafe fn class(n: &str) -> Id {
    let c = CString::new(n).unwrap();
    objc_getClass(c.as_ptr())
}
unsafe fn sel(n: &str) -> Sel {
    let c = CString::new(n).unwrap();
    sel_registerName(c.as_ptr())
}

/// Typed `objc_msgSend`: `msg!(Ret; obj, "selector:", Type = arg, ...)`.
macro_rules! msg {
    ($ret:ty; $obj:expr, $sel:expr $(, $t:ty = $a:expr)*) => {{
        let f: unsafe extern "C" fn(Id, Sel $(, $t)*) -> $ret = std::mem::transmute(objc_msgSend as *const c_void);
        f($obj, sel($sel) $(, $a)*)
    }};
}

unsafe fn nsstring(s: &str) -> Id {
    let c = CString::new(s.replace('\0', "")).unwrap();
    msg!(Id; class("NSString"), "stringWithUTF8String:", *const c_char = c.as_ptr())
}
unsafe fn utf8(s: Id) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let p = msg!(*const c_char; s, "UTF8String");
    (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
}
unsafe fn uptime() -> f64 {
    msg!(f64; msg!(Id; class("NSProcessInfo"), "processInfo"), "systemUptime")
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

/// The page's CSS viewport size, measured in the page.
fn css_viewport<R: Runtime>(win: &WebviewWindow<R>) -> Option<(f64, f64)> {
    let s = eval(win, "innerWidth + ',' + innerHeight").ok()?;
    let (w, h) = s.split_once(',')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// CSS viewport (x, y) -> window coordinates. The CSS viewport is the bottom of the
/// (flipped) WKWebView bounds: any top inset WebKit applies (titlebar, content inset)
/// is the view height minus the scaled viewport height, and the scale is the page
/// zoom. Measured, never assumed: with `titleBarStyle: Overlay` the page fills the
/// view, and adding `safeAreaInsets` put every pointer event one titlebar (32 px) low.
unsafe fn window_point(wk: Id, viewport: Option<(f64, f64)>, x: f64, y: f64) -> NSPoint {
    let b = msg!(NSRect; wk, "bounds");
    let (scale, top) = match viewport {
        Some((w, h)) if w > 0.0 && h > 0.0 => {
            let scale = b.size.width / w;
            (scale, (b.size.height - h * scale).max(0.0))
        }
        _ => (1.0, 0.0),
    };
    let p = NSPoint {
        x: b.origin.x + x * scale,
        y: b.origin.y + top + y * scale,
    };
    msg!(NSPoint; wk, "convertPoint:toView:", NSPoint = p, Id = std::ptr::null_mut())
}

/// Run `f` on the main thread with the WKWebView pointer and wait for it.
fn on_view<R: Runtime, T: Send + 'static>(
    win: &WebviewWindow<R>,
    f: impl FnOnce(Id) -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = channel();
    win.with_webview(move |pv| {
        let _ = tx.send(f(pv.inner()));
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|_| "webview call timed out".to_string())
}

pub fn mouse<R: Runtime>(
    win: &WebviewWindow<R>,
    kind: Mouse,
    button: Button,
    x: f64,
    y: f64,
    clicks: i64,
    flags: u64,
) -> Result<(), String> {
    let viewport = css_viewport(win);
    on_view(win, move |wk| unsafe {
        let window = msg!(Id; wk, "window");
        if window.is_null() {
            return Err("webview has no window".to_string());
        }
        let wp = window_point(wk, viewport, x, y);
        let right = button == Button::Right;
        // Hover is delivered as a button-less `mouseDragged:` on the WKWebView itself:
        // WebKit gates `mouseMoved:` (tracking-area path) on an active window, but the
        // WKWebView responder path works in an inactive window and the page still sees
        // a plain `mousemove` with `buttons == 0`, so `:hover` and `mouseenter` update.
        // NSEventType: 1/2 left down/up, 3/4 right down/up, 5 moved, 6/7 left/right dragged.
        let (ty, handler, pressure) = match (kind, right) {
            (Mouse::Down, false) => (1u64, "mouseDown:", 1.0f32),
            (Mouse::Up, false) => (2, "mouseUp:", 0.0),
            (Mouse::Down, true) => (3, "rightMouseDown:", 1.0),
            (Mouse::Up, true) => (4, "rightMouseUp:", 0.0),
            (Mouse::Move, _) => (6, "mouseDragged:", 0.0),
            (Mouse::Drag, false) => (6, "mouseDragged:", 1.0),
            (Mouse::Drag, true) => (7, "rightMouseDragged:", 1.0),
        };
        let ev = msg!(Id; class("NSEvent"),
            "mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:",
            u64 = ty, NSPoint = wp, u64 = flags, f64 = uptime(), i64 = msg!(i64; window, "windowNumber"),
            Id = std::ptr::null_mut(), i64 = 0, i64 = clicks, f32 = pressure);
        if ev.is_null() {
            return Err("NSEvent creation failed".to_string());
        }
        msg!((); wk, handler, Id = ev);
        Ok(())
    })?
}

pub fn key<R: Runtime>(
    win: &WebviewWindow<R>,
    down: bool,
    code: u16,
    chars: &str,
    flags: u64,
) -> Result<(), String> {
    let chars = chars.to_string();
    on_view(win, move |wk| unsafe {
        let window = msg!(Id; wk, "window");
        if window.is_null() {
            return Err("webview has no window".to_string());
        }
        let s = nsstring(&chars);
        let ev = msg!(Id; class("NSEvent"),
            "keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:",
            u64 = if down { 10 } else { 11 }, NSPoint = NSPoint { x: 0.0, y: 0.0 }, u64 = flags, f64 = uptime(),
            i64 = msg!(i64; window, "windowNumber"), Id = std::ptr::null_mut(), Id = s, Id = s, u8 = 0, u16 = code);
        if ev.is_null() {
            return Err("NSEvent creation failed".to_string());
        }
        msg!((); wk, if down { "keyDown:" } else { "keyUp:" }, Id = ev);
        Ok(())
    })?
}

pub fn wheel<R: Runtime>(
    win: &WebviewWindow<R>,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
) -> Result<(), String> {
    let viewport = css_viewport(win);
    on_view(win, move |wk| unsafe {
        let window = msg!(Id; wk, "window");
        if window.is_null() {
            return Err("webview has no window".to_string());
        }
        let wp = window_point(wk, viewport, x, y);
        let top = CGDisplayPixelsHigh(CGMainDisplayID()) as f64;
        // DOM convention: positive dy scrolls the page down; CG wheel is the inverse.
        let cg =
            CGEventCreateScrollWheelEvent2(std::ptr::null(), 0, 2, (-dy) as i32, (-dx) as i32, 0);
        if cg.is_null() {
            return Err("CGEvent creation failed".to_string());
        }
        // NSEvent built from a CGEvent has no window, so `locationInWindow` is the
        // raw global NS point. Feed it the window-space point directly (no screen
        // conversion) so WebKit maps it back to the intended viewport position.
        CGEventSetLocation(
            cg,
            NSPoint {
                x: wp.x,
                y: top - wp.y,
            },
        );
        let ev = msg!(Id; class("NSEvent"), "eventWithCGEvent:", *const c_void = cg);
        CFRelease(cg);
        if ev.is_null() {
            return Err("NSEvent from CGEvent failed".to_string());
        }
        msg!((); wk, "scrollWheel:", Id = ev);
        Ok(())
    })?
}

// ---- Objective-C blocks (stack block copied by WebKit; payload is a leaked Box) ----

#[repr(C)]
struct Desc {
    reserved: usize,
    size: usize,
}
#[repr(C)]
struct Blk {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: *const c_void,
    desc: *const Desc,
    data: *mut c_void,
}
static DESC: Desc = Desc {
    reserved: 0,
    size: std::mem::size_of::<Blk>(),
};

unsafe fn block(invoke: *const c_void, data: *mut c_void) -> Blk {
    Blk {
        isa: &raw const _NSConcreteStackBlock as *const c_void,
        flags: 0,
        reserved: 0,
        invoke,
        desc: &DESC,
        data,
    }
}

extern "C" fn eval_done(blk: *mut Blk, result: Id, err: Id) {
    unsafe {
        let tx = Box::from_raw((*blk).data as *mut Sender<Result<String, String>>);
        let out = if !err.is_null() {
            Err(utf8(msg!(Id; err, "localizedDescription"))
                .unwrap_or_else(|| "javascript error".into()))
        } else if !result.is_null() && msg!(bool; result, "isKindOfClass:", Id = class("NSString"))
        {
            Ok(utf8(result).unwrap_or_default())
        } else {
            Ok(String::new())
        };
        let _ = tx.send(out);
    }
}

extern "C" fn shot_done(blk: *mut Blk, image: Id, err: Id) {
    unsafe {
        let tx = Box::from_raw((*blk).data as *mut Sender<Result<Vec<u8>, String>>);
        let out = (|| {
            if image.is_null() || !err.is_null() {
                return Err(utf8(msg!(Id; err, "localizedDescription"))
                    .unwrap_or_else(|| "snapshot failed".into()));
            }
            let tiff = msg!(Id; image, "TIFFRepresentation");
            let rep = msg!(Id; class("NSBitmapImageRep"), "imageRepWithData:", Id = tiff);
            let png = msg!(Id; rep, "representationUsingType:properties:", u64 = 4, Id = std::ptr::null_mut());
            if png.is_null() {
                return Err("png encode failed".to_string());
            }
            let len = msg!(usize; png, "length");
            let p = msg!(*const u8; png, "bytes");
            Ok(std::slice::from_raw_parts(p, len).to_vec())
        })();
        let _ = tx.send(out);
    }
}

/// Evaluate JS in the page; the script must already produce a string.
pub fn eval<R: Runtime>(win: &WebviewWindow<R>, js: &str) -> Result<String, String> {
    let (tx, rx) = channel::<Result<String, String>>();
    let js = js.to_string();
    win.with_webview(move |pv| unsafe {
        let wk = pv.inner();
        let data = Box::into_raw(Box::new(tx)) as *mut c_void;
        let mut b = block(eval_done as *const c_void, data);
        msg!((); wk, "evaluateJavaScript:completionHandler:", Id = nsstring(&js), *mut Blk = &mut b);
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(15))
        .map_err(|_| "eval timed out".to_string())?
}

/// PNG of the webview contents via `takeSnapshotWithConfiguration:`.
pub fn screenshot<R: Runtime>(win: &WebviewWindow<R>) -> Result<Vec<u8>, String> {
    let (tx, rx) = channel::<Result<Vec<u8>, String>>();
    win.with_webview(move |pv| unsafe {
        let wk = pv.inner();
        let data = Box::into_raw(Box::new(tx)) as *mut c_void;
        let mut b = block(shot_done as *const c_void, data);
        msg!((); wk, "takeSnapshotWithConfiguration:completionHandler:", Id = std::ptr::null_mut(), *mut Blk = &mut b);
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(15))
        .map_err(|_| "screenshot timed out".to_string())?
}
