//! macOS window helpers for the hub: order front and focus on the main thread and a titlebar
//! policy. Every function is a no-op on other platforms so a host calls them without `cfg`.
//!
//! All AppKit calls run on the main thread (`run_on_main_thread`) and every handle is the live
//! `NSWindow` Tauri returns for the window, checked for null before use.
//!
//! Reveal policy: [`order_front`] and [`focus_on_main`] unhide and activate the application and
//! order a window front, and [`configure_titlebar`] can restore a faded window's opacity. Each asks
//! the shell's guard (`lifecycle::reveal_mode`) with the host's [`RevealSource`] on the main thread,
//! immediately before the AppKit call, and does it only for `RevealMode::Foreground`.

#[cfg(target_os = "macos")]
use tauri::Manager;
use tauri::{AppHandle, Runtime, WebviewWindow};

#[cfg(target_os = "macos")]
use super::{reveal_mode, RevealMode};
use super::{EventSink, RevealSource};

/// What [`configure_titlebar`] applies to the window's native title bar. All fields are the host's;
/// there is no `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitlebarPolicy {
    /// Restore opacity to 1 (a close-to-anchor transition may have hidden the window at alpha 0).
    /// Applied only when the guard allows a foreground reveal: restoring opacity makes a faded
    /// window visible.
    pub restore_opacity: bool,
    /// Content extends under the title bar (the native controls float over the web content). `false`
    /// leaves the window's current style mask alone.
    pub full_size_content: bool,
    /// The title bar draws transparent (`true`) or opaque (`false`).
    pub transparent_titlebar: bool,
    /// The title text is hidden (`true`) or visible (`false`).
    pub hide_title: bool,
    /// Value for "movable by window background".
    pub movable_by_background: bool,
}

#[cfg(target_os = "macos")]
fn order_front_on_main<R: Runtime>(
    win: &WebviewWindow<R>,
    source: RevealSource,
    prefix: &str,
    sink: EventSink,
) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindow};

    let mode = reveal_mode(win.app_handle(), source);
    if mode != RevealMode::Foreground {
        sink(
            &format!("{prefix}_appkit_order_suppressed"),
            &format!("mode={mode:?} source={source:?}"),
        );
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        sink(
            &format!("{prefix}_appkit_order_skipped"),
            "not on main thread",
        );
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.unhide(None);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.activate();

    let Ok(ptr) = win.ns_window() else {
        sink(&format!("{prefix}_appkit_order_error"), "missing ns_window");
        return;
    };
    if ptr.is_null() {
        sink(&format!("{prefix}_appkit_order_error"), "null ns_window");
        return;
    }
    // SAFETY: Tauri returns the live NSWindow for this WebviewWindow. We only
    // order/activate it on the main thread.
    let ns_window: &NSWindow = unsafe { &*(ptr as *const NSWindow) };
    ns_window.makeKeyAndOrderFront(None);
    ns_window.orderFrontRegardless();
    sink(
        &format!("{prefix}_appkit_ordered_front"),
        &super::window_snapshot(win),
    );
}

/// Unhide and activate the application, then make the window key and order it front, on the main
/// thread, when the guard allows a foreground reveal for `source` (otherwise nothing is called and
/// `{prefix}_appkit_order_suppressed` is reported). Events: `{prefix}_appkit_order_suppressed`,
/// `{prefix}_appkit_order_skipped`, `{prefix}_appkit_order_error`, `{prefix}_appkit_ordered_front`.
#[cfg(target_os = "macos")]
pub fn order_front<R: Runtime>(
    app: &AppHandle<R>,
    win: &WebviewWindow<R>,
    source: RevealSource,
    prefix: &str,
    sink: EventSink,
) {
    let win = win.clone();
    let prefix = prefix.to_owned();
    let _ = app.run_on_main_thread(move || order_front_on_main(&win, source, &prefix, sink));
}

#[cfg(not(target_os = "macos"))]
pub fn order_front<R: Runtime>(
    _app: &AppHandle<R>,
    _win: &WebviewWindow<R>,
    _source: RevealSource,
    _prefix: &str,
    _sink: EventSink,
) {
}

/// [`order_front`] followed by `set_focus`, both on the main thread and both only when the guard
/// allows a foreground reveal for `source`. Event on a focus failure:
/// `{prefix}_main_thread_focus_error`.
#[cfg(target_os = "macos")]
pub fn focus_on_main<R: Runtime>(
    app: &AppHandle<R>,
    win: &WebviewWindow<R>,
    source: RevealSource,
    prefix: &str,
    sink: EventSink,
) {
    let win = win.clone();
    let prefix = prefix.to_owned();
    let _ = app.run_on_main_thread(move || {
        if reveal_mode(win.app_handle(), source) != RevealMode::Foreground {
            sink(
                &format!("{prefix}_appkit_order_suppressed"),
                &format!("focus source={source:?}"),
            );
            return;
        }
        order_front_on_main(&win, source, &prefix, sink);
        if let Err(err) = win.set_focus() {
            sink(
                &format!("{prefix}_main_thread_focus_error"),
                &err.to_string(),
            );
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn focus_on_main<R: Runtime>(
    _app: &AppHandle<R>,
    _win: &WebviewWindow<R>,
    _source: RevealSource,
    _prefix: &str,
    _sink: EventSink,
) {
}

#[cfg(target_os = "macos")]
fn configure_titlebar_on_main<R: Runtime>(
    win: &WebviewWindow<R>,
    policy: TitlebarPolicy,
    source: RevealSource,
    prefix: &str,
    sink: EventSink,
) {
    use objc2_app_kit::{NSWindow, NSWindowStyleMask, NSWindowTitleVisibility};

    let Ok(ptr) = win.ns_window() else {
        sink(
            &format!("{prefix}_titlebar_config_error"),
            "missing ns_window",
        );
        return;
    };
    if ptr.is_null() {
        sink(&format!("{prefix}_titlebar_config_error"), "null ns_window");
        return;
    }

    // SAFETY: Tauri returns the live NSWindow for this WebviewWindow. These flags keep the native
    // traffic lights on the web content surface instead of drawing a separate title strip.
    let ns_window: &NSWindow = unsafe { &*(ptr as *const NSWindow) };
    let mut applied = Vec::new();
    if policy.restore_opacity {
        // Alpha 1 makes a faded (alpha 0) window visible, so it is a reveal and asks the guard.
        if reveal_mode(win.app_handle(), source) == RevealMode::Foreground {
            ns_window.setAlphaValue(1.0);
            applied.push("alpha=1");
        } else {
            sink(
                &format!("{prefix}_titlebar_opacity_suppressed"),
                &format!("source={source:?}"),
            );
        }
    }
    if policy.full_size_content {
        ns_window.setStyleMask(ns_window.styleMask() | NSWindowStyleMask::FullSizeContentView);
        applied.push("full_size_content_view");
    }
    ns_window.setTitlebarAppearsTransparent(policy.transparent_titlebar);
    ns_window.setTitleVisibility(if policy.hide_title {
        NSWindowTitleVisibility::Hidden
    } else {
        NSWindowTitleVisibility::Visible
    });
    if policy.hide_title {
        applied.push("hidden_title");
    }
    if policy.transparent_titlebar {
        applied.push("transparent_titlebar");
    }
    ns_window.setMovableByWindowBackground(policy.movable_by_background);
    sink(&format!("{prefix}_titlebar_configured"), &applied.join(" "));
}

/// Apply `policy` to the window's native title bar, on the main thread. `source` is the guard source
/// for the opacity restore (see [`TitlebarPolicy::restore_opacity`]). Events:
/// `{prefix}_titlebar_config_error`, `{prefix}_titlebar_opacity_suppressed`,
/// `{prefix}_titlebar_configured`.
#[cfg(target_os = "macos")]
pub fn configure_titlebar<R: Runtime>(
    app: &AppHandle<R>,
    win: &WebviewWindow<R>,
    policy: TitlebarPolicy,
    source: RevealSource,
    prefix: &str,
    sink: EventSink,
) {
    let win = win.clone();
    let prefix = prefix.to_owned();
    let _ = app.run_on_main_thread(move || {
        configure_titlebar_on_main(&win, policy, source, &prefix, sink)
    });
}

#[cfg(not(target_os = "macos"))]
pub fn configure_titlebar<R: Runtime>(
    _app: &AppHandle<R>,
    _win: &WebviewWindow<R>,
    _policy: TitlebarPolicy,
    _source: RevealSource,
    _prefix: &str,
    _sink: EventSink,
) {
}
