//! Raw framework bindings: CoreFoundation, ApplicationServices (AX, CoreGraphics
//! events and windows), ImageIO, and the Objective-C runtime. No third-party crates.

#![allow(
    // `super::background` (moved from rightkit-qa-driver) declares some of these with other
    // Rust types; same ABI. Unifying them is a follow-up.
    clashing_extern_declarations,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::missing_safety_doc
)]

use std::ffi::c_void;
use std::os::raw::{c_char, c_double, c_int, c_long, c_ulong};

pub type CFTypeRef = *const c_void;
pub type CFIndex = c_long;
pub type Id = *const c_void;
pub type Sel = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGPoint {
    pub x: c_double,
    pub y: c_double,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGSize {
    pub w: c_double,
    pub h: c_double,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CFRange {
    pub location: CFIndex,
    pub length: CFIndex,
}

pub const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFRelease(c: CFTypeRef);
    pub fn CFRetain(c: CFTypeRef) -> CFTypeRef;
    pub fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
    pub fn CFGetTypeID(c: CFTypeRef) -> c_ulong;
    pub fn CFStringGetTypeID() -> c_ulong;
    pub fn CFArrayGetTypeID() -> c_ulong;
    pub fn CFNumberGetTypeID() -> c_ulong;
    pub fn CFBooleanGetTypeID() -> c_ulong;
    pub fn CFStringCreateWithBytes(
        alloc: CFTypeRef,
        bytes: *const u8,
        n: CFIndex,
        enc: u32,
        ext: u8,
    ) -> CFTypeRef;
    pub fn CFStringGetLength(s: CFTypeRef) -> CFIndex;
    pub fn CFStringGetMaximumSizeForEncoding(len: CFIndex, enc: u32) -> CFIndex;
    pub fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, enc: u32) -> u8;
    pub fn CFArrayGetCount(a: CFTypeRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(a: CFTypeRef, i: CFIndex) -> CFTypeRef;
    pub fn CFDictionaryGetValue(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef;
    pub fn CFNumberGetValue(n: CFTypeRef, ty: c_int, out: *mut c_void) -> u8;
    pub fn CFNumberCreate(alloc: CFTypeRef, ty: c_int, v: *const c_void) -> CFTypeRef;
    pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    pub fn CFURLCreateFromFileSystemRepresentation(
        alloc: CFTypeRef,
        buf: *const u8,
        len: CFIndex,
        dir: u8,
    ) -> CFTypeRef;
    pub fn CFRunLoopRunInMode(mode: CFTypeRef, secs: c_double, ret: u8) -> c_int;
    pub static kCFRunLoopDefaultMode: CFTypeRef;
    pub static kCFBooleanTrue: CFTypeRef;
    pub static kCFBooleanFalse: CFTypeRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> u8;
    pub fn AXUIElementCreateApplication(pid: c_int) -> CFTypeRef;
    pub fn AXUIElementCreateSystemWide() -> CFTypeRef;
    /// `x`/`y` are global display coordinates (top-left origin, points). `el` is normally
    /// the system-wide element; an application element restricts the hit test to that app.
    pub fn AXUIElementCopyElementAtPosition(
        el: CFTypeRef,
        x: f32,
        y: f32,
        out: *mut CFTypeRef,
    ) -> c_int;
    pub fn AXUIElementCopyAttributeValue(
        el: CFTypeRef,
        attr: CFTypeRef,
        out: *mut CFTypeRef,
    ) -> c_int;
    pub fn AXUIElementCopyParameterizedAttributeValue(
        el: CFTypeRef,
        attr: CFTypeRef,
        param: CFTypeRef,
        out: *mut CFTypeRef,
    ) -> c_int;
    pub fn AXUIElementSetAttributeValue(el: CFTypeRef, attr: CFTypeRef, v: CFTypeRef) -> c_int;
    pub fn AXUIElementIsAttributeSettable(el: CFTypeRef, attr: CFTypeRef, out: *mut u8) -> c_int;
    pub fn AXUIElementCopyActionNames(el: CFTypeRef, out: *mut CFTypeRef) -> c_int;
    pub fn AXUIElementPerformAction(el: CFTypeRef, action: CFTypeRef) -> c_int;
    pub fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> c_int;
    pub fn AXUIElementGetPid(el: CFTypeRef, pid: *mut c_int) -> c_int;
    pub fn AXValueGetValue(v: CFTypeRef, ty: c_int, out: *mut c_void) -> u8;
    pub fn AXValueCreate(ty: c_int, v: *const c_void) -> CFTypeRef;
    /// Long-standing private-but-stable symbol used to map an AX window to its CGWindowID.
    pub fn _AXUIElementGetWindow(el: CFTypeRef, out: *mut u32) -> c_int;

    pub fn CGWindowListCopyWindowInfo(opt: u32, rel: u32) -> CFTypeRef;
    pub fn CGPreflightScreenCaptureAccess() -> u8;
    pub fn CGEventSourceCreate(state: c_int) -> CFTypeRef;
    pub fn CGEventCreate(src: CFTypeRef) -> CFTypeRef;
    pub fn CGEventCreateKeyboardEvent(src: CFTypeRef, key: u16, down: u8) -> CFTypeRef;
    pub fn CGEventCreateMouseEvent(src: CFTypeRef, ty: u32, pt: CGPoint, button: u32) -> CFTypeRef;
    pub fn CGEventCreateScrollWheelEvent2(
        src: CFTypeRef,
        units: u32,
        count: u32,
        w1: i32,
        w2: i32,
        w3: i32,
    ) -> CFTypeRef;
    pub fn CGEventKeyboardSetUnicodeString(ev: CFTypeRef, n: c_ulong, s: *const u16);
    pub fn CGEventSetFlags(ev: CFTypeRef, flags: u64);
    pub fn CGEventSetLocation(ev: CFTypeRef, pt: CGPoint);
    pub fn CGEventGetLocation(ev: CFTypeRef) -> CGPoint;
    pub fn CGEventSetIntegerValueField(ev: CFTypeRef, field: u32, v: i64);
    pub fn CGEventPostToPid(pid: c_int, ev: CFTypeRef);
    pub fn CGEventPost(tap: u32, ev: CFTypeRef);
    pub fn CGWarpMouseCursorPosition(pt: CGPoint) -> c_int;
    pub fn CGImageGetWidth(img: CFTypeRef) -> usize;
    pub fn CGImageGetHeight(img: CFTypeRef) -> usize;
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    /// HIToolbox: true while any process has secure event input on (system-wide).
    pub fn IsSecureEventInputEnabled() -> u8;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    pub fn CGImageDestinationCreateWithURL(
        url: CFTypeRef,
        ty: CFTypeRef,
        count: usize,
        opts: CFTypeRef,
    ) -> CFTypeRef;
    pub fn CGImageDestinationAddImage(dest: CFTypeRef, img: CFTypeRef, props: CFTypeRef);
    pub fn CGImageDestinationFinalize(dest: CFTypeRef) -> u8;
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    pub fn objc_getClass(name: *const c_char) -> Id;
    pub fn sel_registerName(name: *const c_char) -> Sel;
    pub fn objc_msgSend();
}

extern "C" {
    pub fn kill(pid: c_int, sig: c_int) -> c_int;
    pub fn dlsym(handle: *mut c_void, sym: *const c_char) -> *mut c_void;
}
pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// Owned CF reference, released on drop.
pub struct Cf(pub CFTypeRef);
impl Cf {
    pub fn new(r: CFTypeRef) -> Option<Cf> {
        if r.is_null() {
            None
        } else {
            Some(Cf(r))
        }
    }
    pub fn retain(r: CFTypeRef) -> Option<Cf> {
        if r.is_null() {
            None
        } else {
            Some(Cf(unsafe { CFRetain(r) }))
        }
    }
    pub fn string(s: &str) -> Cf {
        Cf(unsafe {
            CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as CFIndex, UTF8, 0)
        })
    }
}
impl Clone for Cf {
    fn clone(&self) -> Self {
        Cf(unsafe { CFRetain(self.0) })
    }
}
impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}
unsafe impl Send for Cf {}
unsafe impl Sync for Cf {}

pub fn cf_to_string(r: CFTypeRef) -> Option<String> {
    unsafe {
        if r.is_null() || CFGetTypeID(r) != CFStringGetTypeID() {
            return None;
        }
        let max = CFStringGetMaximumSizeForEncoding(CFStringGetLength(r), UTF8) + 1;
        let mut buf = vec![0 as c_char; max as usize];
        if CFStringGetCString(r, buf.as_mut_ptr(), max, UTF8) == 0 {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_string_lossy()
                .into_owned(),
        )
    }
}
pub fn cf_to_i64(r: CFTypeRef) -> Option<i64> {
    unsafe {
        if r.is_null() || CFGetTypeID(r) != CFNumberGetTypeID() {
            return None;
        }
        let mut v: i64 = 0;
        (CFNumberGetValue(r, 4, &mut v as *mut _ as *mut c_void) != 0).then_some(v)
    }
}
pub fn cf_to_f64(r: CFTypeRef) -> Option<f64> {
    unsafe {
        if r.is_null() || CFGetTypeID(r) != CFNumberGetTypeID() {
            return None;
        }
        let mut v: f64 = 0.0;
        (CFNumberGetValue(r, 6, &mut v as *mut _ as *mut c_void) != 0).then_some(v)
    }
}
pub fn cf_to_bool(r: CFTypeRef) -> Option<bool> {
    unsafe {
        (!r.is_null() && CFGetTypeID(r) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(r) != 0)
    }
}
pub fn dict_get(d: CFTypeRef, key: &str) -> CFTypeRef {
    let k = Cf::string(key);
    unsafe { CFDictionaryGetValue(d, k.0) }
}

// ------------------------------------------------------------ objc helpers

pub fn class(name: &std::ffi::CStr) -> Id {
    unsafe { objc_getClass(name.as_ptr()) }
}
pub fn sel(name: &std::ffi::CStr) -> Sel {
    unsafe { sel_registerName(name.as_ptr()) }
}

pub unsafe fn msg0(obj: Id, s: &std::ffi::CStr) -> Id {
    let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s))
}
pub unsafe fn msg1(obj: Id, s: &std::ffi::CStr, a: Id) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s), a)
}
pub unsafe fn msg2(obj: Id, s: &std::ffi::CStr, a: Id, b: Id) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s), a, b)
}
pub unsafe fn msg_i64(obj: Id, s: &std::ffi::CStr) -> i64 {
    let f: unsafe extern "C" fn(Id, Sel) -> i64 = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s))
}
pub unsafe fn msg_f64(obj: Id, s: &std::ffi::CStr) -> f64 {
    let f: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s))
}
pub unsafe fn msg_bool(obj: Id, s: &std::ffi::CStr) -> bool {
    let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s)) != 0
}
pub unsafe fn msg_bool1_u64(obj: Id, s: &std::ffi::CStr, a: u64) -> bool {
    let f: unsafe extern "C" fn(Id, Sel, u64) -> u8 =
        std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s), a) != 0
}
pub unsafe fn msg_bool2(obj: Id, s: &std::ffi::CStr, a: Id, b: Id) -> bool {
    let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> u8 =
        std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s), a, b) != 0
}
pub unsafe fn msg_idx(obj: Id, s: &std::ffi::CStr, i: usize) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, usize) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(s), i)
}

/// `[NSString stringWithUTF8String:]`, autoreleased by the runtime.
pub unsafe fn ns_string(s: &str) -> Id {
    let c = std::ffi::CString::new(s.replace('\0', "")).unwrap_or_default();
    let f: unsafe extern "C" fn(Id, Sel, *const c_char) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
    f(
        class(c"NSString"),
        sel(c"stringWithUTF8String:"),
        c.as_ptr(),
    )
}
pub unsafe fn ns_to_string(o: Id) -> Option<String> {
    if o.is_null() {
        return None;
    }
    let f: unsafe extern "C" fn(Id, Sel) -> *const c_char =
        std::mem::transmute(objc_msgSend as *const ());
    let p = f(o, sel(c"UTF8String"));
    (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
}

/// Run `f` inside an autorelease pool so Objective-C temporaries do not leak.
pub fn pool<R>(f: impl FnOnce() -> R) -> R {
    unsafe {
        let p = msg0(msg0(class(c"NSAutoreleasePool"), c"alloc"), c"init");
        let r = f();
        msg0(p, c"drain");
        r
    }
}

/// NSWorkspace publishes the running-app list and frontmost app through the main
/// run loop. A CLI never spins one, so drain pending sources before every read.
pub fn pump() {
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, 0);
    }
}

/// Process exists (signal 0). Guards against NSWorkspace's briefly stale list.
pub fn alive(pid: i32) -> bool {
    unsafe { kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1) }
}
