//! Frameless window chrome. macOS keeps the native traffic lights over an overlay title bar;
//! Windows/Linux drop decorations and the app draws its own title bar (the `@rightkit/app-shell`
//! TitleBar) using the `rk_shell_window` controls and `data-tauri-drag-region`.
//!
//! Donor: ViewRight `traffic_lights` (capture the macOS-default button origins once, apply a
//! horizontal/vertical offset, re-assert after resize/focus because AppKit resets them).

use tauri::{WebviewWindow, WebviewWindowBuilder, Wry};

/// Style a builder for the frameless look. No-op when `frameless` is false.
pub fn style<'a>(
    b: WebviewWindowBuilder<'a, Wry, tauri::AppHandle<Wry>>,
    frameless: bool,
) -> WebviewWindowBuilder<'a, Wry, tauri::AppHandle<Wry>> {
    if !frameless {
        return b;
    }
    #[cfg(target_os = "macos")]
    {
        b.title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        b.decorations(false)
    }
}

/// The macOS traffic-light offset, usable on its own.
///
/// A plain library type: it needs neither the shell plugin nor the managed `ShellState`. An app that
/// keeps its own lifecycle manages one itself and forwards window events to it:
///
/// ```ignore
/// app.manage(rightkit_shell::frameless::TrafficLights::default());
/// // in `on_window_event`:
/// if let Some(t) = app.try_state::<TrafficLights>() { t.handle_window_event(&webview, event); }
/// // in a command: `t.set_offset(&window, dx, dy)`
/// ```
///
/// With the shell plugin installed the calls run through the shell's main-thread dispatch (which
/// rejects work during shutdown). Without it they run inline when already on the main thread and are
/// queued to it otherwise (fire and forget), so no lifecycle hook or single-instance claim is needed.
/// Cloning shares the captured default origins and the last offset.
#[derive(Default, Clone)]
pub struct TrafficLights {
    #[cfg(target_os = "macos")]
    inner: std::sync::Arc<macos::Inner>,
}

impl std::fmt::Debug for TrafficLights {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrafficLights").finish_non_exhaustive()
    }
}

impl TrafficLights {
    /// A fresh instance with no offset applied.
    pub fn new() -> Self {
        Self::default()
    }

    /// Offset the close/minimise/zoom buttons from their macOS default origin (x right, y down).
    /// No-op off macOS.
    pub fn set_offset(&self, window: &WebviewWindow<Wry>, dx: f64, dy: f64) {
        #[cfg(target_os = "macos")]
        {
            let inner = std::sync::Arc::clone(&self.inner);
            macos::dispatch(window, move |window| macos::apply(window, dx, dy, &inner));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (window, dx, dy);
    }
    /// Re-assert the last offset (call from resize / focus / fullscreen events).
    pub fn reapply(&self, window: &WebviewWindow<Wry>) {
        #[cfg(target_os = "macos")]
        {
            let inner = std::sync::Arc::clone(&self.inner);
            macos::dispatch(window, move |window| macos::reapply(window, &inner));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = window;
    }
    /// Re-assert the offset when `event` is one AppKit answers by resetting the buttons: a resize or
    /// a focus gain (fullscreen exit refocuses). Other events are ignored. No-op off macOS.
    pub fn handle_window_event(&self, window: &WebviewWindow<Wry>, event: &tauri::WindowEvent) {
        if matches!(
            event,
            tauri::WindowEvent::Resized(_)
                | tauri::WindowEvent::Focused(true)
                | tauri::WindowEvent::ScaleFactorChanged { .. }
        ) {
            self.reapply(window);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Mutex;

    use objc2_app_kit::{NSWindow, NSWindowButton};
    use objc2_foundation::NSPoint;
    use tauri::{WebviewWindow, Wry};

    const KINDS: [NSWindowButton; 3] = [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ];

    #[derive(Default)]
    pub struct Inner {
        defaults: Mutex<Option<[NSPoint; 3]>>,
        offset: Mutex<(f64, f64)>,
    }

    extern "C" {
        /// libSystem: 1 on the process main thread, 0 elsewhere.
        fn pthread_main_np() -> std::os::raw::c_int;
    }

    /// Run `f` on the main thread. Through the shell's dispatch when `ShellState` is managed;
    /// otherwise inline on the main thread, or queued to it (no result is awaited).
    pub fn dispatch<F>(window: &WebviewWindow<Wry>, f: F)
    where
        F: FnOnce(&WebviewWindow<Wry>) + Send + 'static,
    {
        use tauri::Manager;
        if window
            .app_handle()
            .try_state::<crate::ShellState>()
            .is_some()
        {
            let _ = crate::windows::dispatch_native(window, move |w| {
                f(w);
                Ok(())
            });
        } else if unsafe { pthread_main_np() } == 1 {
            f(window);
        } else {
            let w = window.clone();
            let _ = window.run_on_main_thread(move || f(&w));
        }
    }

    fn ns_window(window: &WebviewWindow<Wry>) -> Option<&NSWindow> {
        let ptr = window.ns_window().ok()?;
        if ptr.is_null() {
            return None;
        }
        // SAFETY: Tauri returns the live NSWindow of this window; used synchronously.
        Some(unsafe { &*(ptr as *const NSWindow) })
    }

    pub fn apply(window: &WebviewWindow<Wry>, dx: f64, dy: f64, inner: &Inner) {
        let Some(win) = ns_window(window) else { return };
        let mut buttons = Vec::with_capacity(3);
        for k in KINDS {
            match win.standardWindowButton(k) {
                Some(b) => buttons.push(b),
                None => return,
            }
        }
        let base = {
            let mut d = inner.defaults.lock().unwrap_or_else(|e| e.into_inner());
            *d.get_or_insert_with(|| {
                let mut o = [NSPoint { x: 0.0, y: 0.0 }; 3];
                for (i, b) in buttons.iter().enumerate() {
                    o[i] = b.frame().origin;
                }
                o
            })
        };
        for (i, b) in buttons.iter().enumerate() {
            // AppKit's origin is bottom-left, so a downward offset subtracts.
            b.setFrameOrigin(NSPoint {
                x: base[i].x + dx,
                y: base[i].y - dy,
            });
        }
        *inner.offset.lock().unwrap_or_else(|e| e.into_inner()) = (dx, dy);
    }

    pub fn reapply(window: &WebviewWindow<Wry>, inner: &Inner) {
        if inner
            .defaults
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            let (dx, dy) = *inner.offset.lock().unwrap_or_else(|e| e.into_inner());
            apply(window, dx, dy, inner);
        }
    }
}
