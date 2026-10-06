//! Frameless window chrome. macOS keeps the native traffic lights over an overlay title bar;
//! Windows/Linux drop decorations and the app draws its own title bar (the `@rightkit/app-shell`
//! TitleBar) using the `rk_shell_window` controls and `data-tauri-drag-region`.
//!
//! Donor: ViewRight `traffic_lights` (capture the macOS-default button origins once, apply a
//! horizontal/vertical offset, re-assert after resize/focus because AppKit resets them).

use tauri::{WebviewWindow, WebviewWindowBuilder, Wry};

/// Style a builder for the frameless look. No-op when `frameless` is false.
pub fn style<'a>(b: WebviewWindowBuilder<'a, Wry, tauri::AppHandle<Wry>>, frameless: bool) -> WebviewWindowBuilder<'a, Wry, tauri::AppHandle<Wry>> {
    if !frameless {
        return b;
    }
    #[cfg(target_os = "macos")]
    {
        b.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        b.decorations(false)
    }
}

#[derive(Default)]
pub struct TrafficLights {
    #[cfg(target_os = "macos")]
    inner: std::sync::Arc<macos::Inner>,
}

impl TrafficLights {
    /// Offset the close/minimise/zoom buttons from their macOS default origin (x right, y down).
    /// No-op off macOS.
    pub fn set_offset(&self, window: &WebviewWindow<Wry>, dx: f64, dy: f64) {
        #[cfg(target_os = "macos")]
        {
            let inner = std::sync::Arc::clone(&self.inner);
            let _ = crate::windows::dispatch_native(window, move |window| {
                macos::apply(window, dx, dy, &inner);
                Ok(())
            });
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (window, dx, dy);
    }
    /// Re-assert the last offset (call from resize / focus / fullscreen events).
    pub fn reapply(&self, window: &WebviewWindow<Wry>) {
        #[cfg(target_os = "macos")]
        {
            let inner = std::sync::Arc::clone(&self.inner);
            let _ = crate::windows::dispatch_native(window, move |window| {
                macos::reapply(window, &inner);
                Ok(())
            });
        }
        #[cfg(not(target_os = "macos"))]
        let _ = window;
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Mutex;

    use objc2_app_kit::{NSWindow, NSWindowButton};
    use objc2_foundation::NSPoint;
    use tauri::{WebviewWindow, Wry};

    const KINDS: [NSWindowButton; 3] = [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton];

    #[derive(Default)]
    pub struct Inner {
        defaults: Mutex<Option<[NSPoint; 3]>>,
        offset: Mutex<(f64, f64)>,
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
            b.setFrameOrigin(NSPoint { x: base[i].x + dx, y: base[i].y - dy });
        }
        *inner.offset.lock().unwrap_or_else(|e| e.into_inner()) = (dx, dy);
    }

    pub fn reapply(window: &WebviewWindow<Wry>, inner: &Inner) {
        if inner.defaults.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            let (dx, dy) = *inner.offset.lock().unwrap_or_else(|e| e.into_inner());
            apply(window, dx, dy, inner);
        }
    }
}
