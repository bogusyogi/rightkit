//! Frameless window chrome. macOS keeps the native traffic lights over an overlay title bar;
//! Windows/Linux drop decorations and the app draws its own title bar (the `@rightkit/react-shell`
//! TitleBar) using the `rk_shell_window` controls and `data-tauri-drag-region`.
//!
//! Donor: ViewRight `traffic_lights` (capture the macOS-default button origins once, apply a
//! horizontal/vertical offset, re-assert after resize/focus because AppKit resets them).

#[cfg(all(feature = "qa-probes", debug_assertions))]
use serde::Serialize;
use tauri::{WebviewWindow, WebviewWindowBuilder, Wry};

#[cfg(all(feature = "qa-probes", debug_assertions))]
use crate::error::ShellError;

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
/// ```text
/// app.manage(rightkit_tauri_shell::frameless::TrafficLights::default());
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
    /// Place the close / minimise / zoom buttons at an absolute inset instead of a relative offset: the
    /// close button's top-left corner sits `x` points from the window's left edge and `y` points from its
    /// top (y down), and the other two keep the macOS spacing. Replaces the last `set_offset` (and the
    /// reverse), and is re-asserted by [`TrafficLights::reapply`]. No-op off macOS.
    pub fn set_inset(&self, window: &WebviewWindow<Wry>, x: f64, y: f64) {
        #[cfg(target_os = "macos")]
        {
            let inner = std::sync::Arc::clone(&self.inner);
            macos::dispatch(window, move |window| {
                macos::apply_inset(window, x, y, &inner)
            });
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (window, x, y);
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

/// One standard window button's frame in its superview's coordinates (AppKit points, origin at the
/// bottom-left) and whether it is hidden. QA probe: compiled only with the `qa-probes` feature in a debug-assertions build.
#[cfg(all(feature = "qa-probes", debug_assertions))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ButtonFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub hidden: bool,
}

#[cfg(all(feature = "qa-probes", debug_assertions))]
impl ButtonFrame {
    /// Map an AppKit frame (`origin`, `size`) and hidden flag to the typed struct. Pure.
    pub fn new(origin: (f64, f64), size: (f64, f64), hidden: bool) -> Self {
        Self {
            x: origin.0,
            y: origin.1,
            width: size.0,
            height: size.1,
            hidden,
        }
    }
}

/// The three standard window buttons, read without changing anything. See [`button_frames`].
/// QA probe: compiled only with the `qa-probes` feature in a debug-assertions build.
#[cfg(all(feature = "qa-probes", debug_assertions))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonFrames {
    pub close: ButtonFrame,
    pub miniaturize: ButtonFrame,
    pub zoom: ButtonFrame,
    /// `NSWindow.isVisible` at the time of the read.
    pub window_visible: bool,
}

#[cfg(all(feature = "qa-probes", debug_assertions))]
impl ButtonFrames {
    /// `[close, miniaturize, zoom]`.
    pub fn buttons(&self) -> [ButtonFrame; 3] {
        [self.close, self.miniaturize, self.zoom]
    }

    /// The QA control-command shape ViewRight shipped:
    /// `{"buttons": [{"x","y","width","height","hidden"} x3 in close, miniaturize, zoom order], "windowVisible": bool}`.
    pub fn to_json(&self) -> String {
        serde_json::json!({ "buttons": self.buttons(), "windowVisible": self.window_visible })
            .to_string()
    }
}

/// Read-only probe of the macOS close / minimise / zoom buttons: each frame, hidden state, and
/// whether the window is visible. It never moves a button, orders the window in or activates the
/// app, so a QA scenario can assert where AppKit left the traffic lights (for example that an
/// overlay title bar kept them at the macOS default spot).
///
/// AppKit views are read on the main thread only: with the shell plugin installed the read runs
/// through the shell's main-thread dispatch (refused during shutdown); without it, inline when
/// already on the main thread and otherwise queued to it with a 5 s wait. Errors: `Other` when the
/// window has no NSWindow or a button, `Timeout` when the main thread does not answer. On every
/// platform other than macOS it returns [`ShellError::Unsupported`].
///
/// QA probe: compiled only with the `qa-probes` feature in a debug-assertions build (a release build refuses the feature).
#[cfg(all(feature = "qa-probes", debug_assertions))]
pub fn button_frames(window: &WebviewWindow<Wry>) -> Result<ButtonFrames, ShellError> {
    #[cfg(target_os = "macos")]
    {
        macos::frames(window)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        Err(ShellError::Unsupported(
            "window button frames exist only on macOS".into(),
        ))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Mutex;

    use objc2_app_kit::{NSWindow, NSWindowButton};
    use objc2_foundation::NSPoint;
    use tauri::{WebviewWindow, Wry};

    #[cfg(all(feature = "qa-probes", debug_assertions))]
    use crate::error::ShellError;

    const KINDS: [NSWindowButton; 3] = [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ];

    #[derive(Default)]
    pub struct Inner {
        defaults: Mutex<Option<[NSPoint; 3]>>,
        offset: Mutex<(f64, f64)>,
        /// The last absolute inset (`set_inset`); `None` while a relative offset is in force.
        inset: Mutex<Option<(f64, f64)>>,
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

    /// Read the three buttons on the main thread (see [`super::button_frames`]).
    #[cfg(all(feature = "qa-probes", debug_assertions))]
    pub fn frames(window: &WebviewWindow<Wry>) -> Result<super::ButtonFrames, ShellError> {
        use tauri::Manager;
        if window
            .app_handle()
            .try_state::<crate::ShellState>()
            .is_some()
        {
            return crate::windows::dispatch_native(window, read).map_err(ShellError::Other);
        }
        if unsafe { pthread_main_np() } == 1 {
            return read(window).map_err(ShellError::Other);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let target = window.clone();
        window
            .run_on_main_thread(move || {
                let _ = tx.send(read(&target));
            })
            .map_err(|e| ShellError::Other(format!("main thread unavailable: {e}")))?;
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| ShellError::Timeout("main thread did not answer within 5 s".into()))?
            .map_err(ShellError::Other)
    }

    #[cfg(all(feature = "qa-probes", debug_assertions))]
    fn read(window: &WebviewWindow<Wry>) -> Result<super::ButtonFrames, String> {
        let win = ns_window(window).ok_or("no NSWindow for this window")?;
        let mut out = [super::ButtonFrame::new((0.0, 0.0), (0.0, 0.0), false); 3];
        for (slot, (name, kind)) in out.iter_mut().zip([
            ("close", KINDS[0]),
            ("miniaturize", KINDS[1]),
            ("zoom", KINDS[2]),
        ]) {
            let button = win
                .standardWindowButton(kind)
                .ok_or_else(|| format!("window has no {name} button"))?;
            let frame = button.frame();
            *slot = super::ButtonFrame::new(
                (frame.origin.x, frame.origin.y),
                (frame.size.width, frame.size.height),
                button.isHidden(),
            );
        }
        Ok(super::ButtonFrames {
            close: out[0],
            miniaturize: out[1],
            zoom: out[2],
            window_visible: win.isVisible(),
        })
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
        *inner.inset.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Absolute placement: the close button's top-left corner sits `x` points from the window's left
    /// edge and `y` points from its top (y down); the other two keep the macOS spacing.
    pub fn apply_inset(window: &WebviewWindow<Wry>, x: f64, y: f64, inner: &Inner) {
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
            // SAFETY: `b` is a live NSButton owned by the window's titlebar view; AppKit
            // keeps its superview alive for the duration of this main-thread call.
            let Some(container) = (unsafe { b.superview() }) else {
                return;
            };
            let container_height = container.frame().size.height;
            let button_height = b.frame().size.height;
            // AppKit's origin is bottom-left, so the top-down inset is converted here.
            b.setFrameOrigin(NSPoint {
                x: x + (base[i].x - base[0].x),
                y: container_height - y - button_height,
            });
        }
        *inner.inset.lock().unwrap_or_else(|e| e.into_inner()) = Some((x, y));
    }

    pub fn reapply(window: &WebviewWindow<Wry>, inner: &Inner) {
        if inner
            .defaults
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            let inset = *inner.inset.lock().unwrap_or_else(|e| e.into_inner());
            match inset {
                Some((x, y)) => apply_inset(window, x, y, inner),
                None => {
                    let (dx, dy) = *inner.offset.lock().unwrap_or_else(|e| e.into_inner());
                    apply(window, dx, dy, inner);
                }
            }
        }
    }
}

#[cfg(all(test, feature = "qa-probes", debug_assertions))]
mod tests {
    use super::*;

    #[test]
    fn button_frame_maps_an_appkit_frame() {
        let f = ButtonFrame::new((7.0, 6.5), (14.0, 16.0), true);
        assert_eq!(
            f,
            ButtonFrame {
                x: 7.0,
                y: 6.5,
                width: 14.0,
                height: 16.0,
                hidden: true,
            }
        );
    }

    #[test]
    fn button_frames_json_keeps_the_viewright_shape_and_order() {
        let frames = ButtonFrames {
            close: ButtonFrame::new((7.0, 6.0), (14.0, 16.0), false),
            miniaturize: ButtonFrame::new((27.0, 6.0), (14.0, 16.0), false),
            zoom: ButtonFrame::new((47.0, 6.0), (14.0, 16.0), true),
            window_visible: true,
        };
        let v: serde_json::Value = serde_json::from_str(&frames.to_json()).unwrap();
        assert_eq!(v["windowVisible"], true);
        let b = v["buttons"].as_array().unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[0]["x"], 7.0);
        assert_eq!(b[1]["x"], 27.0);
        assert_eq!(b[2]["x"], 47.0);
        assert_eq!(b[0]["hidden"], false);
        assert_eq!(b[2]["hidden"], true);
        for button in b {
            for key in ["x", "y", "width", "height", "hidden"] {
                assert!(button.get(key).is_some(), "{key}");
            }
        }
        assert_eq!(frames.buttons()[1], frames.miniaturize);
    }
}
