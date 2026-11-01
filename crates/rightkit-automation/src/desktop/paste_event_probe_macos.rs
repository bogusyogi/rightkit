//! Bounded, listen-only macOS Cmd+V destination evidence probe.
//!
//! It observes one target PID for a bounded interval & records only tagged
//! Cmd+V events. Callers combine this with retained destination identity & a
//! promised-pasteboard receipt; `None` remains unavailable evidence.
#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::desktop::macos::CG_EVENT_SOURCE_USER_DATA_FIELD;
use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};

const CG_EVENT_KEY_DOWN: u32 = 10;
const CG_EVENT_KEY_UP: u32 = 11;
const CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
const CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
const KEY_V: i64 = 0x09;
const COMMAND_FLAG: u64 = 0x0010_0000;
const CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
const MAX_EVENTS: usize = 8;
const READY_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_LIFETIME: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;

type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreateForPid(
        pid: i32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: CFRunLoopSourceRef, mode: *const c_void);
    fn CFRunLoopRemoveSource(
        run_loop: *mut c_void,
        source: CFRunLoopSourceRef,
        mode: *const c_void,
    );
    fn CFRelease(value: *const c_void);
}

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn mach_absolute_time() -> u64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteEventKind {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasteEvent {
    pub kind: PasteEventKind,
    pub token: i64,
    pub keycode: i64,
    pub flags: u64,
    /// Process-monotonic timestamp suitable for correlating with app logs.
    pub mach_time: u64,
}

#[derive(Clone, Debug)]
pub struct PasteEventProbeResult {
    pub pid: i32,
    pub token: i64,
    pub events: Vec<PasteEvent>,
}

struct ProbeState {
    stopped: AtomicBool,
    events: Mutex<Vec<PasteEvent>>,
}

struct ProbeContext {
    state: Arc<ProbeState>,
    token: i64,
    port: AtomicPtr<c_void>,
}

/// A bounded per-PID event observer. The tap is listen-only and never mutates
/// or suppresses events. Drop stops it and joins its short-lived run-loop.
pub struct PasteEventProbe {
    pid: i32,
    token: i64,
    state: Arc<ProbeState>,
    join: Option<JoinHandle<()>>,
}

impl PasteEventProbe {
    /// Start observing target PID for at most two seconds.
    ///
    /// `None` means CoreGraphics/PSN setup was unavailable. Callers must keep
    /// normal delivery unchanged when this occurs.
    pub fn start(pid: i32, token: i64, lifetime: Duration) -> Option<Self> {
        if pid <= 0 {
            return None;
        }

        let state = Arc::new(ProbeState {
            stopped: AtomicBool::new(false),
            events: Mutex::new(Vec::with_capacity(MAX_EVENTS)),
        });
        let thread_state = Arc::clone(&state);
        let (ready_tx, ready_rx) = sync_channel(1);
        let bounded_lifetime = lifetime.min(MAX_LIFETIME);
        let join = std::thread::Builder::new()
            .name("hr-paste-event-probe".to_string())
            .spawn(move || unsafe {
                let context = Box::new(ProbeContext {
                    state: thread_state,
                    token,
                    port: AtomicPtr::new(std::ptr::null_mut()),
                });
                let context_ptr = Box::into_raw(context);
                let event_mask = (1u64 << CG_EVENT_KEY_DOWN) | (1u64 << CG_EVENT_KEY_UP);
                let port = CGEventTapCreateForPid(
                    pid,
                    CG_HEAD_INSERT_EVENT_TAP,
                    CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                    event_mask,
                    paste_event_tap_callback,
                    context_ptr.cast(),
                );
                if port.is_null() {
                    let _ = Box::from_raw(context_ptr);
                    let _ = ready_tx.send(false);
                    return;
                }
                (*context_ptr).port.store(port, Ordering::SeqCst);
                let source_ref = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
                if source_ref.is_null() {
                    CFRelease(port);
                    let _ = Box::from_raw(context_ptr);
                    let _ = ready_tx.send(false);
                    return;
                }
                let run_loop = CFRunLoop::get_current();
                CFRunLoopAddSource(
                    run_loop.as_concrete_TypeRef().cast(),
                    source_ref,
                    kCFRunLoopDefaultMode.cast(),
                );
                CGEventTapEnable(port, true);
                let _ = ready_tx.send(true);

                let deadline = Instant::now() + bounded_lifetime;
                let state = &(*context_ptr).state;
                while !state.stopped.load(Ordering::Acquire) && Instant::now() < deadline {
                    let _ = CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, POLL_INTERVAL, false);
                }

                state.stopped.store(true, Ordering::Release);
                CGEventTapEnable(port, false);
                CFRunLoopRemoveSource(
                    run_loop.as_concrete_TypeRef().cast(),
                    source_ref,
                    kCFRunLoopDefaultMode.cast(),
                );
                CFRelease(source_ref);
                CFRelease(port);
                let _ = Box::from_raw(context_ptr);
            })
            .ok()?;

        match ready_rx.recv_timeout(READY_TIMEOUT) {
            Ok(true) => Some(Self {
                pid,
                token,
                state,
                join: Some(join),
            }),
            Ok(false) | Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                state.stopped.store(true, Ordering::Release);
                let _ = join.join();
                None
            }
        }
    }

    /// Stop observation now and return captured tagged Cmd+V transitions.
    pub fn finish(mut self) -> PasteEventProbeResult {
        self.state.stopped.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let events = self
            .state
            .events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default();
        PasteEventProbeResult {
            pid: self.pid,
            token: self.token,
            events,
        }
    }
}

impl Drop for PasteEventProbe {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

unsafe extern "C" fn paste_event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if user_info.is_null() || event.is_null() {
        return event;
    }
    let context = &*(user_info.cast::<ProbeContext>());
    if event_type == CG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        let port = context.port.load(Ordering::Acquire);
        if !port.is_null() {
            CGEventTapEnable(port, true);
        }
        return event;
    }
    if event_type != CG_EVENT_KEY_DOWN && event_type != CG_EVENT_KEY_UP {
        return event;
    }

    let keycode = CGEventGetIntegerValueField(event, CG_KEYBOARD_EVENT_KEYCODE);
    let flags = CGEventGetFlags(event);
    let token = CGEventGetIntegerValueField(event, CG_EVENT_SOURCE_USER_DATA_FIELD);
    if keycode != KEY_V || flags & COMMAND_FLAG == 0 || token != context.token {
        return event;
    }

    // Callback stays content-free and bounded: CoreGraphics fields, one clock
    // read, and a tiny preallocated vector only. No AX, logging, or disk I/O.
    if let Ok(mut events) = context.state.events.lock() {
        if events.len() < MAX_EVENTS {
            events.push(PasteEvent {
                kind: if event_type == CG_EVENT_KEY_DOWN {
                    PasteEventKind::Down
                } else {
                    PasteEventKind::Up
                },
                token,
                keycode,
                flags,
                mach_time: mach_absolute_time(),
            });
        }
    }
    event
}
