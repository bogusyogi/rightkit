//! macOS backend: window discovery, per-pid input, Accessibility reads.
//! All input goes through `CGEventPostToPid`; nothing here activates an app,
//! moves the real cursor, or posts to the global HID tap.
#![allow(non_snake_case, non_camel_case_types, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void, CStr, CString};
use std::process::Command;

type CFTypeRef = *const c_void;
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    w: f64,
    h: f64,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(c: CFTypeRef);
    fn CFRetain(c: CFTypeRef) -> CFTypeRef;
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef;
    fn CFStringCreateWithCString(a: CFTypeRef, s: *const c_char, enc: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, len: isize, enc: u32) -> bool;
    fn CFStringGetTypeID() -> usize;
    fn CFGetTypeID(c: CFTypeRef) -> usize;
    fn CFNumberGetValue(n: CFTypeRef, ty: i32, out: *mut c_void) -> bool;
    fn CFBooleanGetValue(b: CFTypeRef) -> bool;
    fn CFArrayGetTypeID() -> usize;
    static kCFBooleanTrue: CFTypeRef;
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(opt: u32, rel: u32) -> CFTypeRef;
    fn CGEventSourceCreate(state: i32) -> CFTypeRef;
    fn CGEventCreateMouseEvent(src: CFTypeRef, ty: u32, p: CGPoint, button: u32) -> CFTypeRef;
    fn CGEventCreateKeyboardEvent(src: CFTypeRef, key: u16, down: bool) -> CFTypeRef;
    fn CGEventCreateScrollWheelEvent2(src: CFTypeRef, unit: u32, n: u32, a: i32, b: i32, c: i32) -> CFTypeRef;
    fn CGEventKeyboardSetUnicodeString(e: CFTypeRef, len: usize, s: *const u16);
    fn CGEventSetIntegerValueField(e: CFTypeRef, f: u32, v: i64);
    fn CGEventSetFlags(e: CFTypeRef, f: u64);
    fn CGEventPostToPid(pid: i32, e: CFTypeRef);
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> i32;
    fn AXUIElementPerformAction(el: CFTypeRef, action: CFTypeRef) -> i32;
    fn AXUIElementSetAttributeValue(el: CFTypeRef, attr: CFTypeRef, v: CFTypeRef) -> i32;
    fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
    fn AXValueGetValue(v: CFTypeRef, ty: u32, out: *mut c_void) -> bool;
}

const UTF8: u32 = 0x0800_0100;

fn cfstr(s: &str) -> CFTypeRef {
    let c = CString::new(s).unwrap_or_default();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) }
}
unsafe fn to_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
        return None;
    }
    let mut buf = vec![0 as c_char; 8192];
    if CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, UTF8) {
        Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
    } else {
        None
    }
}
unsafe fn dict_get(d: CFTypeRef, key: &str) -> CFTypeRef {
    let k = cfstr(key);
    let v = CFDictionaryGetValue(d, k);
    CFRelease(k);
    v
}
unsafe fn num_f64(v: CFTypeRef) -> f64 {
    let mut o = 0f64;
    if !v.is_null() {
        CFNumberGetValue(v, 6, &mut o as *mut f64 as *mut c_void);
    }
    o
}

// ---------- windows ----------

#[derive(Debug, Clone)]
pub struct Win {
    pub id: u32,
    pub pid: i32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub layer: i32,
    pub onscreen: bool,
    pub name: String,
}

/// All windows (including off-screen/hidden) for `pid`, front-to-back.
pub fn windows(pid: i32) -> Vec<Win> {
    let mut out = vec![];
    unsafe {
        let arr = CGWindowListCopyWindowInfo(0, 0);
        if arr.is_null() {
            return out;
        }
        for i in 0..CFArrayGetCount(arr) {
            let d = CFArrayGetValueAtIndex(arr, i);
            let p = num_f64(dict_get(d, "kCGWindowOwnerPID")) as i32;
            if pid != 0 && p != pid {
                continue;
            }
            let b = dict_get(d, "kCGWindowBounds");
            let on = dict_get(d, "kCGWindowIsOnscreen");
            out.push(Win {
                id: num_f64(dict_get(d, "kCGWindowNumber")) as u32,
                pid: p,
                x: num_f64(dict_get(b, "X")),
                y: num_f64(dict_get(b, "Y")),
                w: num_f64(dict_get(b, "Width")),
                h: num_f64(dict_get(b, "Height")),
                layer: num_f64(dict_get(d, "kCGWindowLayer")) as i32,
                onscreen: !on.is_null() && CFBooleanGetValue(on),
                name: to_string(dict_get(d, "kCGWindowName")).unwrap_or_default(),
            });
        }
        CFRelease(arr);
    }
    out
}

/// The app's main window: layer 0, largest area.
pub fn main_window(pid: i32) -> Option<Win> {
    windows(pid)
        .into_iter()
        .filter(|w| w.layer == 0 && w.w > 50.0 && w.h > 50.0)
        .max_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h)))
}

fn front_info(field: &str) -> Option<String> {
    let asn = Command::new("lsappinfo").arg("front").output().ok()?;
    let asn = String::from_utf8_lossy(&asn.stdout).trim().trim_end_matches(':').to_string();
    let info = Command::new("lsappinfo").args(["info", "-only", field, &asn]).output().ok()?;
    Some(String::from_utf8_lossy(&info.stdout).to_string())
}

/// Pid of the frontmost application (no permissions required).
pub fn frontmost_pid() -> Option<i32> {
    let t = front_info("pid")?;
    t.split("pid =").nth(1)?.split_whitespace().next()?.parse().ok()
}

pub fn frontmost_name() -> Option<String> {
    let t = front_info("name")?;
    Some(t.split('"').nth(1)?.to_string())
}

// ---------- input (per-pid, no focus change) ----------

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

fn source() -> CFTypeRef {
    // -1 = kCGEventSourceStatePrivate: ignores the user's held modifiers/buttons.
    unsafe { CGEventSourceCreate(-1) }
}

fn mouse(pid: i32, win: u32, ty: u32, x: f64, y: f64, click_state: i64) {
    unsafe {
        let src = source();
        let e = CGEventCreateMouseEvent(src, ty, CGPoint { x, y }, 0);
        if e.is_null() {
            return;
        }
        CGEventSetIntegerValueField(e, 3, click_state); // mouseEventClickState
        CGEventSetIntegerValueField(e, 91, win as i64); // window under pointer
        CGEventSetIntegerValueField(e, 92, win as i64); // window that can handle
        CGEventPostToPid(pid, e);
        CFRelease(e);
        CFRelease(src);
    }
}

fn pause(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// Real left click at global screen coords, delivered to `pid` only.
pub fn click(pid: i32, win: u32, x: f64, y: f64) {
    mouse(pid, win, 5, x, y, 0); // move: arms hover/tracking areas
    pause(40);
    mouse(pid, win, 1, x, y, 1);
    pause(40);
    mouse(pid, win, 2, x, y, 1);
}

pub fn hover(pid: i32, win: u32, x: f64, y: f64) {
    mouse(pid, win, 5, x, y, 0);
}

pub fn drag(pid: i32, win: u32, from: (f64, f64), to: (f64, f64), steps: u32) {
    mouse(pid, win, 5, from.0, from.1, 0);
    pause(30);
    mouse(pid, win, 1, from.0, from.1, 1);
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        pause(16);
        mouse(pid, win, 6, from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t, 1);
    }
    mouse(pid, win, 2, to.0, to.1, 1);
}

pub fn scroll(pid: i32, win: u32, x: f64, y: f64, dy: i32) {
    mouse(pid, win, 5, x, y, 0);
    unsafe {
        let src = source();
        let e = CGEventCreateScrollWheelEvent2(src, 0, 1, dy, 0, 0);
        if !e.is_null() {
            CGEventSetIntegerValueField(e, 91, win as i64);
            CGEventSetIntegerValueField(e, 92, win as i64);
            CGEventPostToPid(pid, e);
            CFRelease(e);
        }
        CFRelease(src);
    }
}

/// Type text as Unicode key events (layout independent).
pub fn type_text(pid: i32, text: &str) {
    unsafe {
        let src = source();
        for ch in text.chars() {
            let mut buf = [0u16; 2];
            let u = ch.encode_utf16(&mut buf);
            for down in [true, false] {
                let e = CGEventCreateKeyboardEvent(src, 0, down);
                if e.is_null() {
                    continue;
                }
                CGEventKeyboardSetUnicodeString(e, u.len(), u.as_ptr());
                CGEventPostToPid(pid, e);
                CFRelease(e);
            }
            pause(8);
        }
        CFRelease(src);
    }
}

pub fn key(pid: i32, keycode: u16, flags: u64) {
    unsafe {
        let src = source();
        for down in [true, false] {
            let e = CGEventCreateKeyboardEvent(src, keycode, down);
            if e.is_null() {
                continue;
            }
            CGEventSetFlags(e, flags);
            CGEventPostToPid(pid, e);
            CFRelease(e);
            pause(10);
        }
        CFRelease(src);
    }
}

// ---------- accessibility ----------

#[derive(Debug, Clone)]
pub struct Node {
    pub el: usize, // retained AXUIElementRef
    pub role: String,
    pub text: String, // title | description | value (first non-empty)
    pub frame: Option<(f64, f64, f64, f64)>,
}

unsafe fn attr(el: CFTypeRef, name: &str) -> CFTypeRef {
    let k = cfstr(name);
    let mut out: CFTypeRef = std::ptr::null();
    let rc = AXUIElementCopyAttributeValue(el, k, &mut out);
    CFRelease(k);
    if rc == 0 { out } else { std::ptr::null() }
}
unsafe fn str_attr(el: CFTypeRef, name: &str) -> Option<String> {
    let v = attr(el, name);
    let s = to_string(v);
    if !v.is_null() {
        CFRelease(v);
    }
    s.filter(|s| !s.is_empty())
}
unsafe fn frame_of(el: CFTypeRef) -> Option<(f64, f64, f64, f64)> {
    let p = attr(el, "AXPosition");
    let s = attr(el, "AXSize");
    let mut out = None;
    if !p.is_null() && !s.is_null() {
        let (mut pt, mut sz) = (CGPoint { x: 0.0, y: 0.0 }, CGSize { w: 0.0, h: 0.0 });
        if AXValueGetValue(p, 1, &mut pt as *mut _ as *mut c_void)
            && AXValueGetValue(s, 2, &mut sz as *mut _ as *mut c_void)
        {
            out = Some((pt.x, pt.y, sz.w, sz.h));
        }
    }
    for v in [p, s] {
        if !v.is_null() {
            CFRelease(v);
        }
    }
    out
}
unsafe fn describe(el: CFTypeRef) -> Node {
    let text = ["AXTitle", "AXDescription", "AXValue", "AXLabel"]
        .iter()
        .find_map(|a| str_attr(el, a))
        .unwrap_or_default();
    Node {
        el: CFRetain(el) as usize,
        role: str_attr(el, "AXRole").unwrap_or_default(),
        text,
        frame: frame_of(el),
    }
}
/// Retained child elements of `el`.
unsafe fn kids(el: CFTypeRef) -> Vec<CFTypeRef> {
    let a = attr(el, "AXChildren");
    let mut v = vec![];
    if !a.is_null() && CFGetTypeID(a) == CFArrayGetTypeID() {
        for i in 0..CFArrayGetCount(a) {
            v.push(CFRetain(CFArrayGetValueAtIndex(a, i)));
        }
    }
    if !a.is_null() {
        CFRelease(a);
    }
    v
}

fn app_root(pid: i32) -> CFTypeRef {
    unsafe {
        let r = AXUIElementCreateApplication(pid);
        AXUIElementSetMessagingTimeout(r, 3.0);
        r
    }
}

/// Breadth-first walk (bounded). `visit` returns true to stop.
fn walk(pid: i32, max_nodes: usize, mut visit: impl FnMut(&Node, usize) -> bool) {
    unsafe {
        let root = app_root(pid);
        let mut queue = std::collections::VecDeque::from([(root, 0usize)]);
        let mut seen = 0;
        while let Some((el, depth)) = queue.pop_front() {
            seen += 1;
            let n = describe(el);
            let stop = visit(&n, depth);
            CFRelease(n.el as CFTypeRef);
            if !stop && seen < max_nodes {
                for k in kids(el) {
                    queue.push_back((k, depth + 1));
                }
            }
            CFRelease(el);
            if stop || seen >= max_nodes {
                break;
            }
        }
        for (el, _) in queue {
            CFRelease(el);
        }
    }
}

pub fn ax_dump(pid: i32, max_depth: usize, max_nodes: usize) -> Vec<String> {
    let mut lines = vec![];
    walk(pid, max_nodes, |n, d| {
        if d <= max_depth && (!n.text.is_empty() || n.role == "AXButton" || n.role == "AXWebArea") {
            let f = n.frame.map(|f| format!(" @{:.0},{:.0} {:.0}x{:.0}", f.0, f.1, f.2, f.3)).unwrap_or_default();
            lines.push(format!("{}{} {:?}{}", "  ".repeat(d.min(8)), n.role, n.text, f));
        }
        false
    });
    lines
}

/// First element whose label contains `needle` (case-insensitive). Caller owns `el`.
pub fn ax_find(pid: i32, needle: &str, exact: bool) -> Option<Node> {
    let needle = needle.to_lowercase();
    let (mut found, mut fallback): (Option<Node>, Option<Node>) = (None, None);
    walk(pid, 40_000, |n, _| {
        let t = n.text.to_lowercase();
        let hit = !t.is_empty() && if exact { t == needle } else { t.contains(&needle) };
        if !hit {
            return false;
        }
        let kept = Node { el: unsafe { CFRetain(n.el as CFTypeRef) } as usize, ..n.clone() };
        if n.role == "AXStaticText" {
            fallback.get_or_insert(kept);
            false
        } else {
            found = Some(kept);
            true
        }
    });
    found.or(fallback)
}

pub fn ax_press(node: &Node) -> bool {
    unsafe {
        let a = cfstr("AXPress");
        let rc = AXUIElementPerformAction(node.el as CFTypeRef, a);
        CFRelease(a);
        rc == 0
    }
}

pub fn ax_text(node: &Node) -> String {
    unsafe { describe(node.el as CFTypeRef).text }
}

/// Real-click an AX element's center, delivered to its owning pid.
pub fn click_node(pid: i32, node: &Node) -> Result<(), String> {
    let (x, y, w, h) = node.frame.ok_or("element has no frame")?;
    let win = main_window(pid).ok_or("app has no window")?;
    click(pid, win.id, x + w / 2.0, y + h / 2.0);
    Ok(())
}

// ---------- launch ----------

/// Launch an app hidden and in the background (`open -g -j -n`), muted by env
/// supplied by the caller. Returns the pid of the new instance.
pub fn launch_hidden(app: &str, exe_name: &str, env: &[(String, String)]) -> Result<i32, String> {
    let before: std::collections::HashSet<i32> = pids_named(exe_name).into_iter().collect();
    let mut cmd = Command::new("open");
    cmd.args(["-g", "-j", "-n"]);
    for (k, v) in env {
        cmd.arg("--env").arg(format!("{k}={v}"));
    }
    cmd.arg(app);
    let st = cmd.status().map_err(|e| e.to_string())?;
    if !st.success() {
        return Err(format!("open exited {st}"));
    }
    for _ in 0..100 {
        pause(100);
        if let Some(p) = pids_named(exe_name).into_iter().find(|p| !before.contains(p)) {
            return Ok(p);
        }
    }
    Err("launched app pid not found".into())
}

fn pids_named(name: &str) -> Vec<i32> {
    let out = Command::new("pgrep").args(["-x", name]).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

pub fn capture_window(win: u32, out: &str) -> bool {
    Command::new("screencapture").args(["-x", "-o", "-l", &win.to_string(), out]).status().map(|s| s.success()).unwrap_or(false)
}

/// Mark the app's window key/main *inside the app* via Accessibility, without
/// activating the application (no frontmost change). Needed so AppKit/WKWebView
/// accepts mouse clicks on an inactive app. Returns the AX status codes.
pub fn ax_make_key(pid: i32, attrs: &[&str]) -> Vec<(String, i32)> {
    unsafe {
        let root = app_root(pid);
        let w = attr(root, "AXFocusedWindow");
        let w = if w.is_null() { first_window(root) } else { w };
        let mut out = vec![];
        if !w.is_null() {
            for a in attrs {
                let k = cfstr(a);
                out.push((a.to_string(), AXUIElementSetAttributeValue(w, k, kCFBooleanTrue)));
                CFRelease(k);
            }
        }
        out
    }
}
unsafe fn first_window(root: CFTypeRef) -> CFTypeRef {
    let a = attr(root, "AXWindows");
    if a.is_null() || CFArrayGetCount(a) == 0 {
        return std::ptr::null();
    }
    CFRetain(CFArrayGetValueAtIndex(a, 0))
}

// ---------- private SkyLight: make a window key without activating the app ----------

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Psn {
    hi: u32,
    lo: u32,
}
extern "C" {
    fn dlopen(path: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void;
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn GetProcessForPID(pid: i32, psn: *mut Psn) -> i32;
}

/// Post the "window became key" event records yabai uses for focus-without-raise.
/// The front application and window order stay unchanged. Private API: returns
/// false if SkyLight does not export the symbol on this macOS build.
pub fn key_without_raise(pid: i32, win: u32) -> bool {
    unsafe {
        let lib = CString::new("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight").unwrap();
        let h = dlopen(lib.as_ptr(), 1);
        if h.is_null() {
            return false;
        }
        let sym = CString::new("SLPSPostEventRecordTo").unwrap();
        let f = dlsym(h, sym.as_ptr());
        if f.is_null() {
            return false;
        }
        let post: extern "C" fn(*const Psn, *const u8) -> i32 = std::mem::transmute(f);
        let mut psn = Psn::default();
        if GetProcessForPID(pid, &mut psn) != 0 {
            return false;
        }
        for kind in [0x01u8, 0x02u8] {
            let mut b = [0u8; 0xf8];
            b[0x04] = 0xf8;
            b[0x08] = kind;
            b[0x3a] = 0x10;
            b[0x3c..0x40].copy_from_slice(&win.to_ne_bytes());
            for x in &mut b[0x20..0x30] {
                *x = 0xff;
            }
            post(&psn, b.as_ptr());
        }
        true
    }
}
