//! The hand-over between a window and an anchor rectangle: the maths, the timing loop and, on
//! Windows (feature `overlay`), the native opaque shell that grows from, or returns to, the anchor
//! while the warm webview window stays hidden.
//!
//! macOS renders the same hand-over in an AppKit panel owned by the host; on Windows this shell is
//! the equivalent, so a webview resize never distorts the window during the transition and focus
//! stays in the target application (the shell is created non-activating).
//!
//! Host inputs: the anchor's native handle and visible rectangle, the clearance above the anchor,
//! the timing and easing, the shell's class and window names, its fill colour and corner limit, and the reveal
//! source. The crate knows no product and no colour.
//!
//! Reveal policy: the shell window is drawn on screen, so the Windows path asks the shell's guard
//! (`lifecycle::reveal_mode`) before it creates the shell. Only `RevealMode::Foreground` draws.
//! Under `Forbidden` nothing is created, shown or moved on screen and the morph completes
//! logically; under `Background` no shell is drawn either, and the window is revealed the way the
//! shell reveals in the background (ordered back, no activation, no focus). The shell's creation,
//! frame and destroy functions are private, so nothing outside `from_anchor` and `to_anchor` can
//! draw it.
//!
//! Bounds on every public numeric input (all enforced in the constructors, whose fields are
//! private, so a struct literal cannot bypass them): see [`MorphFrame::new`], [`MorphTiming::new`],
//! [`MAX_CLEARANCE_PX`], [`MAX_FRAMES`] and, with feature `overlay`, `ShellStyle::new`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Runtime};

#[cfg(all(windows, feature = "overlay"))]
use super::{reveal_mode, EventSink, RevealMode, RevealSource};

/// A frame's `x` and `y` are held to `-MAX_FRAME_COORD..=MAX_FRAME_COORD` physical pixels.
pub const MAX_FRAME_COORD: i32 = 1_000_000;
/// A frame's width and height are held to `1..=MAX_FRAME_EXTENT` physical pixels.
pub const MAX_FRAME_EXTENT: i32 = 16_384;
/// The clearance above an anchor is held to `0..=MAX_CLEARANCE_PX` physical pixels.
pub const MAX_CLEARANCE_PX: i32 = 1024;
/// The most intermediate frames [`run`] ever posts (`MAX_DURATION / MIN_FRAME` plus two).
pub const MAX_FRAMES: u32 = 502;

/// A rectangle in physical pixels. The fields are private: every value passed through [`new`] is
/// held to the bounds above.
///
/// [`new`]: MorphFrame::new
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MorphFrame {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl MorphFrame {
    /// `x` and `y` held to `-1_000_000..=1_000_000`, width and height to `1..=16_384`.
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x: x.clamp(-MAX_FRAME_COORD, MAX_FRAME_COORD),
            y: y.clamp(-MAX_FRAME_COORD, MAX_FRAME_COORD),
            width: width.clamp(1, MAX_FRAME_EXTENT),
            height: height.clamp(1, MAX_FRAME_EXTENT),
        }
    }

    pub fn x(self) -> i32 {
        self.x
    }

    pub fn y(self) -> i32 {
        self.y
    }

    pub fn width(self) -> i32 {
        self.width
    }

    pub fn height(self) -> i32 {
        self.height
    }

    /// From a floating-point `[x, y, width, height]` (an anchor that reports its settled capsule
    /// rectangle). Non-finite values read as 0 (and 1 for the size); then the bounds of [`new`].
    ///
    /// [`new`]: MorphFrame::new
    pub fn from_rect_f64([x, y, width, height]: [f64; 4]) -> Self {
        let int = |v: f64| if v.is_finite() { v.round() as i32 } else { 0 };
        Self::new(int(x), int(y), int(width), int(height))
    }

    /// The frame `progress` of the way from `self` to `destination` (`0.0` is `self`, `1.0` is
    /// `destination`), each field rounded. `progress` is clamped to `0.0..=1.0` and a non-finite
    /// value reads as `0.0`.
    pub fn interpolate(self, destination: Self, progress: f64) -> Self {
        let progress = if progress.is_finite() {
            progress.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let lerp =
            |from: i32, to: i32| (from as f64 + (to - from) as f64 * progress).round() as i32;
        Self::new(
            lerp(self.x, destination.x),
            lerp(self.y, destination.y),
            lerp(self.width, destination.width),
            lerp(self.height, destination.height),
        )
    }

    /// Where a window of this size lands above `anchor` (top-left coordinates, y grows down): its
    /// bottom edge `clearance_px` above the anchor's top edge, or where it already is when that is
    /// higher. `clearance_px` is held to `0..=`[`MAX_CLEARANCE_PX`].
    pub fn destination_above(self, anchor: Self, clearance_px: i32) -> Self {
        let clearance = clearance_px.clamp(0, MAX_CLEARANCE_PX);
        let required_y = anchor.y - clearance - self.height;
        Self::new(self.x, self.y.min(required_y), self.width, self.height)
    }

    /// Corner radius for a shell of this frame: the smaller side, at most `max_px`, at least 1.
    pub fn corner_radius(self, max_px: i32) -> i32 {
        self.width.min(self.height).min(max_px).max(1)
    }
}

/// Ease-out quartic: fast start, slow landing. `linear` is clamped to `0.0..=1.0`.
pub fn ease_out_quart(linear: f64) -> f64 {
    let linear = if linear.is_finite() {
        linear.clamp(0.0, 1.0)
    } else {
        0.0
    };
    1.0 - (1.0 - linear).powi(4)
}

/// Duration and frame interval of a morph. The fields are private; [`MorphTiming::new`] is the only
/// constructor, so every value is within the documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MorphTiming {
    duration: Duration,
    frame: Duration,
}

impl MorphTiming {
    pub const MAX_DURATION: Duration = Duration::from_millis(2000);
    pub const MIN_DURATION: Duration = Duration::from_millis(1);
    pub const MIN_FRAME: Duration = Duration::from_millis(4);
    pub const MAX_FRAME: Duration = Duration::from_millis(100);

    /// `duration` held to 1..=2000 ms and `frame` to 4..=100 ms.
    pub fn new(duration: Duration, frame: Duration) -> Self {
        Self {
            duration: duration.clamp(Self::MIN_DURATION, Self::MAX_DURATION),
            frame: frame.clamp(Self::MIN_FRAME, Self::MAX_FRAME),
        }
    }

    pub fn duration(self) -> Duration {
        self.duration
    }

    pub fn frame(self) -> Duration {
        self.frame
    }
}

/// Run a morph from `source` to `destination` on a worker thread. Each intermediate frame
/// (progress below 1, shaped by the host's `ease`, for example [`ease_out_quart`]; a result outside
/// `0.0..=1.0` or non-finite is held to that range) is handed to `apply` on the main thread; the
/// final frame (`destination` exactly) and then `completion` run together in one main-thread task.
///
/// Bounded whatever the host passes: the loop ends when the (at most 2 s) duration has elapsed, or
/// after [`MAX_FRAMES`] posts, or at the first failed post to the main thread (the event loop is
/// gone; the final task is then not posted either). Each sleep is at most 100 ms. This function
/// shows no window; `apply` and `completion` are the host's closures.
pub fn run<R: Runtime>(
    app: AppHandle<R>,
    source: MorphFrame,
    destination: MorphFrame,
    timing: MorphTiming,
    ease: fn(f64) -> f64,
    apply: Arc<dyn Fn(MorphFrame) + Send + Sync>,
    completion: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        let started = Instant::now();
        let duration = timing.duration.as_secs_f64();
        let mut posted = 0u32;
        let mut loop_gone = false;
        while posted < MAX_FRAMES {
            let linear = (started.elapsed().as_secs_f64() / duration).min(1.0);
            if linear >= 1.0 {
                break;
            }
            let frame = source.interpolate(destination, ease(linear));
            let apply = Arc::clone(&apply);
            if app.run_on_main_thread(move || apply(frame)).is_err() {
                loop_gone = true;
                break;
            }
            posted += 1;
            std::thread::sleep(timing.frame);
        }
        if !loop_gone {
            let _ = app.run_on_main_thread(move || {
                apply(destination);
                completion();
            });
        }
    });
}

#[cfg(all(windows, feature = "overlay"))]
pub use native::*;

/// The Windows native shell (feature `overlay`; `rightkit_overlay` owns the window styles).
#[cfg(all(windows, feature = "overlay"))]
mod native {
    use std::sync::{Arc, Mutex};

    use tauri::{Manager, Runtime, WebviewWindow, Window};
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, CreateSolidBrush, SetWindowRgn};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, GetWindowRect, IsWindow, RegisterClassW, SetWindowPos,
        SWP_NOACTIVATE, SWP_SHOWWINDOW, WNDCLASSW,
    };

    use super::{
        reveal_mode, run, EventSink, MorphFrame, MorphTiming, RevealMode, RevealSource,
        MAX_CLEARANCE_PX,
    };

    /// The most distinct window class names [`ShellStyle`] may register in one process.
    pub const MAX_SHELL_CLASSES: usize = 8;

    /// An anchor the morph starts from or lands on: its native window handle (the shell sits just
    /// above it in z-order) and its visible rectangle in physical pixels.
    #[derive(Debug, Clone, Copy)]
    pub struct Anchor {
        hwnd: isize,
        frame: MorphFrame,
    }

    impl Anchor {
        /// `hwnd` must be non-zero (it is checked again against `IsWindow` when the morph starts).
        pub fn new(hwnd: isize, frame: MorphFrame) -> Result<Self, String> {
            if hwnd == 0 {
                return Err("morph anchor handle is null".into());
            }
            Ok(Self { hwnd, frame })
        }

        pub fn hwnd(&self) -> isize {
            self.hwnd
        }

        pub fn frame(&self) -> MorphFrame {
            self.frame
        }

        fn check_live(&self) -> Result<(), String> {
            if unsafe { IsWindow(self.hwnd as HWND) } == 0 {
                return Err("morph anchor is not a live window".into());
            }
            Ok(())
        }
    }

    /// How the native shell looks and is named. All values are the host's; the fields are private
    /// and [`ShellStyle::new`] validates them.
    #[derive(Debug, Clone)]
    pub struct ShellStyle {
        class_name: String,
        window_name: String,
        fill_colorref: u32,
        max_corner_px: i32,
    }

    impl ShellStyle {
        /// - `class_name`: Win32 class name, 1..=256 UTF-16 units, no NUL.
        /// - `window_name`: window name (title) of the shell window; at most 256 UTF-16 units, no NUL;
        ///   may be empty.
        /// - `fill_colorref`: fill as a Win32 `COLORREF` (`0x00BBGGRR`, at most `0x00FFFFFF`).
        /// - `max_corner_px`: largest corner radius in physical pixels, `0..=512`.
        pub fn new(
            class_name: impl Into<String>,
            window_name: impl Into<String>,
            fill_colorref: u32,
            max_corner_px: i32,
        ) -> Result<Self, String> {
            let style = Self {
                class_name: class_name.into(),
                window_name: window_name.into(),
                fill_colorref,
                max_corner_px,
            };
            style.validate()?;
            Ok(style)
        }

        fn validate(&self) -> Result<(), String> {
            let name_ok = !self.class_name.is_empty()
                && self.class_name.encode_utf16().count() <= 256
                && !self.class_name.contains('\0');
            if !name_ok {
                return Err("morph shell class name must be 1..=256 characters, no NUL".into());
            }
            if self.window_name.contains('\0') || self.window_name.encode_utf16().count() > 256 {
                return Err(
                    "morph shell window name must be at most 256 characters, no NUL".into(),
                );
            }
            if self.fill_colorref > 0x00FF_FFFF {
                return Err("morph shell fill is not a COLORREF".into());
            }
            if !(0..=512).contains(&self.max_corner_px) {
                return Err("morph shell corner limit must be 0..=512".into());
            }
            Ok(())
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    static REGISTERED: Mutex<Vec<(String, Result<(), String>)>> = Mutex::new(Vec::new());

    fn ensure_class(style: &ShellStyle) -> Result<(), String> {
        let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, result)) = registered
            .iter()
            .find(|(name, _)| *name == style.class_name)
        {
            return result.clone();
        }
        if registered.len() >= MAX_SHELL_CLASSES {
            return Err(format!(
                "morph shell: more than {MAX_SHELL_CLASSES} distinct class names"
            ));
        }
        let result = unsafe {
            let brush = CreateSolidBrush(style.fill_colorref);
            if brush.is_null() {
                Err("CreateSolidBrush failed for morph shell".to_string())
            } else {
                let class_name = wide(&style.class_name);
                let class = WNDCLASSW {
                    hInstance: GetModuleHandleW(std::ptr::null()),
                    hbrBackground: brush,
                    lpfnWndProc: Some(DefWindowProcW),
                    lpszClassName: class_name.as_ptr(),
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    // Zero when another first use raced us; the lock above makes that impossible
                    // within this process, so a zero is a real failure.
                    Err("RegisterClassW failed for morph shell".to_string())
                } else {
                    Ok(())
                }
            }
        };
        registered.push((style.class_name.clone(), result.clone()));
        result
    }

    /// A window's current frame in physical pixels.
    fn window_frame(hwnd: isize) -> Result<MorphFrame, String> {
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        if unsafe { GetWindowRect(hwnd as HWND, &mut rect) } == 0 {
            return Err("GetWindowRect failed for morph".to_string());
        }
        Ok(MorphFrame::new(
            rect.left,
            rect.top,
            rect.right.saturating_sub(rect.left),
            rect.bottom.saturating_sub(rect.top),
        ))
    }

    /// Place and round the shell, just above `anchor` in z-order, without activating it. Private:
    /// with `show` it makes the shell visible, so only the guarded morph entry points reach it.
    fn set_frame(style: &ShellStyle, shell: isize, anchor: isize, frame: MorphFrame, show: bool) {
        unsafe {
            // The region is owned by Windows after SetWindowRgn succeeds.
            let corner = frame.corner_radius(style.max_corner_px);
            let region = CreateRoundRectRgn(0, 0, frame.width(), frame.height(), corner, corner);
            if !region.is_null() {
                let _ = SetWindowRgn(shell as HWND, region, 1);
            }
            let flags = SWP_NOACTIVATE | if show { SWP_SHOWWINDOW } else { 0 };
            let _ = SetWindowPos(
                shell as HWND,
                anchor as HWND,
                frame.x(),
                frame.y(),
                frame.width(),
                frame.height(),
                flags,
            );
        }
    }

    /// Create the shell at `frame`, above `anchor`, shown and non-activating (`rightkit_overlay`
    /// `OverlayStyle::PASSIVE`: topmost, tool window, never activates). Private: it draws on
    /// screen, so it runs only after the reveal guard allowed a foreground morph.
    fn create(style: &ShellStyle, frame: MorphFrame, anchor: isize) -> Result<isize, String> {
        ensure_class(style)?;
        let shell = rightkit_overlay::windows::create_overlay_window(
            unsafe { GetModuleHandleW(std::ptr::null()) } as isize,
            &style.class_name,
            &style.window_name,
            rightkit_overlay::OverlayRect {
                x: frame.x(),
                y: frame.y(),
                width: frame.width(),
                height: frame.height(),
            },
            rightkit_overlay::OverlayStyle::PASSIVE,
        );
        if shell == 0 {
            return Err("create_overlay_window failed for morph shell".to_string());
        }
        set_frame(style, shell, anchor, frame, true);
        Ok(shell)
    }

    fn destroy(shell: isize) {
        unsafe { DestroyWindow(shell as HWND) };
    }

    /// Grow `win` out of `anchor`. Asks the shell's guard for `source` first. Under `Foreground`:
    /// position and size the hidden, warm window at its destination above the anchor
    /// (`clearance_px`, held to `0..=`[`MAX_CLEARANCE_PX`], between them), run `before_start`,
    /// animate the shell, then destroy it, reveal the window through the guard (show and focus) and
    /// call `on_shown`. Under any other mode no shell is created and nothing is drawn: `Forbidden`
    /// shows nothing and `on_shown` does not run; `Background` orders the window back without
    /// activation and runs `on_shown`. `before_start` does not run when nothing is drawn. Returns
    /// `Ok(())` in both suppressed cases (the morph completed logically). Events:
    /// `morph_suppressed` (`direction=from_anchor mode=.. source=..`), `morph_reveal_suppressed`,
    /// `morph_show_error`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_anchor<R: Runtime>(
        win: &WebviewWindow<R>,
        style: &Arc<ShellStyle>,
        timing: MorphTiming,
        ease: fn(f64) -> f64,
        anchor: Anchor,
        clearance_px: i32,
        source: RevealSource,
        sink: EventSink,
        before_start: impl FnOnce(),
        on_shown: impl FnOnce(&WebviewWindow<R>) + Send + 'static,
    ) -> Result<(), String> {
        let app = win.app_handle().clone();
        let mode = reveal_mode(&app, source);
        if mode != RevealMode::Foreground {
            sink(
                "morph_suppressed",
                &format!("direction=from_anchor mode={mode:?} source={source:?}"),
            );
            let applied = crate::hub_window::show_only(&app, win, source, "morph", sink);
            if applied == RevealMode::Background {
                on_shown(win);
            }
            return Ok(());
        }
        anchor.check_live()?;
        let clearance_px = clearance_px.clamp(0, MAX_CLEARANCE_PX);
        let label = win.label().to_owned();
        let hub_hwnd = win.hwnd().map_err(|err| err.to_string())?.0 as isize;
        let destination = window_frame(hub_hwnd)?.destination_above(anchor.frame, clearance_px);
        let shell = create(style, anchor.frame, anchor.hwnd)?;

        // Position the hidden, warm window before the hand-over.
        let _ = win.set_position(tauri::PhysicalPosition::new(
            destination.x(),
            destination.y(),
        ));
        let _ = win.set_size(tauri::PhysicalSize::new(
            destination.width() as u32,
            destination.height() as u32,
        ));
        before_start();
        let apply_style = Arc::clone(style);
        let completion_app = app.clone();
        run(
            app,
            anchor.frame,
            destination,
            timing,
            ease,
            Arc::new(move |frame| set_frame(&apply_style, shell, anchor.hwnd, frame, false)),
            move || {
                destroy(shell);
                if let Some(win) = completion_app.get_webview_window(&label) {
                    // The policy is asked again at the moment of the show: it may have changed
                    // since the morph began.
                    let applied = crate::hub_window::show_and_focus(
                        &completion_app,
                        &win,
                        source,
                        "morph",
                        "morph",
                        sink,
                    );
                    if applied != RevealMode::Forbidden {
                        on_shown(&win);
                    }
                }
            },
        );
        Ok(())
    }

    /// Shrink `win` into `anchor`. Asks the shell's guard for `source` first. Under `Foreground`:
    /// create the shell over the window's frame, hide the window, run `before_start`, animate to the
    /// anchor, then destroy the shell. Under any other mode no shell is created and nothing is
    /// drawn: the window is only hidden (hiding reveals nothing) and `before_start` does not run.
    /// Event: `morph_suppressed` (`direction=to_anchor ..`).
    pub fn to_anchor<R: Runtime>(
        win: &Window<R>,
        style: &Arc<ShellStyle>,
        timing: MorphTiming,
        ease: fn(f64) -> f64,
        anchor: Anchor,
        source: RevealSource,
        sink: EventSink,
        before_start: impl FnOnce(),
    ) -> Result<(), String> {
        let app = win.app_handle().clone();
        let mode = reveal_mode(&app, source);
        if mode != RevealMode::Foreground {
            sink(
                "morph_suppressed",
                &format!("direction=to_anchor mode={mode:?} source={source:?}"),
            );
            let _ = win.hide();
            return Ok(());
        }
        anchor.check_live()?;
        let hub_hwnd = win.hwnd().map_err(|err| err.to_string())?.0 as isize;
        let source_frame = window_frame(hub_hwnd)?;
        let shell = create(style, source_frame, anchor.hwnd)?;
        let _ = win.hide();
        before_start();
        let apply_style = Arc::clone(style);
        run(
            app,
            source_frame,
            anchor.frame,
            timing,
            ease,
            Arc::new(move |frame| set_frame(&apply_style, shell, anchor.hwnd, frame, false)),
            move || destroy(shell),
        );
        Ok(())
    }
}
