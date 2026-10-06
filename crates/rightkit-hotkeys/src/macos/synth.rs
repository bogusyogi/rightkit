//! Synthetic input for tests and self-diagnostics.
//!
//! Events are created from a *private* event source, so they carry exactly the flags set on them
//! and never merge with, or leave behind, the user's real modifier state. That is what lets an
//! end-to-end run exercise registration and taps without stealing a physical shortcut.

use super::ffi::*;
use super::tap::TapLocation;

/// Stable marker carried by private synthetic events so E2E taps can ignore real user input.
pub const SYNTHETIC_EVENT_USER_DATA: i64 = 0x5249_4748_544B_4954;

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn can_listen() -> bool {
    unsafe { CGPreflightListenEventAccess() }
}

pub fn can_post() -> bool {
    unsafe { CGPreflightPostEventAccess() }
}

fn tap_location(location: TapLocation) -> u32 {
    match location {
        TapLocation::Hid => K_CG_HID_EVENT_TAP,
        TapLocation::Session => K_CG_SESSION_EVENT_TAP,
        TapLocation::AnnotatedSession => K_CG_ANNOTATED_SESSION_EVENT_TAP,
    }
}

/// Virtual keycodes of F1..F20. Hardware tags these with the secondary-fn flag, and macOS only
/// matches a registered system hotkey on a function key when that flag is present.
const FUNCTION_KEYCODES: [u16; 20] = [
    122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90,
];

/// Post one key event with explicit `flags` (see the `FLAG_*` constants). Function keys get
/// [`FLAG_SECONDARY_FN`] added, exactly as a physical keyboard reports them.
pub fn post_key(location: TapLocation, keycode: u16, down: bool, flags: u64) -> bool {
    let flags = if FUNCTION_KEYCODES.contains(&keycode) { flags | FLAG_SECONDARY_FN } else { flags };
    unsafe {
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_PRIVATE);
        if source.is_null() {
            return false;
        }
        let event = CGEventCreateKeyboardEvent(source, keycode, down);
        if event.is_null() {
            CFRelease(source);
            return false;
        }
        CGEventSetFlags(event, flags);
        CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_EVENT_USER_DATA);
        CGEventPost(tap_location(location), event);
        CFRelease(event);
        CFRelease(source);
    }
    true
}

/// Post a modifier transition (`FlagsChanged`) for `keycode` with the resulting `flags`.
pub fn post_flags_changed(location: TapLocation, keycode: u16, flags: u64) -> bool {
    // A keyboard event for a modifier keycode is delivered as a flags-changed event.
    post_key(location, keycode, flags != 0, flags)
}

/// Post an "other" mouse button press (button 2 = middle) at `(x, y)`.
pub fn post_other_mouse(location: TapLocation, button: u32, x: f64, y: f64, down: bool) -> bool {
    unsafe {
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_PRIVATE);
        if source.is_null() {
            return false;
        }
        let event = CGEventCreateMouseEvent(
            source,
            if down { K_CG_EVENT_OTHER_MOUSE_DOWN_T } else { K_CG_EVENT_OTHER_MOUSE_UP_T },
            CGPoint { x, y },
            button,
        );
        if event.is_null() {
            CFRelease(source);
            return false;
        }
        CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_EVENT_USER_DATA);
        CGEventPost(tap_location(location), event);
        CFRelease(event);
        CFRelease(source);
    }
    true
}

static APP_HOST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Pump the calling thread's event loop for `seconds`. Carbon hotkey events are delivered through
/// the main thread's event queue: once [`init_background_app`] ran this drains `NSApplication`'s
/// queue (what a Tauri/AppKit host does), otherwise it spins the plain run loop.
pub fn pump_run_loop(seconds: f64) {
    use std::ffi::c_void;
    let app = APP_HOST.load(std::sync::atomic::Ordering::SeqCst) as *mut c_void;
    if app.is_null() {
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, false) };
        return;
    }
    unsafe {
        type SendF = unsafe extern "C" fn(*mut c_void, *mut c_void, f64) -> *mut c_void;
        type SendP = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
        type SendStr = unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8) -> *mut c_void;
        type Next = unsafe extern "C" fn(*mut c_void, *mut c_void, u64, *mut c_void, *mut c_void, bool) -> *mut c_void;
        let m = objc_msgSend as unsafe extern "C" fn();
        let date_class = objc_getClass(b"NSDate\0".as_ptr());
        let string_class = objc_getClass(b"NSString\0".as_ptr());
        let until = std::mem::transmute::<_, SendF>(m)(date_class, sel_registerName(b"dateWithTimeIntervalSinceNow:\0".as_ptr()), seconds);
        let mode = std::mem::transmute::<_, SendStr>(m)(string_class, sel_registerName(b"stringWithUTF8String:\0".as_ptr()), b"kCFRunLoopDefaultMode\0".as_ptr());
        let next = sel_registerName(b"nextEventMatchingMask:untilDate:inMode:dequeue:\0".as_ptr());
        let send_event = sel_registerName(b"sendEvent:\0".as_ptr());
        loop {
            let event = std::mem::transmute::<_, Next>(m)(app, next, u64::MAX, until, mode, true);
            if event.is_null() {
                break;
            }
            std::mem::transmute::<_, SendP>(m)(app, send_event, event);
        }
    }
}

/// Make this process behave like a real background app: connect to the WindowServer through
/// `NSApplication` and take the *accessory* activation policy (no Dock icon, no menu bar, never
/// activated, never foreground). Carbon hotkey events are only delivered to processes that have
/// that connection; a Tauri/AppKit host already has it, a bare CLI or test binary does not.
/// Idempotent. Must run on the main thread.
pub fn init_background_app() -> bool {
    use std::ffi::c_void;
    unsafe {
        let class = objc_getClass(b"NSApplication\0".as_ptr());
        if class.is_null() {
            return false;
        }
        let shared: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let app = shared(class, sel_registerName(b"sharedApplication\0".as_ptr()));
        if app.is_null() {
            return false;
        }
        // NSApplicationActivationPolicyAccessory = 1
        let set_policy: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> bool =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let ok = set_policy(app, sel_registerName(b"setActivationPolicy:\0".as_ptr()), 1);
        // Finish launching without activating: installs the event queue the pump drains.
        shared(app, sel_registerName(b"finishLaunching\0".as_ptr()));
        APP_HOST.store(app as usize, std::sync::atomic::Ordering::SeqCst);
        ok
    }
}
