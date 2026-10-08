//! macOS in-process input: synthesized `NSEvent`s dispatched straight to the
//! `WKWebView` responder methods. Nothing here activates the app or raises the
//! window, and AppKit's inactive-window first-mouse swallow is bypassed
//! because the window's `sendEvent:` is never involved. Raw Objective-C
//! runtime FFI keeps the crate free of extra dependencies.
//! During pointer dispatch only, AppKit's process-local button query reports
//! the automation button mask and WKWebView skips asynchronous text-input
//! interception. Both hooks delegate unchanged outside that thread-local scope.
#![allow(non_snake_case, clippy::missing_safety_doc)]

use super::{Button, Mouse};
use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
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
    fn class_getClassMethod(class: Id, name: Sel) -> Id;
    fn class_getInstanceMethod(class: Id, name: Sel) -> Id;
    fn method_getImplementation(method: Id) -> *const c_void;
    fn method_setImplementation(method: Id, imp: *const c_void) -> *const c_void;
    fn object_getClass(obj: Id) -> Id;
    fn object_setClass(obj: Id, class: Id) -> Id;
    fn objc_allocateClassPair(base: Id, name: *const c_char, extra: usize) -> Id;
    fn objc_registerClassPair(class: Id);
    fn objc_disposeClassPair(class: Id);
    fn class_addMethod(class: Id, name: Sel, imp: *const c_void, types: *const c_char) -> bool;
    fn method_getTypeEncoding(method: Id) -> *const c_char;
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
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
    (NSRect; $obj:expr, $sel:expr $(, $t:ty = $a:expr)*) => {{
        // NSRect is returned indirectly on Intel, directly on Apple Silicon.
        #[cfg(target_arch = "x86_64")]
        {
            let mut rect = std::mem::MaybeUninit::<NSRect>::uninit();
            let f: unsafe extern "C" fn(*mut NSRect, Id, Sel $(, $t)*) =
                std::mem::transmute(objc_msgSend_stret as *const c_void);
            f(rect.as_mut_ptr(), $obj, sel($sel) $(, $a)*);
            rect.assume_init()
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let f: unsafe extern "C" fn(Id, Sel $(, $t)*) -> NSRect =
                std::mem::transmute(objc_msgSend as *const c_void);
            f($obj, sel($sel) $(, $a)*)
        }
    }};
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

thread_local! {
    // WebKit's UserDriven NSEvent path reads this class method, rather than
    // deriving `buttons` from the event type. Never change HID state to supply it.
    static POINTER_BUTTONS: Cell<Option<usize>> = const { Cell::new(None) };
    static POINTER_VIEW: Cell<Id> = const { Cell::new(std::ptr::null_mut()) };
}
static POINTER_HOOKS: OnceLock<Result<(), String>> = OnceLock::new();
static ORIGINAL_BUTTONS: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_INPUT_CONTEXT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn pressed_mouse_buttons(obj: Id, selector: Sel) -> usize {
    if let Ok(Some(buttons)) = POINTER_BUTTONS.try_with(Cell::get) {
        return buttons;
    }
    // Installation stores the original IMP before exposing this callback.
    unsafe {
        let imp = ORIGINAL_BUTTONS.load(Ordering::Acquire);
        let original: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(imp);
        original(obj, selector)
    }
}

extern "C" fn pointer_input_context(obj: Id, selector: Sel) -> Id {
    if POINTER_VIEW.try_with(Cell::get).ok() == Some(obj) {
        // Editable content otherwise routes pointer events through an asynchronous
        // NSTextInputContext callback, after the scoped button state has expired.
        // Pointer dispatch does not compose text; key/IME calls keep the original.
        return std::ptr::null_mut();
    }
    unsafe {
        let original: unsafe extern "C" fn(Id, Sel) -> Id =
            std::mem::transmute(ORIGINAL_INPUT_CONTEXT.load(Ordering::Acquire));
        original(obj, selector)
    }
}

/// Only the app's main-thread native dispatch observes the supplied button mask.
/// Other threads and normal user events continue calling AppKit's original IMP.
struct PointerButtons(Option<usize>, Id);
impl PointerButtons {
    unsafe fn enter(wk: Id, buttons: usize) -> Result<Self, String> {
        // on_view serializes installation on the AppKit main thread. Store the
        // original before installing the replacement (including for other threads).
        let hooks = POINTER_HOOKS.get_or_init(|| {
            let method = class_getClassMethod(class("NSEvent"), sel("pressedMouseButtons"));
            let context = class_getInstanceMethod(class("WKWebView"), sel("inputContext"));
            if method.is_null() || context.is_null() {
                return Err("AppKit pointer state methods unavailable".into());
            }
            let imp = method_getImplementation(method);
            let context_imp = method_getImplementation(context);
            if imp.is_null() || context_imp.is_null() {
                return Err("AppKit pointer state has no implementation".into());
            }
            ORIGINAL_BUTTONS.store(imp as usize, Ordering::Release);
            ORIGINAL_INPUT_CONTEXT.store(context_imp as usize, Ordering::Release);
            method_setImplementation(method, pressed_mouse_buttons as *const c_void);
            method_setImplementation(context, pointer_input_context as *const c_void);
            Ok(())
        });
        hooks.as_ref().map_err(Clone::clone)?;
        Ok(Self(
            POINTER_BUTTONS.with(|state| state.replace(Some(buttons))),
            POINTER_VIEW.with(|state| state.replace(wk)),
        ))
    }
}
impl Drop for PointerButtons {
    fn drop(&mut self) {
        POINTER_BUTTONS.with(|state| state.set(self.0));
        POINTER_VIEW.with(|state| state.set(self.1));
    }
}

fn mouse_spec(kind: Mouse, button: Button) -> (u64, &'static str, f32, usize) {
    match (kind, button) {
        (Mouse::Down, Button::Left) => (1, "mouseDown:", 1.0, 1),
        (Mouse::Up, Button::Left) => (2, "mouseUp:", 0.0, 0),
        (Mouse::Down, Button::Right) => (3, "rightMouseDown:", 1.0, 2),
        (Mouse::Up, Button::Right) => (4, "rightMouseUp:", 0.0, 0),
        // A button-less drag bypasses WebKit's inactive tracking-area checks.
        (Mouse::Move, _) => (6, "mouseDragged:", 0.0, 0),
        (Mouse::Drag, Button::Left) => (6, "mouseDragged:", 1.0, 1),
        (Mouse::Drag, Button::Right) => (7, "rightMouseDragged:", 1.0, 2),
    }
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
    parse_css_viewport(&eval(win, CSS_VIEWPORT).ok()?).ok()
}

const CSS_VIEWPORT: &str = "innerWidth + ',' + innerHeight";
const VIEWPORT_TIMEOUT: Duration = Duration::from_secs(2);

fn parse_css_viewport(s: &str) -> Result<(f64, f64), String> {
    let invalid = || format!("invalid CSS viewport readback: {s:?}");
    let (w, h) = s.split_once(',').ok_or_else(invalid)?;
    let size: (f64, f64) = (
        w.trim().parse().map_err(|_| invalid())?,
        h.trim().parse().map_err(|_| invalid())?,
    );
    if !size.0.is_finite() || !size.1.is_finite() || size.0 < 0.0 || size.1 < 0.0 {
        return Err(invalid());
    }
    Ok(size)
}

/// The caller waits off the main thread. Zero/stale dimensions and transient JS
/// failures must not become inputs to another resize. Each evaluation shares the
/// deadline, including a missing WebKit completion callback.
fn poll_css_viewport(
    target: Option<(f64, f64)>,
    mut read: impl FnMut(Duration) -> Result<(f64, f64), String>,
    mut elapsed: impl FnMut() -> Duration,
    mut wait: impl FnMut(Duration),
) -> Result<(f64, f64), String> {
    let mut measured = None;
    let mut last_error = None;
    loop {
        let remaining = VIEWPORT_TIMEOUT.saturating_sub(elapsed());
        if remaining.is_zero() {
            let requested = target
                .map(|(w, h)| format!("requested {w}x{h}, "))
                .unwrap_or_default();
            let actual = measured
                .map(|(w, h)| format!("measured {w}x{h}"))
                .unwrap_or_else(|| "no CSS viewport measurement".into());
            let error = last_error.map(|e| format!("; {e}")).unwrap_or_default();
            return Err(format!(
                "CSS viewport readback timed out: {requested}{actual}{error}"
            ));
        }
        match read(remaining.min(Duration::from_millis(250))) {
            Ok(size) => {
                measured = Some(size);
                if size.0 > 0.0 && size.1 > 0.0 && target.is_none_or(|t| size == t) {
                    return Ok(size);
                }
            }
            Err(e) => last_error = Some(e),
        }
        wait(Duration::from_millis(20).min(VIEWPORT_TIMEOUT.saturating_sub(elapsed())));
    }
}

fn wait_css_viewport<R: Runtime>(
    win: &WebviewWindow<R>,
    target: Option<(f64, f64)>,
) -> Result<(f64, f64), String> {
    let start = Instant::now();
    poll_css_viewport(
        target,
        |timeout| parse_css_viewport(&eval_with_timeout(win, CSS_VIEWPORT, timeout)?),
        || start.elapsed(),
        std::thread::sleep,
    )
}

extern "C" fn unconstrained_frame(_: Id, _: Sel, rect: NSRect, _: Id) -> NSRect {
    rect
}

/// An ivar-free subclass affects only this window during setFrame. Restore its
/// original class on every Rust exit; no process-wide swizzle or focus change.
struct UnconstrainedWindow(Id, Id);
impl UnconstrainedWindow {
    unsafe fn enter(window: Id) -> Result<Self, String> {
        let base = object_getClass(window);
        let name = CString::new(format!("RightKitUnconstrained_{:x}", base as usize)).unwrap();
        let mut subclass = objc_getClass(name.as_ptr());
        if subclass.is_null() {
            let selector = sel("constrainFrameRect:toScreen:");
            let method = class_getInstanceMethod(base, selector);
            if method.is_null() {
                return Err("NSWindow frame constraint method unavailable".into());
            }
            subclass = objc_allocateClassPair(base, name.as_ptr(), 0);
            if subclass.is_null() {
                return Err("NSWindow resize subclass allocation failed".into());
            }
            if !class_addMethod(
                subclass,
                selector,
                unconstrained_frame as *const c_void,
                method_getTypeEncoding(method),
            ) {
                objc_disposeClassPair(subclass);
                return Err("NSWindow resize override installation failed".into());
            }
            objc_registerClassPair(subclass);
        }
        object_setClass(window, subclass);
        Ok(Self(window, base))
    }
}
impl Drop for UnconstrainedWindow {
    fn drop(&mut self) {
        unsafe {
            object_setClass(self.0, self.1);
        }
    }
}

/// Set native content size without visible-screen clamping, then measure CSS.
/// Layout/zoom/titlebar insets are measured rather than assuming frame == viewport.
pub fn set_viewport<R: Runtime>(
    win: &WebviewWindow<R>,
    width: u32,
    height: u32,
) -> Result<(u32, u32), String> {
    let target = (width as f64, height as f64);
    let viewport = wait_css_viewport(win, None)?;
    on_view(win, move |wk| unsafe {
        let window = msg!(Id; wk, "window");
        if window.is_null() {
            return Err("webview has no window".to_string());
        }
        let bounds = msg!(NSRect; wk, "bounds");
        let mut view_frame = msg!(NSRect; wk, "frame");
        let frame = msg!(NSRect; window, "frame");
        let mut content = msg!(NSRect; window, "contentRectForFrameRect:", NSRect = frame);
        let scale = bounds.size.width / viewport.0;
        let dw = (target.0 - viewport.0) * scale;
        let dh = (target.1 - viewport.1) * scale;
        content.size.width += dw;
        content.size.height += dh;
        view_frame.size.width += dw;
        view_frame.size.height += dh;
        if !scale.is_finite()
            || scale <= 0.0
            || view_frame.size.width <= 0.0
            || view_frame.size.height <= 0.0
        {
            return Err("invalid native webview bounds for viewport resize".into());
        }
        let frame = msg!(NSRect; window, "frameRectForContentRect:", NSRect = content);
        {
            let _scope = UnconstrainedWindow::enter(window)?;
            // display:NO never orders, activates, or makes the window key.
            msg!((); window, "setFrame:display:", NSRect = frame, bool = false);
        }
        // Hidden windows may defer Tauri's resize notification. Explicitly
        // update this view too; preserve its origin, zoom and content insets.
        let content_view = msg!(Id; window, "contentView");
        msg!((); content_view, "layoutSubtreeIfNeeded");
        msg!((); wk, "setFrame:", NSRect = view_frame);
        msg!((); wk, "layoutSubtreeIfNeeded");
        Ok::<(), String>(())
    })??;
    // Return from the AppKit block before scheduling/waiting on WebKit's JS.
    // Hidden pages throttle rAF; poll real page dimensions on the service thread.
    let measured = wait_css_viewport(win, Some(target))?;
    Ok((measured.0 as u32, measured.1 as u32))
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
        // Tauri marshals with_webview onto its event loop; verify that assumption
        // before touching AppKit or installing the scoped pointer state.
        if !unsafe { msg!(bool; class("NSThread"), "isMainThread") } {
            let _ = tx.send(Err("AppKit control requires the main thread".into()));
            return;
        }
        let _ = tx.send(Ok(f(pv.inner())));
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|_| "webview call timed out".to_string())?
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
        // Hover is delivered as a button-less `mouseDragged:` on the WKWebView itself:
        // WebKit gates `mouseMoved:` (tracking-area path) on an active window, but the
        // WKWebView responder path works in an inactive window and the page still sees
        // a plain `mousemove` with `buttons == 0`, so `:hover` and `mouseenter` update.
        // NSEventType: 1/2 left down/up, 3/4 right down/up, 5 moved, 6/7 left/right dragged.
        let (ty, handler, pressure, buttons) = mouse_spec(kind, button);
        let ev = msg!(Id; class("NSEvent"),
            "mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:",
            u64 = ty, NSPoint = wp, u64 = flags, f64 = uptime(), i64 = msg!(i64; window, "windowNumber"),
            Id = std::ptr::null_mut(), i64 = 0, i64 = clicks, f32 = pressure);
        if ev.is_null() {
            return Err("NSEvent creation failed".to_string());
        }
        let _buttons = PointerButtons::enter(wk, buttons)?;
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
    eval_with_timeout(win, js, Duration::from_secs(15))
}

fn eval_with_timeout<R: Runtime>(
    win: &WebviewWindow<R>,
    js: &str,
    timeout: Duration,
) -> Result<String, String> {
    let (tx, rx) = channel::<Result<String, String>>();
    let js = js.to_string();
    win.with_webview(move |pv| unsafe {
        let wk = pv.inner();
        let data = Box::into_raw(Box::new(tx)) as *mut c_void;
        let mut b = block(eval_done as *const c_void, data);
        msg!((); wk, "evaluateJavaScript:completionHandler:", Id = nsstring(&js), *mut Blk = &mut b);
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(timeout)
        .map_err(|_| "eval timed out".to_string())?
}

/// PNG of the webview contents via `takeSnapshotWithConfiguration:`.
pub fn screenshot<R: Runtime>(win: &WebviewWindow<R>) -> Result<Vec<u8>, String> {
    let (tx, rx) = channel::<Result<Vec<u8>, String>>();
    on_view(win, move |wk| unsafe {
        let config = msg!(Id; class("WKSnapshotConfiguration"), "new");
        let bounds = msg!(NSRect; wk, "bounds");
        msg!((); config, "setRect:", NSRect = bounds);
        let data = Box::into_raw(Box::new(tx)) as *mut c_void;
        let mut b = block(shot_done as *const c_void, data);
        msg!((); wk, "takeSnapshotWithConfiguration:completionHandler:", Id = config, *mut Blk = &mut b);
        msg!((); config, "release");
    })?;
    rx.recv_timeout(Duration::from_secs(15))
        .map_err(|_| "screenshot timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scripted_readback(
        target: Option<(f64, f64)>,
        samples: &[Result<&str, &str>],
    ) -> (Result<(f64, f64), String>, Duration, Vec<Duration>) {
        let clock = Cell::new(Duration::ZERO);
        let mut samples_iter = samples.iter();
        let mut timeouts = Vec::new();
        let result = poll_css_viewport(
            target,
            |timeout| {
                timeouts.push(timeout);
                let sample = samples_iter
                    .next()
                    .unwrap_or_else(|| samples.last().unwrap());
                match sample {
                    Ok(s) => parse_css_viewport(s),
                    Err(e) => {
                        if *e == "eval timed out" {
                            clock.set(clock.get() + timeout);
                        }
                        Err(e.to_string())
                    }
                }
            },
            || clock.get(),
            |delay| clock.set(clock.get() + delay),
        );
        (result, clock.get(), timeouts)
    }

    #[test]
    fn css_readback_parses_real_dimensions_and_rejects_invalid_values() {
        assert_eq!(
            parse_css_viewport(" 1440, 1000 ").unwrap(),
            (1440.0, 1000.0)
        );
        assert_eq!(parse_css_viewport("0,0").unwrap(), (0.0, 0.0));
        for s in [
            "", "1440", "1440,", "null", "NaN,1000", "1440,inf", "-1,1000",
        ] {
            assert!(parse_css_viewport(s).is_err(), "{s}");
        }
    }

    #[test]
    fn css_readback_retries_empty_errors_and_stale_layout_until_target() {
        let samples = [
            Ok("0,0"),
            Ok(""),
            Err("javascript error"),
            Ok("900,640"),
            Ok("1440,0"),
            Ok("1440,1000"),
        ];
        let (result, elapsed, attempts) = scripted_readback(Some((1440.0, 1000.0)), &samples);
        assert_eq!(result.unwrap(), (1440.0, 1000.0));
        assert_eq!(attempts.len(), samples.len());
        assert_eq!(elapsed, Duration::from_millis(100));
    }

    #[test]
    fn css_initial_readback_waits_for_nonzero_dimensions() {
        let (result, _, attempts) = scripted_readback(None, &[Ok("0,0"), Ok("900,640")]);
        assert_eq!(result.unwrap(), (900.0, 640.0));
        assert_eq!(attempts.len(), 2);
    }

    #[test]
    fn css_readback_timeout_reports_measured_mismatch_and_empty_viewport() {
        for (sample, measured) in [("900,640", "measured 900x640"), ("0,0", "measured 0x0")] {
            let (result, elapsed, _) = scripted_readback(Some((1440.0, 1000.0)), &[Ok(sample)]);
            let error = result.unwrap_err();
            assert!(error.contains("requested 1440x1000"), "{error}");
            assert!(error.contains(measured), "{error}");
            assert_eq!(elapsed, VIEWPORT_TIMEOUT);
        }
    }

    #[test]
    fn css_missing_callbacks_share_deadline_instead_of_waiting_fifteen_seconds() {
        let (result, elapsed, timeouts) =
            scripted_readback(Some((1440.0, 1000.0)), &[Err("eval timed out")]);
        let error = result.unwrap_err();
        assert!(error.contains("no CSS viewport measurement"), "{error}");
        assert!(error.contains("eval timed out"), "{error}");
        assert_eq!(elapsed, VIEWPORT_TIMEOUT);
        assert_eq!(timeouts.len(), 8);
        assert_eq!(timeouts.last(), Some(&Duration::from_millis(110)));
    }

    #[test]
    fn css_failed_readback_preserves_last_measured_dimensions() {
        let (result, elapsed, _) = scripted_readback(
            Some((1440.0, 1000.0)),
            &[Ok("900,640"), Err("eval timed out")],
        );
        let error = result.unwrap_err();
        assert!(error.contains("measured 900x640"), "{error}");
        assert!(error.contains("eval timed out"), "{error}");
        assert_eq!(elapsed, VIEWPORT_TIMEOUT);
    }

    #[test]
    fn resize_override_is_scoped_to_one_instance_and_restores_class() {
        // NSObject stand-ins exercise Objective-C dispatch/ABI without WindowServer.
        extern "C" fn constrained(_: Id, _: Sel, mut rect: NSRect, _: Id) -> NSRect {
            rect.size.height = 789.0;
            rect
        }
        unsafe {
            let selector = sel("constrainFrameRect:toScreen:");
            let base = objc_allocateClassPair(class("NSObject"), c"RightKitResizeTest".as_ptr(), 0);
            assert!(!base.is_null());
            let method = class_getInstanceMethod(class("NSWindow"), selector);
            assert!(!method.is_null());
            assert!(class_addMethod(
                base,
                selector,
                constrained as *const c_void,
                method_getTypeEncoding(method)
            ));
            objc_registerClassPair(base);
            let first = msg!(Id; base, "new");
            let second = msg!(Id; base, "new");
            let rect = NSRect {
                origin: NSPoint { x: 13.0, y: 17.0 },
                size: NSSize {
                    width: 1440.0,
                    height: 1000.0,
                },
            };
            let read = |obj| msg!(NSRect; obj, "constrainFrameRect:toScreen:", NSRect = rect, Id = std::ptr::null_mut());
            assert_eq!(read(first).size.height, 789.0);
            {
                let _scope = UnconstrainedWindow::enter(first).unwrap();
                let actual = read(first);
                assert_eq!(actual.size.height, 1000.0);
                assert_eq!(actual.size.width, 1440.0);
                assert_eq!((actual.origin.x, actual.origin.y), (13.0, 17.0));
                assert_eq!(read(second).size.height, 789.0);
                assert_eq!(object_getClass(second), base);
            }
            assert_eq!(object_getClass(first), base);
            assert_eq!(read(first).size.height, 789.0);
            msg!((); first, "release");
            msg!((); second, "release");
        }
    }

    #[test]
    fn native_pointer_buttons_include_press_drag_and_release() {
        for (button, mask) in [(Button::Left, 1), (Button::Right, 2)] {
            assert_eq!(mouse_spec(Mouse::Down, button).3, mask);
            assert_eq!(mouse_spec(Mouse::Drag, button).3, mask);
            assert_eq!(mouse_spec(Mouse::Up, button).3, 0);
            assert_eq!(mouse_spec(Mouse::Move, button).3, 0);
        }
    }

    #[test]
    fn pointer_state_is_nested_and_thread_local() {
        // Exercise scope restoration without installing AppKit hooks in the test runner.
        assert_eq!(POINTER_BUTTONS.with(Cell::get), None);
        let view = 1usize as Id;
        let outer = PointerButtons(
            POINTER_BUTTONS.with(|s| s.replace(Some(1))),
            POINTER_VIEW.with(|s| s.replace(view)),
        );
        let inner = PointerButtons(
            POINTER_BUTTONS.with(|s| s.replace(Some(0))),
            POINTER_VIEW.with(|s| s.replace(std::ptr::null_mut())),
        );
        assert_eq!(POINTER_BUTTONS.with(Cell::get), Some(0));
        drop(inner);
        assert_eq!(POINTER_BUTTONS.with(Cell::get), Some(1));
        assert_eq!(POINTER_VIEW.with(Cell::get), view);
        std::thread::spawn(|| {
            assert_eq!(POINTER_BUTTONS.with(Cell::get), None);
            assert!(POINTER_VIEW.with(Cell::get).is_null());
        })
        .join()
        .unwrap();
        drop(outer);
        assert_eq!(POINTER_BUTTONS.with(Cell::get), None);
        assert!(POINTER_VIEW.with(Cell::get).is_null());
    }
}
