//! macOS event sources for the pill visibility guardian, for one guardian generation.
//!
//! Replaces the 250 ms fullscreen poll. Nothing here wakes on a timer:
//!
//! - `NSWorkspaceDidActivateApplicationNotification` (another app became frontmost)
//! - `NSWorkspaceActiveSpaceDidChangeNotification` (native fullscreen windows own a
//!   Space, so entering/leaving fullscreen switches Space)
//! - an `AXObserver` on the frontmost app: `AXFocusedWindowChanged` and
//!   `AXMainWindowChanged` on the application element, and `AXWindowResized`,
//!   `AXWindowMiniaturized`, `AXWindowDeminiaturized` on its focused window (window
//!   notifications are posted by the window element, so they are re-registered
//!   whenever the focused window changes).
//!
//! Threading. NSWorkspace delivers on the main thread, and the AX observer's run-loop
//! source is added to the MAIN run loop (common modes), so every callback runs on the
//! main thread and only enqueues a `GuardianEvent`. All AX IPC (observer install,
//! notification registration, focused-window lookup) happens on the guardian worker
//! via `GuardianSources::refresh`, never on the AppKit thread: AX is synchronous IPC
//! into a possibly slow Chromium/Terminal/Electron target. AppKit calls made on the
//! worker are wrapped in an autorelease pool because a plain `std::thread` has none.
//!
//! Generation. Every callback names the generation of the sources that registered it (the
//! observer object's ivar, the AX refcon) and reports to that generation only, so a callback that
//! outlives its sources is dropped once the worker cleared its slot.

use core_foundation::base::{CFType, TCFType};
use core_foundation::runloop::{
    kCFRunLoopCommonModes, CFRunLoop, CFRunLoopSource, CFRunLoopSourceRef,
};
use core_foundation::string::{CFString, CFStringRef};
use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass};
use objc2_app_kit::{
    NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
    NSWorkspaceDidActivateApplicationNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObject};
use rightkit_automation::macos::ax::{AxOptions, El};
use std::ffi::c_void;
use std::time::Duration;

use crate::guardian::{notify_generation, GuardianEvent, GuardianTimings};

/// Registered on the application element.
const APP_NOTIFICATIONS: [&str; 2] = ["AXFocusedWindowChanged", "AXMainWindowChanged"];
/// Registered on the focused window element.
const WINDOW_NOTIFICATIONS: [&str; 3] = [
    "AXWindowResized",
    "AXWindowMiniaturized",
    "AXWindowDeminiaturized",
];

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXObserverCreate(
        pid: i32,
        callback: extern "C" fn(*const c_void, *const c_void, *const c_void, *mut c_void),
        observer: *mut *const c_void,
    ) -> i32;
    fn AXObserverAddNotification(
        observer: *const c_void,
        element: *const c_void,
        notification: *const c_void,
        refcon: *mut c_void,
    ) -> i32;
    fn AXObserverRemoveNotification(
        observer: *const c_void,
        element: *const c_void,
        notification: *const c_void,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: *const c_void) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *const c_void);
    fn CFHash(cf: *const c_void) -> usize;
}

/// Maps an AX notification name to the guardian event it implies.
fn event_for_ax_notification(name: &str) -> GuardianEvent {
    match name {
        "AXWindowResized" => GuardianEvent::GeometryChanged,
        // Focused/main window changes and minimise/restore: the window that
        // owns the screen may have changed, so re-evaluate and re-check the pill.
        _ => GuardianEvent::WindowStateChanged,
    }
}

/// Runs on the main thread (the observer source lives on the main run loop). `refcon` carries the
/// generation the observer was registered for (a number, never dereferenced).
extern "C" fn ax_callback(
    _observer: *const c_void,
    _element: *const c_void,
    notification: *const c_void,
    refcon: *mut c_void,
) {
    let generation = refcon as usize as u64;
    // A panic must not unwind into the C run loop that called this: an event that cannot be
    // decoded is reported as the broadest one (re-evaluate), which is always safe.
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if notification.is_null() {
            return GuardianEvent::WindowStateChanged;
        }
        let name =
            unsafe { CFString::wrap_under_get_rule(notification as CFStringRef) }.to_string();
        event_for_ax_notification(&name)
    }));
    notify_generation(
        generation,
        decoded.unwrap_or(GuardianEvent::WindowStateChanged),
    );
}

struct GuardianObserverIvars {
    generation: u64,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RightKitPillGuardianObserver"]
    #[ivars = GuardianObserverIvars]
    struct GuardianObserver;

    impl GuardianObserver {
        #[unsafe(method(guardianApplicationDidActivate:))]
        fn application_did_activate(&self, _notification: &NSNotification) {
            notify_generation(self.ivars().generation, GuardianEvent::ForegroundChanged);
        }

        #[unsafe(method(guardianActiveSpaceDidChange:))]
        fn active_space_did_change(&self, _notification: &NSNotification) {
            notify_generation(self.ivars().generation, GuardianEvent::SpaceChanged);
        }
    }
);

impl GuardianObserver {
    fn new(generation: u64) -> Retained<Self> {
        let this = Self::alloc().set_ivars(GuardianObserverIvars { generation });
        unsafe { msg_send![super(this), init] }
    }
}

struct WorkspaceObserver {
    center: Retained<NSNotificationCenter>,
    observer: Retained<GuardianObserver>,
}

impl WorkspaceObserver {
    fn install(generation: u64) -> Self {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let observer = GuardianObserver::new(generation);
        unsafe {
            center.addObserver_selector_name_object(
                &observer,
                sel!(guardianApplicationDidActivate:),
                Some(NSWorkspaceDidActivateApplicationNotification),
                None,
            );
            center.addObserver_selector_name_object(
                &observer,
                sel!(guardianActiveSpaceDidChange:),
                Some(NSWorkspaceActiveSpaceDidChangeNotification),
                None,
            );
        }
        Self { center, observer }
    }
}

impl Drop for WorkspaceObserver {
    fn drop(&mut self) {
        unsafe { self.center.removeObserver(&self.observer) };
    }
}

struct WatchedWindow {
    element: CFType,
    identity: isize,
}

/// An `AXObserver` on one application, with its run-loop source on the main loop.
struct AxWatch {
    pid: i32,
    /// The generation the callbacks report to, passed as the notification refcon.
    generation: u64,
    /// The host's bound for one Accessibility request (`GuardianTimings::ax_messaging_timeout_ms`).
    ax_timeout: Duration,
    observer: *const c_void,
    source: CFRunLoopSource,
    /// Held only to keep the application element alive for the observer.
    _app: CFType,
    window: Option<WatchedWindow>,
}

fn add_notification(
    observer: *const c_void,
    element: *const c_void,
    name: &'static str,
    generation: u64,
) -> bool {
    let notification = CFString::from_static_string(name);
    unsafe {
        AXObserverAddNotification(
            observer,
            element,
            notification.as_CFTypeRef(),
            generation as usize as *mut c_void,
        ) == 0
    }
}

fn remove_notification(observer: *const c_void, element: *const c_void, name: &'static str) {
    let notification = CFString::from_static_string(name);
    // Fails harmlessly when the window is already gone.
    let _ = unsafe { AXObserverRemoveNotification(observer, element, notification.as_CFTypeRef()) };
}

/// The app's focused (else main) window, plus a stable identity for change detection.
fn focused_window(pid: i32, ax_timeout: Duration) -> Option<WatchedWindow> {
    let app = El::app_with(pid, &AxOptions::with_timeout(ax_timeout)).ok()?;
    let window = app
        .el_attr("AXFocusedWindow")
        .or_else(|| app.el_attr("AXMainWindow"))?;
    window.set_timeout(ax_timeout);
    let identity = unsafe { CFHash(window.as_raw()) } as isize;
    let element = unsafe { CFType::wrap_under_get_rule(window.as_raw()) };
    Some(WatchedWindow { element, identity })
}

impl AxWatch {
    fn install(pid: i32, generation: u64, ax_timeout: Duration) -> Option<Self> {
        let app = El::app_with(pid, &AxOptions::with_timeout(ax_timeout)).ok()?;
        let app_ref = unsafe { CFType::wrap_under_get_rule(app.as_raw()) };
        let mut observer: *const c_void = std::ptr::null();
        if unsafe { AXObserverCreate(pid, ax_callback, &mut observer) } != 0 || observer.is_null() {
            return None;
        }
        let app_element = app_ref.as_CFTypeRef();
        let mut registered = false;
        for name in APP_NOTIFICATIONS {
            registered |= add_notification(observer, app_element, name, generation);
        }
        let source_ref = unsafe { AXObserverGetRunLoopSource(observer) };
        if !registered || source_ref.is_null() {
            unsafe { CFRelease(observer) };
            return None;
        }
        let source =
            unsafe { CFRunLoopSource::wrap_under_get_rule(source_ref as CFRunLoopSourceRef) };
        // CFRunLoopAddSource is thread-safe; the callbacks run on the main thread.
        CFRunLoop::get_main().add_source(&source, unsafe { kCFRunLoopCommonModes });
        let mut watch = Self {
            pid,
            generation,
            ax_timeout,
            observer,
            source,
            _app: app_ref,
            window: None,
        };
        watch.rewatch_window();
        Some(watch)
    }

    /// Re-point the window-level notifications at the app's current focused window.
    fn rewatch_window(&mut self) {
        let current = focused_window(self.pid, self.ax_timeout);
        match (&self.window, &current) {
            (Some(old), Some(new)) if old.identity == new.identity => return,
            (None, None) => return,
            _ => {}
        }
        if let Some(old) = self.window.take() {
            for name in WINDOW_NOTIFICATIONS {
                remove_notification(self.observer, old.element.as_CFTypeRef(), name);
            }
        }
        if let Some(new) = current {
            let element = new.element.as_CFTypeRef();
            for name in WINDOW_NOTIFICATIONS {
                add_notification(self.observer, element, name, self.generation);
            }
            self.window = Some(new);
        }
    }
}

impl Drop for AxWatch {
    fn drop(&mut self) {
        CFRunLoop::get_main().remove_source(&self.source, unsafe { kCFRunLoopCommonModes });
        unsafe { CFRelease(self.observer) };
    }
}

/// All macOS event sources. Created, refreshed and dropped on the guardian worker.
pub(crate) struct GuardianSources {
    generation: u64,
    ax_timeout: Duration,
    _workspace: WorkspaceObserver,
    ax: Option<AxWatch>,
}

impl GuardianSources {
    pub(crate) fn start(
        generation: u64,
        _pump_thread_name: &str,
        timings: &GuardianTimings,
    ) -> Option<Self> {
        objc2::rc::autoreleasepool(|_| {
            Some(Self {
                generation,
                ax_timeout: Duration::from_millis(timings.ax_messaging_timeout_ms),
                _workspace: WorkspaceObserver::install(generation),
                ax: None,
            })
        })
    }

    /// Called by the worker before each evaluation: follow the frontmost pid with
    /// the AX observer, and the observer's window-level notifications with the
    /// focused window. AX registration is retried here on the next event if it
    /// failed (for example Accessibility not granted yet); never on a timer.
    pub(crate) fn refresh(&mut self) {
        objc2::rc::autoreleasepool(|_| {
            let pid = NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|app| app.processIdentifier());
            let own_pid = std::process::id() as i32;
            // The host's own windows are never a fullscreen app to react to.
            let Some(pid) = pid.filter(|pid| *pid != own_pid) else {
                self.ax = None;
                return;
            };
            if let Some(watch) = self.ax.as_mut().filter(|watch| watch.pid == pid) {
                watch.rewatch_window();
                return;
            }
            // Drop the old observer (and its source) before installing the new one.
            drop(self.ax.take());
            self.ax = AxWatch::install(pid, self.generation, self.ax_timeout);
        });
    }
}
