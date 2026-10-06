//! Raw CoreGraphics / CoreFoundation / Carbon / ApplicationServices bindings used by the macOS
//! hotkey primitives. The safe `core-graphics` tap wrapper cannot return NULL to swallow an event,
//! so the tap path uses these directly.

#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use std::ffi::c_void;

pub type CGEventRef = *mut c_void;
pub type CGEventTapProxy = *mut c_void;
pub type CFMachPortRef = *mut c_void;
pub type CFRunLoopSourceRef = *mut c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFAllocatorRef = *const c_void;
pub type CGEventSourceRef = *mut c_void;

pub type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    pub fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    pub fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    pub fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: u32,
        position: CGPoint,
        button: u32,
    ) -> CGEventRef;
    pub fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
    pub fn CGEventPost(tap: u32, event: CGEventRef);
    pub fn CGPreflightListenEventAccess() -> bool;
    pub fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: *const c_void;
    pub fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    pub fn CFMachPortInvalidate(port: CFMachPortRef);
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: *const c_void);
    pub fn CFRunLoopRun();
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: bool) -> i32;
    pub fn CFRelease(cf: *const c_void);
    pub static kCFRunLoopDefaultMode: *const c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> bool;
    pub fn AXUIElementCreateSystemWide() -> *const c_void;
    pub fn AXUIElementCopyElementAtPosition(
        app: *const c_void,
        x: f32,
        y: f32,
        element: *mut *const c_void,
    ) -> i32;
    pub fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: *const c_void,
        value: *mut *const c_void,
    ) -> i32;
    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const u8,
        encoding: u32,
    ) -> *const c_void;
    pub fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut u8,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
}

// Event-tap location / placement / options.
pub const K_CG_HID_EVENT_TAP: u32 = 0;
pub const K_CG_SESSION_EVENT_TAP: u32 = 1;
pub const K_CG_ANNOTATED_SESSION_EVENT_TAP: u32 = 2;
pub const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
pub const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
pub const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;

// Event types.
pub const K_CG_EVENT_KEY_DOWN: u32 = 10;
pub const K_CG_EVENT_KEY_UP: u32 = 11;
pub const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
pub const K_CG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
pub const K_CG_EVENT_OTHER_MOUSE_UP: u32 = 26;
pub const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
pub const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

// Event fields.
pub const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
pub const K_CG_MOUSE_EVENT_BUTTON_NUMBER: u32 = 3;
/// Event-source user data: lets a process mark its own synthetic events.
pub const K_CG_EVENT_SOURCE_USER_DATA: u32 = 42;

// Flag masks.
pub const FLAG_SHIFT: u64 = 0x0002_0000;
pub const FLAG_CONTROL: u64 = 0x0004_0000;
pub const FLAG_ALTERNATE: u64 = 0x0008_0000;
pub const FLAG_COMMAND: u64 = 0x0010_0000;
pub const FLAG_SECONDARY_FN: u64 = 0x0080_0000;

// Event source state: private = does not merge with (or alter) the global modifier state.
pub const K_CG_EVENT_SOURCE_STATE_PRIVATE: i32 = -1;

// Mouse event types for synthetic input.
pub const K_CG_EVENT_OTHER_MOUSE_DOWN_T: u32 = 25;
pub const K_CG_EVENT_OTHER_MOUSE_UP_T: u32 = 26;

pub const KEYCODE_SPACE: u16 = 49;
pub const KEYCODE_ESCAPE: u16 = 53;
pub const KEYCODE_LEFT_OPTION: u16 = 58;
pub const KEYCODE_LEFT_CONTROL: u16 = 59;
pub const KEYCODE_RIGHT_OPTION: u16 = 61;
pub const KEYCODE_RIGHT_CONTROL: u16 = 62;
pub const KEYCODE_FN: u16 = 63;

// ---- Carbon hotkeys -------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EventTypeSpec {
    pub event_class: u32,
    pub event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct EventHotKeyID {
    pub signature: u32,
    pub id: u32,
}

pub type EventHandlerUPP = unsafe extern "C" fn(
    next: *mut c_void,
    event: *mut c_void,
    user_data: *mut c_void,
) -> i32;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    pub fn GetEventDispatcherTarget() -> *mut c_void;
    pub fn InstallEventHandler(
        target: *mut c_void,
        handler: EventHandlerUPP,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut *mut c_void,
    ) -> i32;
    pub fn RemoveEventHandler(handler_ref: *mut c_void) -> i32;
    pub fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: *mut c_void,
        options: u32,
        out_ref: *mut *mut c_void,
    ) -> i32;
    pub fn UnregisterEventHotKey(hotkey_ref: *mut c_void) -> i32;
    pub fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: u32,
        actual_size: *mut u32,
        data: *mut c_void,
    ) -> i32;
    pub fn GetEventKind(event: *mut c_void) -> u32;
}

pub const K_EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
pub const K_EVENT_HOTKEY_PRESSED: u32 = 5;
pub const K_EVENT_HOTKEY_RELEASED: u32 = 6;
pub const K_EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
pub const TYPE_EVENT_HOTKEY_ID: u32 = u32::from_be_bytes(*b"hkid");

// Carbon modifier bits for RegisterEventHotKey.
pub const CARBON_CMD_KEY: u32 = 1 << 8;
pub const CARBON_SHIFT_KEY: u32 = 1 << 9;
pub const CARBON_OPTION_KEY: u32 = 1 << 11;
pub const CARBON_CONTROL_KEY: u32 = 1 << 12;

/// `eventHotKeyExistsErr`: the chord is already registered.
pub const EVENT_HOTKEY_EXISTS_ERR: i32 = -9878;
/// `eventHotKeyInvalidErr`.
pub const EVENT_HOTKEY_INVALID_ERR: i32 = -9879;

// ---- Objective-C runtime (background app host) ---------------------------------------------

#[link(name = "objc")]
extern "C" {
    pub fn objc_getClass(name: *const u8) -> *mut c_void;
    pub fn sel_registerName(name: *const u8) -> *mut c_void;
    pub fn objc_msgSend();
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}
