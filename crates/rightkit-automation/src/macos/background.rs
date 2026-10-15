//! macOS backend: window discovery, per-pid input, Accessibility reads.
//! Non-embedded fallback input goes through `CGEventPostToPid`; nothing here activates an app,
//! moves the real cursor, or posts to the global HID tap.
//! Hidden/non-key WKWebView targets should use the embedded server instead:
//! window-id fields cannot bypass AppKit's first-mouse handling.
//!
//! Reads (`windows`, `main_window`, `frontmost_*`, `accessibility_trusted`,
//! `ax_dump`, `ax_find`, `ax_text`) are public. Every effect (input, AX
//! actions, key-window changes, launch, capture) is reachable only through
//! [`Gated`], which admits each call through an [`EffectGate`] (EFF-001), or
//! through the explicitly named [`unsafe_ungated`] opt-in.
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
    fn CGEventCreateScrollWheelEvent2(
        src: CFTypeRef,
        unit: u32,
        n: u32,
        a: i32,
        b: i32,
        c: i32,
    ) -> CFTypeRef;
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

#[repr(C)]
#[derive(Default)]
struct ProcessSerialNumber {
    high: u32,
    low: u32,
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    // Process Manager (deprecated, still exported): answers from LaunchServices on every call, with no
    // run loop needed. `NSWorkspace.frontmostApplication` is only refreshed by the main run loop, so a
    // sampling thread in a test or CLI process would read a stale snapshot and could miss an owned app
    // coming to the front.
    fn GetFrontProcess(psn: *mut ProcessSerialNumber) -> i32;
    fn GetProcessPID(psn: *const ProcessSerialNumber, pid: *mut i32) -> i32;
}
#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *const c_void;
    fn objc_msgSend();
}
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

/// The localized name of the running application `pid` (`NSRunningApplication`), looked up by
/// pid at call time.
fn app_name_for_pid(pid: i32) -> Option<String> {
    unsafe {
        let cls = objc_getClass(c"NSRunningApplication".as_ptr());
        if cls.is_null() {
            return None;
        }
        let by_pid: unsafe extern "C" fn(*mut c_void, *const c_void, i32) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const c_void);
        let app = by_pid(
            cls,
            sel_registerName(c"runningApplicationWithProcessIdentifier:".as_ptr()),
            pid,
        );
        if app.is_null() {
            return None;
        }
        let get: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const c_void);
        let name = get(app, sel_registerName(c"localizedName".as_ptr()));
        if name.is_null() {
            return None;
        }
        let utf8: unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_char =
            std::mem::transmute(objc_msgSend as *const c_void);
        let p = utf8(name, sel_registerName(c"UTF8String".as_ptr()));
        (!p.is_null())
            .then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
    }
}

/// `lsappinfo` prints `"key"=value`, `"key"="value"` or `key = value` depending on the field and the
/// macOS version. Take the text AFTER the `=` of the line whose key is one of `keys`, never the key.
pub fn parse_lsappinfo(text: &str, keys: &[&str]) -> Option<String> {
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().trim_matches('"').trim();
        if !keys.iter().any(|want| k.eq_ignore_ascii_case(want)) {
            continue;
        }
        let v = v.trim().trim_matches('"').trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

fn front_info(field: &str) -> Option<String> {
    let asn = Command::new("lsappinfo").arg("front").output().ok()?;
    let asn = String::from_utf8_lossy(&asn.stdout)
        .trim()
        .trim_end_matches(':')
        .to_string();
    let info = Command::new("lsappinfo")
        .args(["info", "-only", field, &asn])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&info.stdout).to_string())
}

/// Pid of the frontmost application (no permissions required), or `None` when it cannot be read:
/// callers that prove a background run must treat `None` as unproven, never as "not frontmost".
pub fn frontmost_pid() -> Option<i32> {
    unsafe {
        let mut psn = ProcessSerialNumber::default();
        let mut pid = 0i32;
        if GetFrontProcess(&mut psn) == 0 && GetProcessPID(&psn, &mut pid) == 0 && pid > 0 {
            return Some(pid);
        }
    }
    // Fallback: lsappinfo, tolerant of both output formats.
    parse_lsappinfo(&front_info("pid")?, &["pid"])?.parse().ok()
}

/// Localized name of the frontmost application, or `None` when it cannot be read.
pub fn frontmost_name() -> Option<String> {
    if let Some(name) = frontmost_pid().and_then(app_name_for_pid) {
        return Some(name);
    }
    parse_lsappinfo(&front_info("name")?, &["name", "LSDisplayName"])
}

#[cfg(test)]
mod frontmost_tests {
    use super::*;

    #[test]
    fn lsappinfo_value_is_the_text_after_the_equals_in_every_format() {
        // CI (macos-15) printed the quoted key and value; the old parser returned the key.
        assert_eq!(
            parse_lsappinfo("\"LSDisplayName\"=\"Ember\"\n", &["name", "LSDisplayName"]).as_deref(),
            Some("Ember")
        );
        assert_eq!(
            parse_lsappinfo("\"name\"=\"Ember Beta\"", &["name", "LSDisplayName"]).as_deref(),
            Some("Ember Beta")
        );
        assert_eq!(
            parse_lsappinfo("name = \"Ember\"", &["name"]).as_deref(),
            Some("Ember")
        );
        assert_eq!(
            parse_lsappinfo("\"pid\"=4242\n", &["pid"]).as_deref(),
            Some("4242")
        );
        assert_eq!(
            parse_lsappinfo("pid = 4242\n", &["pid"]).as_deref(),
            Some("4242")
        );
        assert_eq!(
            parse_lsappinfo("pid=4242", &["pid"]).as_deref(),
            Some("4242")
        );
        // A key is never returned as the value; other keys and empty values are skipped.
        assert_eq!(
            parse_lsappinfo("\"bundleID\"=\"x\"\n\"pid\"=7\n", &["pid"]).as_deref(),
            Some("7")
        );
        assert_eq!(parse_lsappinfo("\"pid\"=\n", &["pid"]), None);
        assert_eq!(parse_lsappinfo("", &["pid"]), None);
        assert_eq!(
            parse_lsappinfo("\"LSDisplayName\"", &["name", "LSDisplayName"]),
            None
        );
    }
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
pub(crate) fn click(pid: i32, win: u32, x: f64, y: f64) {
    mouse(pid, win, 5, x, y, 0); // move: arms hover/tracking areas
    pause(40);
    mouse(pid, win, 1, x, y, 1);
    pause(40);
    mouse(pid, win, 2, x, y, 1);
}

pub(crate) fn hover(pid: i32, win: u32, x: f64, y: f64) {
    mouse(pid, win, 5, x, y, 0);
}

pub(crate) fn drag(pid: i32, win: u32, from: (f64, f64), to: (f64, f64), steps: u32) {
    mouse(pid, win, 5, from.0, from.1, 0);
    pause(30);
    mouse(pid, win, 1, from.0, from.1, 1);
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        pause(16);
        mouse(
            pid,
            win,
            6,
            from.0 + (to.0 - from.0) * t,
            from.1 + (to.1 - from.1) * t,
            1,
        );
    }
    mouse(pid, win, 2, to.0, to.1, 1);
}

pub(crate) fn scroll(pid: i32, win: u32, x: f64, y: f64, dy: i32) {
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
pub(crate) fn type_text(pid: i32, text: &str) {
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

pub(crate) fn key(pid: i32, keycode: u16, flags: u64) {
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
    if rc == 0 {
        out
    } else {
        std::ptr::null()
    }
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
            let f = n
                .frame
                .map(|f| format!(" @{:.0},{:.0} {:.0}x{:.0}", f.0, f.1, f.2, f.3))
                .unwrap_or_default();
            lines.push(format!(
                "{}{} {:?}{}",
                "  ".repeat(d.min(8)),
                n.role,
                n.text,
                f
            ));
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
        let hit = !t.is_empty()
            && if exact {
                t == needle
            } else {
                t.contains(&needle)
            };
        if !hit {
            return false;
        }
        let kept = Node {
            el: unsafe { CFRetain(n.el as CFTypeRef) } as usize,
            ..n.clone()
        };
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

pub(crate) fn ax_press(node: &Node) -> bool {
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
pub(crate) fn click_node(pid: i32, node: &Node) -> Result<(), String> {
    let (x, y, w, h) = node.frame.ok_or("element has no frame")?;
    let win = main_window(pid).ok_or("app has no window")?;
    click(pid, win.id, x + w / 2.0, y + h / 2.0);
    Ok(())
}

// ---------- launch ----------

/// Launch an app hidden and in the background (`open -g -j -n`), muted by env
/// supplied by the caller. Returns the pid of the new instance.
pub(crate) fn launch_hidden(
    app: &str,
    exe_name: &str,
    env: &[(String, String)],
) -> Result<i32, String> {
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
        if let Some(p) = pids_named(exe_name)
            .into_iter()
            .find(|p| !before.contains(p))
        {
            return Ok(p);
        }
    }
    Err("launched app pid not found".into())
}

fn pids_named(name: &str) -> Vec<i32> {
    let out = Command::new("pgrep").args(["-x", name]).output();
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    })
    .unwrap_or_default()
}

pub(crate) fn capture_window(win: u32, out: &str) -> bool {
    Command::new("screencapture")
        .args(["-x", "-o", "-l", &win.to_string(), out])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Mark the app's window key/main *inside the app* via Accessibility, without
/// activating the application (no frontmost change). Needed so AppKit/WKWebView
/// accepts mouse clicks on an inactive app. Returns the AX status codes.
pub(crate) fn ax_make_key(pid: i32, attrs: &[&str]) -> Vec<(String, i32)> {
    unsafe {
        let root = app_root(pid);
        let w = attr(root, "AXFocusedWindow");
        let w = if w.is_null() { first_window(root) } else { w };
        let mut out = vec![];
        if !w.is_null() {
            for a in attrs {
                let k = cfstr(a);
                out.push((
                    a.to_string(),
                    AXUIElementSetAttributeValue(w, k, kCFBooleanTrue),
                ));
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
pub(crate) fn key_without_raise(pid: i32, win: u32) -> bool {
    unsafe {
        let lib =
            CString::new("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight").unwrap();
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

// ---------- EFF-001 gated entry points ----------

use crate::control::admission::{
    CancelToken, Effect, EffectGate, EffectKind, EffectRequest, ExecError,
};
use std::sync::Arc;

/// Every effectful macOS primitive, admitted through one [`EffectGate`]:
/// validate → approval → execute → settle. A denial settles before the
/// primitive runs (no event is posted, nothing is launched or written).
/// Request params carry the payload (coordinates, text) for payload-bound
/// approval; they are never emitted in events or settlements.
#[derive(Debug, Clone)]
pub struct Gated {
    gate: Arc<EffectGate>,
}

fn pid_target(pid: i32) -> String {
    format!("pid:{pid}")
}

impl Gated {
    pub fn new(gate: Arc<EffectGate>) -> Self {
        Self { gate }
    }
    /// [`EffectGate::allow_all`]: every effect is admitted (still settled, still stopped by
    /// the kill switch). This is an explicit opt-out of the allowlist; there is no `Default`.
    /// Hosts should pass a gate built on `control::policy_hook::PolicyAdmission` instead.
    pub fn allow_all() -> Self {
        Self::new(Arc::new(EffectGate::allow_all()))
    }
    pub fn gate(&self) -> &Arc<EffectGate> {
        &self.gate
    }

    /// Admit an arbitrary effect through this gate (used by the CLI for
    /// composite commands such as "find then click").
    pub fn admit<T>(
        &self,
        kind: EffectKind,
        method: &str,
        target: String,
        params: String,
        run: impl FnOnce() -> Result<T, String>,
    ) -> Effect<T> {
        let req = EffectRequest::new(kind, method, target).with_params(params);
        self.gate
            .admit(req, &CancelToken::new(), |_| run().map_err(ExecError::from))
    }

    pub fn click(&self, pid: i32, win: u32, x: f64, y: f64) -> Effect<()> {
        let p = format!("{{\"window\":{win},\"x\":{x},\"y\":{y}}}");
        self.admit(EffectKind::Input, "click", pid_target(pid), p, || {
            click(pid, win, x, y);
            Ok(())
        })
    }
    pub fn hover(&self, pid: i32, win: u32, x: f64, y: f64) -> Effect<()> {
        let p = format!("{{\"window\":{win},\"x\":{x},\"y\":{y}}}");
        self.admit(EffectKind::Input, "hover", pid_target(pid), p, || {
            hover(pid, win, x, y);
            Ok(())
        })
    }
    pub fn drag(
        &self,
        pid: i32,
        win: u32,
        from: (f64, f64),
        to: (f64, f64),
        steps: u32,
    ) -> Effect<()> {
        let p = format!(
            "{{\"window\":{win},\"from\":[{},{}],\"to\":[{},{}],\"steps\":{steps}}}",
            from.0, from.1, to.0, to.1
        );
        self.admit(EffectKind::Input, "drag", pid_target(pid), p, || {
            drag(pid, win, from, to, steps);
            Ok(())
        })
    }
    pub fn scroll(&self, pid: i32, win: u32, x: f64, y: f64, dy: i32) -> Effect<()> {
        let p = format!("{{\"window\":{win},\"x\":{x},\"y\":{y},\"dy\":{dy}}}");
        self.admit(EffectKind::Input, "scroll", pid_target(pid), p, || {
            scroll(pid, win, x, y, dy);
            Ok(())
        })
    }
    pub fn type_text(&self, pid: i32, text: &str) -> Effect<()> {
        let p = format!("{{\"text\":\"{}\"}}", crate::json::esc(text));
        self.admit(EffectKind::Input, "type", pid_target(pid), p, || {
            type_text(pid, text);
            Ok(())
        })
    }
    pub fn key(&self, pid: i32, keycode: u16, flags: u64) -> Effect<()> {
        let p = format!("{{\"keycode\":{keycode},\"flags\":{flags}}}");
        self.admit(EffectKind::Input, "key", pid_target(pid), p, || {
            key(pid, keycode, flags);
            Ok(())
        })
    }
    /// `AXPress`; the value is whether AX reported success.
    pub fn ax_press(&self, node: &Node) -> Effect<bool> {
        let p = format!("{{\"role\":\"{}\"}}", crate::json::esc(&node.role));
        self.admit(EffectKind::Action, "press", "ax-node".into(), p, || {
            Ok(ax_press(node))
        })
    }
    pub fn click_node(&self, pid: i32, node: &Node) -> Effect<()> {
        let p = format!("{{\"role\":\"{}\"}}", crate::json::esc(&node.role));
        self.admit(EffectKind::Input, "click_node", pid_target(pid), p, || {
            click_node(pid, node)
        })
    }
    pub fn launch_hidden(
        &self,
        app: &str,
        exe_name: &str,
        env: &[(String, String)],
    ) -> Effect<i32> {
        let p = format!(
            "{{\"app\":\"{}\",\"exe\":\"{}\",\"env\":{}}}",
            crate::json::esc(app),
            crate::json::esc(exe_name),
            env.len()
        );
        self.admit(EffectKind::Process, "launch", app.to_string(), p, || {
            launch_hidden(app, exe_name, env)
        })
    }
    /// Window capture to `out`; settles `Failed` when `screencapture` fails.
    pub fn capture_window(&self, win: u32, out: &str) -> Effect<()> {
        let p = format!("{{\"out\":\"{}\"}}", crate::json::esc(out));
        self.admit(
            EffectKind::Capture,
            "capture",
            format!("window:{win}"),
            p,
            || {
                capture_window(win, out)
                    .then_some(())
                    .ok_or_else(|| "screencapture failed".to_string())
            },
        )
    }
    pub fn ax_make_key(&self, pid: i32, attrs: &[&str]) -> Effect<Vec<(String, i32)>> {
        let p = format!("{{\"attrs\":{}}}", attrs.len());
        self.admit(EffectKind::Action, "make_key", pid_target(pid), p, || {
            Ok(ax_make_key(pid, attrs))
        })
    }
    /// Private SkyLight focus-without-raise; value is whether it was posted.
    pub fn key_without_raise(&self, pid: i32, win: u32) -> Effect<bool> {
        let p = format!("{{\"window\":{win}}}");
        self.admit(
            EffectKind::Action,
            "key_without_raise",
            pid_target(pid),
            p,
            || Ok(key_without_raise(pid, win)),
        )
    }
}

/// **Ungated** effectful primitives: these bypass [`EffectGate`] admission
/// entirely: no validation, no host approval, no settlement, no events. The
/// process-wide kill switch (`policy::kill`) still applies: while it is
/// engaged every effectful function here does nothing and reports failure
/// (`false`, an empty list or an `Err`).
///
/// They also bypass the allowlist (`Policy`): nothing here knows which app a
/// pid is. Use only where admission is provably handled by the caller (e.g.
/// inside an executor already admitted by a gate) or in fixtures that drive a
/// test app directly. Host-facing code must use [`Gated`] with a
/// `control::policy_hook::PolicyAdmission` hook. Calling anything here is an
/// explicit, greppable opt-out of EFF-001.
pub mod unsafe_ungated {
    use super::Node;
    use crate::policy::killed;

    pub fn click(pid: i32, win: u32, x: f64, y: f64) {
        if !killed() {
            super::click(pid, win, x, y)
        }
    }
    pub fn hover(pid: i32, win: u32, x: f64, y: f64) {
        if !killed() {
            super::hover(pid, win, x, y)
        }
    }
    pub fn drag(pid: i32, win: u32, from: (f64, f64), to: (f64, f64), steps: u32) {
        if !killed() {
            super::drag(pid, win, from, to, steps)
        }
    }
    pub fn scroll(pid: i32, win: u32, x: f64, y: f64, dy: i32) {
        if !killed() {
            super::scroll(pid, win, x, y, dy)
        }
    }
    pub fn type_text(pid: i32, text: &str) {
        if !killed() {
            super::type_text(pid, text)
        }
    }
    pub fn key(pid: i32, keycode: u16, flags: u64) {
        if !killed() {
            super::key(pid, keycode, flags)
        }
    }
    pub fn ax_press(node: &Node) -> bool {
        !killed() && super::ax_press(node)
    }
    pub fn click_node(pid: i32, node: &Node) -> Result<(), String> {
        if killed() {
            return Err("kill switch engaged".into());
        }
        super::click_node(pid, node)
    }
    pub fn launch_hidden(
        app: &str,
        exe_name: &str,
        env: &[(String, String)],
    ) -> Result<i32, String> {
        if killed() {
            return Err("kill switch engaged".into());
        }
        super::launch_hidden(app, exe_name, env)
    }
    pub fn capture_window(win: u32, out: &str) -> bool {
        !killed() && super::capture_window(win, out)
    }
    pub fn ax_make_key(pid: i32, attrs: &[&str]) -> Vec<(String, i32)> {
        if killed() {
            return Vec::new();
        }
        super::ax_make_key(pid, attrs)
    }
    pub fn key_without_raise(pid: i32, win: u32) -> bool {
        !killed() && super::key_without_raise(pid, win)
    }
}
