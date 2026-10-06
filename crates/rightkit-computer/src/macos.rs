//! macOS backend: public CoreFoundation / ApplicationServices / CoreGraphics
//! only. No private API in this tier (the SkyLight tier is a documented follow-up).

#![allow(non_snake_case, non_upper_case_globals, clippy::missing_safety_doc)]

use crate::model::*;
use std::ffi::c_void;
use std::os::raw::{c_char, c_double, c_int, c_long, c_ulong};

type CFTypeRef = *const c_void;
type CFIndex = c_long;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: c_double,
    y: c_double,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    w: c_double,
    h: c_double,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(c: CFTypeRef);
    fn CFGetTypeID(c: CFTypeRef) -> c_ulong;
    fn CFStringGetTypeID() -> c_ulong;
    fn CFArrayGetTypeID() -> c_ulong;
    fn CFNumberGetTypeID() -> c_ulong;
    fn CFBooleanGetTypeID() -> c_ulong;
    fn CFStringCreateWithBytes(alloc: CFTypeRef, bytes: *const u8, n: CFIndex, enc: u32, ext: u8) -> CFTypeRef;
    fn CFStringGetLength(s: CFTypeRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(len: CFIndex, enc: u32) -> CFIndex;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, enc: u32) -> u8;
    fn CFArrayGetCount(a: CFTypeRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: CFIndex) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(n: CFTypeRef, ty: c_int, out: *mut c_void) -> u8;
    fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    static kCFBooleanTrue: CFTypeRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementCreateApplication(pid: c_int) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> c_int;
    fn AXUIElementSetAttributeValue(el: CFTypeRef, attr: CFTypeRef, v: CFTypeRef) -> c_int;
    fn AXUIElementCopyActionNames(el: CFTypeRef, out: *mut CFTypeRef) -> c_int;
    fn AXUIElementPerformAction(el: CFTypeRef, action: CFTypeRef) -> c_int;
    fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> c_int;
    fn AXValueGetValue(v: CFTypeRef, ty: c_int, out: *mut c_void) -> u8;

    fn CGWindowListCopyWindowInfo(opt: u32, rel: u32) -> CFTypeRef;
    fn CGPreflightScreenCaptureAccess() -> u8;
    fn CGEventSourceCreate(state: c_int) -> CFTypeRef;
    fn CGEventCreateKeyboardEvent(src: CFTypeRef, key: u16, down: u8) -> CFTypeRef;
    fn CGEventKeyboardSetUnicodeString(ev: CFTypeRef, n: c_ulong, s: *const u16);
    fn CGEventSetFlags(ev: CFTypeRef, flags: u64);
    fn CGEventPostToPid(pid: c_int, ev: CFTypeRef);
}

const UTF8: u32 = 0x0800_0100;

/// Owned CF reference, released on drop.
struct Cf(CFTypeRef);
impl Cf {
    fn new(r: CFTypeRef) -> Option<Cf> {
        if r.is_null() { None } else { Some(Cf(r)) }
    }
    fn string(s: &str) -> Cf {
        Cf(unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as CFIndex, UTF8, 0) })
    }
}
impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

fn to_string(r: CFTypeRef) -> Option<String> {
    unsafe {
        if r.is_null() || CFGetTypeID(r) != CFStringGetTypeID() {
            return None;
        }
        let max = CFStringGetMaximumSizeForEncoding(CFStringGetLength(r), UTF8) + 1;
        let mut buf = vec![0 as c_char; max as usize];
        if CFStringGetCString(r, buf.as_mut_ptr(), max, UTF8) == 0 {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
    }
}
fn to_i64(r: CFTypeRef) -> Option<i64> {
    unsafe {
        if r.is_null() || CFGetTypeID(r) != CFNumberGetTypeID() {
            return None;
        }
        let mut v: i64 = 0;
        (CFNumberGetValue(r, 4, &mut v as *mut _ as *mut c_void) != 0).then_some(v)
    }
}
fn to_f64(r: CFTypeRef) -> Option<f64> {
    unsafe {
        let mut v: f64 = 0.0;
        (!r.is_null() && CFNumberGetValue(r, 6, &mut v as *mut _ as *mut c_void) != 0).then_some(v)
    }
}
fn to_bool(r: CFTypeRef) -> Option<bool> {
    unsafe { (!r.is_null() && CFGetTypeID(r) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(r) != 0) }
}
fn dict_get(d: CFTypeRef, key: &str) -> CFTypeRef {
    let k = Cf::string(key);
    unsafe { CFDictionaryGetValue(d, k.0) }
}

// ---------------------------------------------------------------- status

pub fn status() -> Status {
    unsafe { Status { accessibility: AXIsProcessTrusted() != 0, screen_recording: CGPreflightScreenCaptureAccess() != 0 } }
}

// ---------------------------------------------------------------- windows

/// All windows, front to back (CoreGraphics order). `on_screen_only=false` includes minimized/other-space.
pub fn list_windows(on_screen_only: bool) -> Vec<WindowInfo> {
    let opt = if on_screen_only { 1 | 16 } else { 16 };
    let Some(arr) = Cf::new(unsafe { CGWindowListCopyWindowInfo(opt, 0) }) else { return vec![] };
    let mut out = vec![];
    unsafe {
        for i in 0..CFArrayGetCount(arr.0) {
            let d = CFArrayGetValueAtIndex(arr.0, i);
            let b = dict_get(d, "kCGWindowBounds");
            let num = |k: &str| to_f64(dict_get(b, k)).unwrap_or(0.0);
            out.push(WindowInfo {
                window_id: to_i64(dict_get(d, "kCGWindowNumber")).unwrap_or(0) as u32,
                pid: to_i64(dict_get(d, "kCGWindowOwnerPID")).unwrap_or(0) as i32,
                owner: to_string(dict_get(d, "kCGWindowOwnerName")).unwrap_or_default(),
                title: to_string(dict_get(d, "kCGWindowName")).unwrap_or_default(),
                layer: to_i64(dict_get(d, "kCGWindowLayer")).unwrap_or(0) as i32,
                on_screen: to_bool(dict_get(d, "kCGWindowIsOnscreen")).unwrap_or(false),
                bounds: Rect { x: num("X"), y: num("Y"), w: num("Width"), h: num("Height") },
            });
        }
    }
    out
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}
#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> *const c_void;
    fn sel_registerName(name: *const c_char) -> *const c_void;
    fn objc_msgSend();
}

/// Frontmost application pid via NSWorkspace (system-wide AX focus query is
/// unreliable: returns kAXErrorCannotComplete on current macOS).
pub fn frontmost_pid() -> Option<i32> {
    unsafe {
        type Obj = unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void;
        type Int = unsafe extern "C" fn(*const c_void, *const c_void) -> c_int;
        let send_obj: Obj = std::mem::transmute(objc_msgSend as *const ());
        let send_int: Int = std::mem::transmute(objc_msgSend as *const ());
        let ws = send_obj(objc_getClass(c"NSWorkspace".as_ptr()), sel_registerName(c"sharedWorkspace".as_ptr()));
        let app = send_obj(ws, sel_registerName(c"frontmostApplication".as_ptr()));
        (!app.is_null()).then(|| send_int(app, sel_registerName(c"processIdentifier".as_ptr())))
    }
}

// ---------------------------------------------------------------- AX elements

pub struct El(Cf);

impl El {
    fn attr_of(el: CFTypeRef, name: &str) -> Option<El> {
        let k = Cf::string(name);
        let mut out: CFTypeRef = std::ptr::null();
        let e = unsafe { AXUIElementCopyAttributeValue(el, k.0, &mut out) };
        if e == 0 { Cf::new(out).map(El) } else { None }
    }
    pub fn app(pid: i32) -> Result<El> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return Err(Error::PermissionDenied("Accessibility"));
        }
        let r = Cf::new(unsafe { AXUIElementCreateApplication(pid) }).ok_or_else(|| Error::NotFound(format!("pid {pid}")))?;
        unsafe { AXUIElementSetMessagingTimeout(r.0, 2.0) };
        Ok(El(r))
    }
    fn raw(&self, name: &str) -> Option<El> {
        El::attr_of((self.0).0, name)
    }
    pub fn pid(&self) -> Option<i32> {
        extern "C" {
            fn AXUIElementGetPid(el: CFTypeRef, pid: *mut c_int) -> c_int;
        }
        let mut p = 0;
        (unsafe { AXUIElementGetPid((self.0).0, &mut p) } == 0).then_some(p)
    }
    pub fn str_attr(&self, name: &str) -> Option<String> {
        self.raw(name).and_then(|e| to_string((e.0).0))
    }
    pub fn bool_attr(&self, name: &str) -> Option<bool> {
        self.raw(name).and_then(|e| to_bool((e.0).0))
    }
    pub fn role(&self) -> String {
        self.str_attr("AXRole").unwrap_or_default()
    }
    pub fn children(&self) -> Vec<El> {
        self.list("AXChildren")
    }
    pub fn list(&self, name: &str) -> Vec<El> {
        let Some(a) = self.raw(name) else { return vec![] };
        unsafe {
            if CFGetTypeID((a.0).0) != CFArrayGetTypeID() {
                return vec![];
            }
            (0..CFArrayGetCount((a.0).0))
                .filter_map(|i| {
                    let v = CFArrayGetValueAtIndex((a.0).0, i);
                    // Array owns v; retain through a second reference.
                    extern "C" {
                        fn CFRetain(c: CFTypeRef) -> CFTypeRef;
                    }
                    Cf::new(CFRetain(v)).map(El)
                })
                .collect()
        }
    }
    pub fn actions(&self) -> Vec<String> {
        let mut out: CFTypeRef = std::ptr::null();
        if unsafe { AXUIElementCopyActionNames((self.0).0, &mut out) } != 0 {
            return vec![];
        }
        let Some(a) = Cf::new(out) else { return vec![] };
        unsafe { (0..CFArrayGetCount(a.0)).filter_map(|i| to_string(CFArrayGetValueAtIndex(a.0, i))).collect() }
    }
    pub fn frame(&self) -> Option<Rect> {
        let (p, s) = (self.raw("AXPosition")?, self.raw("AXSize")?);
        let (mut pt, mut sz) = (CGPoint { x: 0.0, y: 0.0 }, CGSize { w: 0.0, h: 0.0 });
        unsafe {
            (AXValueGetValue((p.0).0, 1, &mut pt as *mut _ as *mut c_void) != 0
                && AXValueGetValue((s.0).0, 2, &mut sz as *mut _ as *mut c_void) != 0)
                .then_some(Rect { x: pt.x, y: pt.y, w: sz.w, h: sz.h })
        }
    }
    /// No pointer, no focus change: asks the app to perform AXPress etc.
    pub fn perform(&self, action: &str) -> Result<()> {
        let a = Cf::string(action);
        match unsafe { AXUIElementPerformAction((self.0).0, a.0) } {
            0 => Ok(()),
            c => Err(Error::Ax(c, "perform")),
        }
    }
    pub fn press(&self) -> Result<()> {
        self.perform("AXPress")
    }
    pub fn set_string(&self, attr: &str, v: &str) -> Result<()> {
        let (k, s) = (Cf::string(attr), Cf::string(v));
        match unsafe { AXUIElementSetAttributeValue((self.0).0, k.0, s.0) } {
            0 => Ok(()),
            c => Err(Error::Ax(c, "set_value")),
        }
    }
    pub fn set_value(&self, v: &str) -> Result<()> {
        self.set_string("AXValue", v)
    }
    pub fn set_bool(&self, attr: &str, v: bool) -> Result<()> {
        let k = Cf::string(attr);
        let b = if v { unsafe { kCFBooleanTrue } } else { return Err(Error::Unsupported("false bool")) };
        match unsafe { AXUIElementSetAttributeValue((self.0).0, k.0, b) } {
            0 => Ok(()),
            c => Err(Error::Ax(c, "set_bool")),
        }
    }
    /// Depth-first search.
    pub fn find(&self, depth: usize, pred: &dyn Fn(&El) -> bool) -> Option<El> {
        for c in self.children() {
            if pred(&c) {
                return Some(c);
            }
            if depth > 0 {
                if let Some(f) = c.find(depth - 1, pred) {
                    return Some(f);
                }
            }
        }
        None
    }
    fn snapshot(&self, depth: usize, budget: &mut usize) -> Node {
        *budget = budget.saturating_sub(1);
        let mut n = Node {
            role: self.role(),
            subrole: self.str_attr("AXSubrole"),
            title: self.str_attr("AXTitle").filter(|s| !s.is_empty()),
            value: self.str_attr("AXValue").filter(|s| !s.is_empty()),
            description: self.str_attr("AXDescription").filter(|s| !s.is_empty()),
            enabled: self.bool_attr("AXEnabled"),
            frame: self.frame(),
            actions: self.actions().into_iter().map(|a| a.trim_start_matches("AX").to_string()).collect(),
            children: vec![],
        };
        if depth > 0 {
            for c in self.children() {
                if *budget == 0 {
                    break;
                }
                n.children.push(c.snapshot(depth - 1, budget));
            }
        }
        n
    }
    /// Walk a menu bar path ("File","New") and AXPress the leaf WITHOUT opening the menu.
    pub fn press_menu(&self, path: &[&str]) -> Result<()> {
        let mut cur = self.raw("AXMenuBar").ok_or_else(|| Error::NotFound("menu bar".into()))?;
        for (i, want) in path.iter().enumerate() {
            let item = cur
                .children()
                .into_iter()
                .find(|c| c.str_attr("AXTitle").as_deref() == Some(want))
                .ok_or_else(|| Error::NotFound(format!("menu item {want}")))?;
            if i + 1 == path.len() {
                return item.press();
            }
            cur = item.children().into_iter().next().ok_or_else(|| Error::NotFound(format!("submenu {want}")))?;
        }
        Err(Error::NotFound("empty menu path".into()))
    }
}

/// Bounded accessibility snapshot of an app (windows first).
pub fn observe_tree(pid: i32, max_depth: usize, max_nodes: usize) -> Result<Node> {
    let app = El::app(pid)?;
    let mut budget = max_nodes;
    Ok(app.snapshot(max_depth, &mut budget))
}

// ---------------------------------------------------------------- keys

/// Type text to one process via CGEventPostToPid. Does not move the pointer or
/// change the frontmost app; delivery depends on the target's key window.
pub fn type_text_to_pid(pid: i32, text: &str) -> Result<()> {
    let src = Cf::new(unsafe { CGEventSourceCreate(-1) }).ok_or(Error::Unsupported("event source"))?; // private state: ignore user's held modifiers
    for ch in text.chars() {
        let mut buf = [0u16; 2];
        let u = ch.encode_utf16(&mut buf);
        for down in [1u8, 0u8] {
            let ev = Cf::new(unsafe { CGEventCreateKeyboardEvent(src.0, 0, down) }).ok_or(Error::Unsupported("key event"))?;
            unsafe {
                CGEventKeyboardSetUnicodeString(ev.0, u.len() as c_ulong, u.as_ptr());
                CGEventSetFlags(ev.0, 0);
                CGEventPostToPid(pid, ev.0);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    Ok(())
}

// ---------------------------------------------------------------- capture

/// Per-window PNG. Prototype shells to `screencapture -l` (works for background
/// windows, honours Screen Recording permission). Production: ScreenCaptureKit
/// SCContentFilter(desktopIndependentWindow:) via a small Swift/objc2 shim.
pub fn capture_window(window_id: u32, path: &std::path::Path) -> Result<()> {
    if unsafe { CGPreflightScreenCaptureAccess() } == 0 {
        return Err(Error::PermissionDenied("Screen Recording"));
    }
    let ok = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-o", "-l", &window_id.to_string()])
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && path.exists() { Ok(()) } else { Err(Error::Unsupported("screencapture failed")) }
}
