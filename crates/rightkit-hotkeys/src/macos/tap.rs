//! Generic `CGEventTap` on a dedicated run-loop thread.
//!
//! The donor spawned one thread per hook and swallowed creation failures (a missing Accessibility
//! grant produced a silent no-op). [`EventTap::spawn`] performs a creation handshake so the caller
//! learns synchronously whether the tap exists, re-enables the tap when macOS disables it for a
//! slow callback, and tears the run loop down deterministically on drop.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use super::ffi::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapLocation {
    /// Sees events as they leave the HID layer (needed to swallow before any app).
    Hid,
    /// Session level; events posted with `CGEventPost(kCGSessionEventTap)` enter here.
    Session,
    /// After all session taps ran; swallowed events never arrive here.
    AnnotatedSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapKind {
    KeyDown,
    KeyUp,
    FlagsChanged,
    OtherMouseDown,
    /// Release of an "other" button; lets a tap that swallowed a press account for its release.
    OtherMouseUp,
}

impl TapKind {
    fn from_raw(etype: u32) -> Option<TapKind> {
        Some(match etype {
            K_CG_EVENT_KEY_DOWN => TapKind::KeyDown,
            K_CG_EVENT_KEY_UP => TapKind::KeyUp,
            K_CG_EVENT_FLAGS_CHANGED => TapKind::FlagsChanged,
            K_CG_EVENT_OTHER_MOUSE_DOWN => TapKind::OtherMouseDown,
            K_CG_EVENT_OTHER_MOUSE_UP => TapKind::OtherMouseUp,
            _ => return None,
        })
    }

    fn mask_bit(self) -> u64 {
        1u64 << match self {
            TapKind::KeyDown => K_CG_EVENT_KEY_DOWN,
            TapKind::KeyUp => K_CG_EVENT_KEY_UP,
            TapKind::FlagsChanged => K_CG_EVENT_FLAGS_CHANGED,
            TapKind::OtherMouseDown => K_CG_EVENT_OTHER_MOUSE_DOWN,
            TapKind::OtherMouseUp => K_CG_EVENT_OTHER_MOUSE_UP,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TapEvent {
    pub kind: TapKind,
    /// Virtual keycode for key/flags events, 0 for mouse events.
    pub keycode: u16,
    /// `CGEventFlags` bits (see the `FLAG_*` constants in [`super::ffi`]).
    pub flags: u64,
    /// HID button number for mouse events (2 = middle), -1 otherwise.
    pub button: i64,
    /// Event location in global display coordinates (mouse events).
    pub location: (f64, f64),
    /// `kCGEventSourceUserData`: lets a process recognise its own synthetic events.
    pub user_data: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapDecision {
    Pass,
    /// Consume the event so no application sees it. Ignored by listen-only taps.
    Swallow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapError {
    /// `CGEventTapCreate` returned NULL: Accessibility / Input Monitoring is not granted.
    NotPermitted,
    RunLoopSource,
    Thread(String),
}

impl std::fmt::Display for TapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TapError::NotPermitted => {
                write!(
                    f,
                    "CGEventTapCreate failed: grant Accessibility (and Input Monitoring)"
                )
            }
            TapError::RunLoopSource => {
                write!(f, "could not create the run-loop source for the tap")
            }
            TapError::Thread(e) => write!(f, "could not start the tap thread: {e}"),
        }
    }
}

impl std::error::Error for TapError {}

#[derive(Clone, Debug)]
pub struct TapConfig {
    pub name: String,
    pub location: TapLocation,
    pub listen_only: bool,
    pub kinds: Vec<TapKind>,
}

type Handler = Box<dyn Fn(&TapEvent) -> TapDecision + Send + 'static>;

type OnReenable = Box<dyn Fn() + Send + 'static>;

struct Ctx {
    handler: Handler,
    port: AtomicPtr<c_void>,
    /// Times macOS disabled the tap (slow callback or user input) and it was switched back on.
    reenables: Arc<AtomicU64>,
    on_reenable: Option<OnReenable>,
}

unsafe extern "C" fn callback(
    _proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if user_info.is_null() {
        return event;
    }
    let ctx = &*(user_info as *const Ctx);
    if etype == K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT || etype == K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        let port = ctx.port.load(Ordering::SeqCst);
        if !port.is_null() {
            CGEventTapEnable(port, true);
        }
        ctx.reenables.fetch_add(1, Ordering::SeqCst);
        // Events seen before the disable may have been lost; let the handler drop half-seen state.
        if let Some(on_reenable) = &ctx.on_reenable {
            on_reenable();
        }
        return event;
    }
    let Some(kind) = TapKind::from_raw(etype) else {
        return event;
    };
    if event.is_null() {
        return event;
    }
    let is_mouse = matches!(kind, TapKind::OtherMouseDown | TapKind::OtherMouseUp);
    let tap_event = TapEvent {
        kind,
        keycode: if is_mouse {
            0
        } else {
            CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16
        },
        flags: CGEventGetFlags(event),
        button: if is_mouse {
            CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER)
        } else {
            -1
        },
        location: if is_mouse {
            let p = CGEventGetLocation(event);
            (p.x, p.y)
        } else {
            (0.0, 0.0)
        },
        user_data: CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA),
    };
    match (ctx.handler)(&tap_event) {
        TapDecision::Pass => event,
        TapDecision::Swallow => std::ptr::null_mut(),
    }
}

/// A live event tap. Dropping it stops the run loop, invalidates the port and joins the thread.
pub struct EventTap {
    run_loop: usize,
    thread: Option<thread::JoinHandle<()>>,
    reenables: Arc<AtomicU64>,
}

impl EventTap {
    pub fn spawn(
        config: TapConfig,
        handler: impl Fn(&TapEvent) -> TapDecision + Send + 'static,
    ) -> Result<EventTap, TapError> {
        Self::spawn_inner(config, handler, None)
    }

    /// Like [`EventTap::spawn`], and runs `on_reenable` (inside the tap callback, so it must be
    /// quick) each time macOS disabled the tap and it was switched back on. Use it to drop any
    /// state that depended on seeing every event.
    pub fn spawn_with_recovery(
        config: TapConfig,
        handler: impl Fn(&TapEvent) -> TapDecision + Send + 'static,
        on_reenable: impl Fn() + Send + 'static,
    ) -> Result<EventTap, TapError> {
        Self::spawn_inner(config, handler, Some(Box::new(on_reenable)))
    }

    /// How many times the OS disabled this tap and it was re-enabled.
    pub fn reenable_count(&self) -> u64 {
        self.reenables.load(Ordering::SeqCst)
    }

    fn spawn_inner(
        config: TapConfig,
        handler: impl Fn(&TapEvent) -> TapDecision + Send + 'static,
        on_reenable: Option<OnReenable>,
    ) -> Result<EventTap, TapError> {
        let reenables = Arc::new(AtomicU64::new(0));
        let thread_reenables = reenables.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<usize, TapError>>();
        let handler: Handler = Box::new(handler);
        let thread = thread::Builder::new()
            .name(config.name.clone())
            .spawn(move || unsafe {
                run(config, handler, on_reenable, thread_reenables, ready_tx)
            })
            .map_err(|e| TapError::Thread(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(run_loop)) => Ok(EventTap {
                run_loop,
                thread: Some(thread),
                reenables,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => Err(TapError::Thread(
                "tap thread exited before reporting".into(),
            )),
        }
    }
}

impl Drop for EventTap {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // CFRunLoopStop is a no-op until the loop is running, and the handshake is sent just
            // before `CFRunLoopRun`; keep nudging until the thread has actually exited.
            while !thread.is_finished() {
                unsafe { CFRunLoopStop(self.run_loop as CFRunLoopRef) };
                thread::sleep(std::time::Duration::from_millis(2));
            }
            let _ = thread.join();
        }
    }
}

unsafe fn run(
    config: TapConfig,
    handler: Handler,
    on_reenable: Option<OnReenable>,
    reenables: Arc<AtomicU64>,
    ready: mpsc::Sender<Result<usize, TapError>>,
) {
    let ctx = Box::into_raw(Box::new(Ctx {
        handler,
        port: AtomicPtr::new(std::ptr::null_mut()),
        reenables,
        on_reenable,
    }));
    let mask = config.kinds.iter().fold(0u64, |m, k| m | k.mask_bit());
    let location = match config.location {
        TapLocation::Hid => K_CG_HID_EVENT_TAP,
        TapLocation::Session => K_CG_SESSION_EVENT_TAP,
        TapLocation::AnnotatedSession => K_CG_ANNOTATED_SESSION_EVENT_TAP,
    };
    let options = if config.listen_only {
        K_CG_EVENT_TAP_OPTION_LISTEN_ONLY
    } else {
        K_CG_EVENT_TAP_OPTION_DEFAULT
    };
    let port = CGEventTapCreate(
        location,
        K_CG_HEAD_INSERT_EVENT_TAP,
        options,
        mask,
        callback,
        ctx as *mut c_void,
    );
    if port.is_null() {
        drop(Box::from_raw(ctx));
        let _ = ready.send(Err(TapError::NotPermitted));
        return;
    }
    (*ctx).port.store(port, Ordering::SeqCst);
    let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
    if source.is_null() {
        CFRelease(port);
        drop(Box::from_raw(ctx));
        let _ = ready.send(Err(TapError::RunLoopSource));
        return;
    }
    let run_loop = CFRunLoopGetCurrent();
    CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
    CGEventTapEnable(port, true);
    let _ = ready.send(Ok(run_loop as usize));
    CFRunLoopRun();
    CGEventTapEnable(port, false);
    CFMachPortInvalidate(port);
    CFRelease(source);
    CFRelease(port);
    drop(Box::from_raw(ctx));
}
