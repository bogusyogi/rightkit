//! Platform-neutral overlay state and placement policy.
//!
//! Native hosts are responsible for creating windows. This crate keeps the policy identical for
//! the web-rendered pill, the native pill, and the docked notch: finite geometry, work-area
//! clamping, delayed hover collapse, and the explicit native style contract. Callers own overlay
//! kinds and platform window levels.

use serde::{Deserialize, Serialize};

pub mod stack;

/// Windows native hosts must keep both the extended style and actual z-order in this band.
pub const WINDOWS_TOPMOST_REQUIRED: bool = true;
pub const HOVER_ENTER_TICKS: u32 = 1;
pub const HOVER_LEAVE_TICKS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PillMode {
    Idle,
    Starting,
    Recording,
    Processing,
    Status,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayEvent {
    Hide,
    Idle,
    Starting,
    Recording,
    Processing { cancellable: bool },
    Status { label: String },
    Error { label: String },
}

impl OverlayEvent {
    pub fn mode(&self) -> Option<PillMode> {
        match self {
            Self::Hide => None,
            Self::Idle => Some(PillMode::Idle),
            Self::Starting => Some(PillMode::Starting),
            Self::Recording => Some(PillMode::Recording),
            Self::Processing { .. } => Some(PillMode::Processing),
            Self::Status { .. } => Some(PillMode::Status),
            Self::Error { .. } => Some(PillMode::Error),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn finite(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f64::is_finite)
            && self.width >= 0.0
            && self.height >= 0.0
    }
    pub fn contains(self, x: f64, y: f64) -> bool {
        self.finite()
            && x.is_finite()
            && y.is_finite()
            && x >= self.x
            && x <= self.x + self.width
            && y >= self.y
            && y <= self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkArea {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// A watchdog calls this after foreground/z-order notifications. Reassert only when either the
/// style bit or observed band is lost; preserving focus is the caller's responsibility.
pub fn should_reassert_windows_topmost(style_topmost: bool, in_topmost_band: bool) -> bool {
    !style_topmost || !in_topmost_band
}

/// Values of the Windows extended window styles an overlay uses (`WS_EX_*`). They are plain
/// numbers here so [`OverlayStyle::ex_style`] is the same on every OS and its tests run
/// everywhere.
pub mod ex_style {
    pub const TOPMOST: u32 = 0x0000_0008;
    pub const TRANSPARENT: u32 = 0x0000_0020;
    pub const TOOLWINDOW: u32 = 0x0000_0080;
    pub const APPWINDOW: u32 = 0x0004_0000;
    pub const LAYERED: u32 = 0x0008_0000;
    pub const NOACTIVATE: u32 = 0x0800_0000;
}

/// How an overlay window behaves towards focus, the pointer and the z-order.
///
/// Every overlay is a layered tool window (per-pixel alpha, no taskbar button). The three
/// switches cover the kinds a desktop app builds: a passive panel, a click-through drawing
/// surface, and a panel that takes the keyboard while it is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayStyle {
    /// Stay above ordinary windows.
    pub topmost: bool,
    /// The window may become the foreground window (a panel the user types into). When false
    /// it never activates, so showing or clicking it leaves the user's app in front.
    pub takes_focus: bool,
    /// The whole window is invisible to the pointer: clicks and moves go to what is below it.
    /// A window that is not click-through still passes the pointer through fully transparent
    /// pixels, so give a hit zone a faint alpha to make it catch the pointer.
    pub click_through: bool,
}

impl OverlayStyle {
    /// A topmost panel that is seen and clicked but never takes focus (a notch, a hover card).
    pub const PASSIVE: Self = Self {
        topmost: true,
        takes_focus: false,
        click_through: false,
    };
    /// A topmost drawing surface the pointer passes through (a radial menu drawn over the app
    /// whose clicks are read some other way).
    pub const CLICK_THROUGH: Self = Self {
        topmost: true,
        takes_focus: false,
        click_through: true,
    };
    /// A topmost panel that takes the keyboard while it is open (a launcher).
    pub const KEYBOARD: Self = Self {
        topmost: true,
        takes_focus: true,
        click_through: false,
    };

    /// The extended style a window should have for this behaviour, given the one it has now.
    /// Bits this type does not own are kept.
    pub fn ex_style(self, current: u32) -> u32 {
        use ex_style::*;
        let owned = TOPMOST | TRANSPARENT | TOOLWINDOW | APPWINDOW | LAYERED | NOACTIVATE;
        let mut wanted = LAYERED | TOOLWINDOW;
        if self.topmost {
            wanted |= TOPMOST;
        }
        if !self.takes_focus {
            wanted |= NOACTIVATE;
        }
        if self.click_through {
            wanted |= TRANSPARENT;
        }
        (current & !owned) | wanted
    }

    /// Like [`OverlayStyle::ex_style`], without `WS_EX_LAYERED`. Used by
    /// [`windows::create_overlay_window`], so a new overlay is a plain topmost tool window that
    /// paints without `SetLayeredWindowAttributes`. Click-through needs a layered window, so the
    /// creator refuses that style rather than creating a window that ignores it.
    pub fn unlayered_ex_style(self, current: u32) -> u32 {
        self.ex_style(current) & !ex_style::LAYERED
    }
}

/// The position and size of an overlay window, in screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

/// What an overlay does while another program covers the whole monitor (a game, a video, a
/// presentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullScreenPolicy {
    /// Stay visible above the full-screen window.
    StayAbove,
    /// Hide until the full-screen window is gone.
    Hide,
}

impl FullScreenPolicy {
    /// Whether the overlay is shown, given whether a foreground window fills its monitor.
    pub fn shows(self, foreground_fills_monitor: bool) -> bool {
        match self {
            Self::StayAbove => true,
            Self::Hide => !foreground_fills_monitor,
        }
    }
}

/// True when `window` covers all of `monitor` (both in the same screen coordinates). A window
/// that is merely maximised does not: it stops at the work area.
pub fn fills_monitor(window: WorkArea, monitor: WorkArea) -> bool {
    [
        window.left,
        window.top,
        window.right,
        window.bottom,
        monitor.left,
        monitor.top,
        monitor.right,
        monitor.bottom,
    ]
    .into_iter()
    .all(f64::is_finite)
        && monitor.right > monitor.left
        && monitor.bottom > monitor.top
        && window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// `WS_CAPTION` (`WS_BORDER | WS_DLGFRAME`): a title bar. Held here so the style test runs on
/// every OS.
pub const WS_CAPTION_BITS: u32 = 0x00C0_0000;
/// `WS_THICKFRAME`: a sizing border.
pub const WS_THICKFRAME_BIT: u32 = 0x0004_0000;

/// True when a window style has neither a caption nor a sizing border (`WS_CAPTION`,
/// `WS_THICKFRAME` both absent). `WS_CAPTION` is two bits; any of them counts as a caption.
pub fn style_is_borderless(style: u32) -> bool {
    style & (WS_CAPTION_BITS | WS_THICKFRAME_BIT) == 0
}

/// True when `window` is exactly the monitor rectangle (all four edges equal, both finite, the
/// monitor non-empty). Stricter than [`fills_monitor`], which also accepts a window that
/// overhangs the monitor.
pub fn rect_equals_monitor(window: WorkArea, monitor: WorkArea) -> bool {
    [
        window.left,
        window.top,
        window.right,
        window.bottom,
        monitor.left,
        monitor.top,
        monitor.right,
        monitor.bottom,
    ]
    .into_iter()
    .all(f64::is_finite)
        && monitor.right > monitor.left
        && monitor.bottom > monitor.top
        && window.left == monitor.left
        && window.top == monitor.top
        && window.right == monitor.right
        && window.bottom == monitor.bottom
}

/// True when `pid` belongs to the app itself: `current_pid` or one of `own_pids` (for example a
/// sidecar or helper process that owns windows for the same app).
pub fn is_own_pid(pid: u32, current_pid: u32, own_pids: &[u32]) -> bool {
    pid == current_pid || own_pids.contains(&pid)
}

/// The full-screen verdict for one foreground window, from plain values: a window that is not
/// the app's own, has a borderless style and is exactly the monitor rectangle.
pub fn is_foreign_borderless_fullscreen(
    style: u32,
    window: WorkArea,
    monitor: WorkArea,
    own_pid: bool,
) -> bool {
    !own_pid && style_is_borderless(style) && rect_equals_monitor(window, monitor)
}

/// `WS_BORDER`: a one-pixel border. Alone it does not make a window framed (see
/// [`style_has_no_frame`]).
pub const WS_BORDER_BIT: u32 = 0x0080_0000;
/// `WS_DLGFRAME`: a dialog-style frame. Alone it does not make a window framed (see
/// [`style_has_no_frame`]).
pub const WS_DLGFRAME_BIT: u32 = 0x0040_0000;

/// True when a window style draws no frame, under the rule a foreground-app check uses: a window
/// is framed when it has the whole caption (`WS_BORDER` and `WS_DLGFRAME` together) or a sizing
/// border (`WS_THICKFRAME`). A lone `WS_BORDER` or a lone `WS_DLGFRAME` leaves it frameless.
/// Stricter than [`style_is_borderless`], which counts either bit as a frame.
pub fn style_has_no_frame(style: u32) -> bool {
    style & WS_CAPTION_BITS != WS_CAPTION_BITS && style & WS_THICKFRAME_BIT == 0
}

/// Whether a foreground window counts as full screen for an overlay's [`FullScreenPolicy`]: it
/// is not the app's own window, it has no frame ([`style_has_no_frame`]) and it covers the whole
/// monitor, overhang included ([`fills_monitor`]). A window larger than the monitor (a game or a
/// video that spills over the edges) therefore counts as full screen.
pub fn is_foreign_fullscreen_overhang(
    style: u32,
    window: WorkArea,
    monitor: WorkArea,
    own_pid: bool,
) -> bool {
    !own_pid && style_has_no_frame(style) && fills_monitor(window, monitor)
}

/// Whether two rectangles share any area. Touching edges do not count as overlap.
pub fn rects_overlap(a: WorkArea, b: WorkArea) -> bool {
    a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top
}

/// One window stacked above an overlay, reduced to the facts the covering check needs. A host
/// fills these in from the z-order walk, nearest window above first.
#[derive(Clone, Debug, PartialEq)]
pub struct CoveringCandidate {
    /// The window class name (`GetClassNameW`).
    pub class_name: String,
    /// The window is visible.
    pub visible: bool,
    /// The window belongs to the app itself (its process or one of its own pids).
    pub own_process: bool,
    /// The window is cloaked by the compositor (`DWMWA_CLOAKED`), so it is not on screen.
    pub cloaked: bool,
    /// The window's bounds in screen pixels.
    pub rect: WorkArea,
}

/// How many windows above an overlay a covering check reads. A short walk: the topmost band is
/// small, and a longer walk costs time on every check.
pub const MAX_COVERING_WALK: usize = 64;

/// The covering verdict for an overlay, from plain values. `"not_topmost"` when the overlay lost
/// its topmost flag. Otherwise the class name of the first candidate, nearest above first, that
/// is visible, not the app's own, not cloaked and overlaps the overlay. `None` when nothing
/// covers it. Only the first [`MAX_COVERING_WALK`] candidates are read.
pub fn covering_window(
    topmost: bool,
    overlay: WorkArea,
    above: &[CoveringCandidate],
) -> Option<String> {
    if !topmost {
        return Some("not_topmost".to_string());
    }
    above.iter().take(MAX_COVERING_WALK).find_map(|c| {
        let counts = c.visible && !c.own_process && !c.cloaked && rects_overlap(c.rect, overlay);
        counts.then(|| c.class_name.clone())
    })
}

/// One monitor, for placing an overlay on it: its full bounds, its work area (bounds without
/// the taskbar) and its DPI. Coordinates are physical screen pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorPlacement {
    pub bounds: WorkArea,
    pub work: WorkArea,
    /// Effective DPI; 96 is 100% scaling.
    pub dpi: u32,
}

impl MonitorPlacement {
    /// The factor from design units at 100% scaling to this monitor's pixels (1.5 at 144 DPI).
    pub fn scale(&self) -> f64 {
        f64::from(self.dpi.max(1)) / 96.0
    }
}

/// Windows-native overlay operations, ported from a mature native pill host. Hosts pass the raw
/// `HWND` value as an `isize` so the crate stays independent of a Tauri/WebView window type while
/// still owning the locked no-activation and TOPMOST behavior. Hosts must install the WNDPROC
/// guard on the thread that owns the HWND, before that window receives UI messages.
#[cfg(windows)]
pub mod windows {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO,
        MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, EnumWindows, GetDesktopWindow, GetForegroundWindow,
        GetShellWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, SetWindowLongPtrW, SetWindowPos, GWLP_WNDPROC, GWL_EXSTYLE, GWL_STYLE,
        HWND_TOPMOST, MA_NOACTIVATE, SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, WM_MOUSEACTIVATE, WM_NCDESTROY, WNDPROC,
        WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, WS_POPUP};

    use crate::{ex_style, MonitorPlacement, OverlayRect, OverlayStyle, WorkArea};

    static ORIGINAL_WNDPROCS: OnceLock<Mutex<HashMap<isize, isize>>> = OnceLock::new();

    fn original_wndprocs() -> &'static Mutex<HashMap<isize, isize>> {
        ORIGINAL_WNDPROCS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Keeps interactive overlay child windows from activating their process on mouse-down.
    ///
    /// # Safety
    /// Install only as the window procedure of a window whose original procedure was recorded in
    /// `ORIGINAL_WNDPROCS`; Windows calls it with that window's handle and message arguments.
    pub unsafe extern "system" fn noactivate_wndproc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_MOUSEACTIVATE {
            return MA_NOACTIVATE as LRESULT;
        }

        let original = {
            let mut wndprocs = original_wndprocs()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let original = wndprocs.get(&(hwnd as isize)).copied();
            if message == WM_NCDESTROY {
                wndprocs.remove(&(hwnd as isize));
            }
            original
        };
        match original {
            Some(original) => {
                let original: WNDPROC = std::mem::transmute(original);
                CallWindowProcW(original, hwnd, message, wparam, lparam)
            }
            None => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    /// Installs the donor's no-activation WNDPROC guard once for a host HWND.
    ///
    /// # Safety
    ///
    /// Call this on the thread that owns `hwnd`, before that window receives UI messages, and keep
    /// the HWND alive until it sends `WM_NCDESTROY`.
    pub unsafe fn install_noactivate_mouse_guard(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let mut wndprocs = original_wndprocs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if wndprocs.contains_key(&hwnd) {
            return false;
        }
        let hwnd = hwnd as HWND;
        let original =
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, noactivate_wndproc as *const () as isize);
        if original == 0 {
            return false;
        }
        wndprocs.insert(hwnd as isize, original);
        true
    }

    /// Applies `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, clears `WS_EX_APPWINDOW`, and refreshes
    /// frame styles without changing position or z-order.
    pub fn make_noactivate(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        unsafe {
            let hwnd = hwnd as HWND;
            let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let wanted = (current | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize)
                & !(WS_EX_APPWINDOW as isize);
            let style_changed = wanted != current;
            if style_changed {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
                let _ = SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            let mouse_guard_changed = install_noactivate_mouse_guard(hwnd as isize);
            style_changed || mouse_guard_changed
        }
    }

    /// Reads actual z-order, including the style-bit-loss and style-bit-survives-but-sunk cases.
    pub fn is_out_of_topmost_band(hwnd: isize) -> bool {
        if hwnd == 0 {
            return true;
        }
        unsafe {
            let hwnd = hwnd as HWND;
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if style as u32 & WS_EX_TOPMOST == 0 {
                return true;
            }

            struct State {
                target: isize,
                found: bool,
                band_broken: bool,
            }

            unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
                let state = &mut *(lparam as *mut State);
                if IsWindowVisible(hwnd) == 0 {
                    return 1;
                }
                if hwnd as isize == state.target {
                    state.found = true;
                    return 0;
                }
                if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST == 0 {
                    state.band_broken = true;
                    return 0;
                }
                1
            }

            let mut state = State {
                target: hwnd as isize,
                found: false,
                band_broken: false,
            };
            let _ = EnumWindows(Some(callback), &mut state as *mut _ as LPARAM);
            !state.found || state.band_broken
        }
    }

    /// Reasserts TOPMOST z-order without moving, resizing, or activating the overlay.
    pub fn reassert_topmost(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            ) != 0
        }
    }

    /// Reasserts only after the observed style or z-band has drifted.
    pub fn reassert_if_needed(hwnd: isize) -> bool {
        if is_out_of_topmost_band(hwnd) {
            reassert_topmost(hwnd)
        } else {
            false
        }
    }

    /// Write `wanted` as the extended style of the window and make Windows re-read it, without
    /// moving, resizing, reordering or activating the window. Returns whether it changed.
    fn write_ex_style(hwnd: HWND, current: isize, wanted: u32) -> bool {
        // The owned bits all sit in the low 32 bits; keep whatever is above them.
        let high = (current as usize) & !(u32::MAX as usize);
        let wanted = (high | wanted as usize) as isize;
        if wanted == current {
            return false;
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
            let _ = SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        true
    }

    /// Give `hwnd` the extended style of `style` (see [`OverlayStyle::ex_style`]). Returns
    /// whether the style changed. This sets the style bits only: a window that must not
    /// activate on a mouse click also needs [`make_noactivate`], and the z-order is set by
    /// [`show_noactivate`] or [`reassert_topmost`].
    pub fn apply_style(hwnd: isize, style: OverlayStyle) -> bool {
        if hwnd == 0 {
            return false;
        }
        let hwnd = hwnd as HWND;
        let current = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        write_ex_style(hwnd, current, style.ex_style(current as u32))
    }

    /// Creates a hidden, topmost, non-activating tool window of `style` at `rect` **without**
    /// `WS_EX_LAYERED` (`WS_POPUP`, no taskbar button). Show it with [`show_noactivate`]. The
    /// caller registers `class_name` with `hinstance` first. Returns the handle, or `0` when it
    /// cannot be made: an empty name or one with an interior NUL, `style.click_through` (that
    /// needs a layered window), or a failed `CreateWindowExW`.
    pub fn create_overlay_window(
        hinstance: isize,
        class_name: &str,
        title: &str,
        rect: OverlayRect,
        style: OverlayStyle,
    ) -> isize {
        if style.click_through || class_name.is_empty() {
            return 0;
        }
        let (Some(class_name), Some(title)) = (wide(class_name), wide(title)) else {
            return 0;
        };
        let ex = style.unlayered_ex_style(0);
        unsafe {
            CreateWindowExW(
                ex,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance as HINSTANCE,
                std::ptr::null(),
            ) as isize
        }
    }

    /// NUL-terminated UTF-16 copy of `text`, or `None` when it contains a NUL.
    fn wide(text: &str) -> Option<Vec<u16>> {
        if text.contains('\0') {
            return None;
        }
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.push(0);
        Some(units)
    }

    /// Turn click-through on or off at run time (`WS_EX_TRANSPARENT`; the window must be
    /// layered). Returns whether the style changed.
    pub fn set_click_through(hwnd: isize, on: bool) -> bool {
        if hwnd == 0 {
            return false;
        }
        let hwnd = hwnd as HWND;
        let current = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        let bits = current as u32;
        let wanted = if on {
            bits | ex_style::TRANSPARENT | ex_style::LAYERED
        } else {
            bits & !ex_style::TRANSPARENT
        };
        write_ex_style(hwnd, current, wanted)
    }

    /// Show the window in the topmost band without activating it, moving it or resizing it.
    pub fn show_noactivate(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            ) != 0
        }
    }

    /// Hide the window without changing the foreground window or the z-order.
    pub fn hide(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_HIDEWINDOW,
            ) != 0
        }
    }

    fn area(rect: RECT) -> WorkArea {
        WorkArea {
            left: f64::from(rect.left),
            top: f64::from(rect.top),
            right: f64::from(rect.right),
            bottom: f64::from(rect.bottom),
        }
    }

    fn placement(monitor: HMONITOR) -> Option<MonitorPlacement> {
        if monitor.is_null() {
            return None;
        }
        let empty = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: empty,
            rcWork: empty,
            dwFlags: 0,
        };
        // SAFETY: `info` is a valid MONITORINFO with its size set, as the call requires.
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        // SAFETY: both out-parameters are valid; a failure leaves the 96 DPI default.
        let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        Some(MonitorPlacement {
            bounds: area(info.rcMonitor),
            work: area(info.rcWork),
            dpi: if dpi_x == 0 { 96 } else { dpi_x },
        })
    }

    /// The monitor under (or nearest to) a point in physical screen pixels, for example the
    /// pointer position. The DPI is the per-monitor value only in a process that is
    /// per-monitor DPI aware; otherwise Windows reports the system DPI.
    pub fn monitor_at(x: i32, y: i32) -> Option<MonitorPlacement> {
        // SAFETY: MonitorFromPoint takes the point by value and has no other preconditions.
        placement(unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) })
    }

    /// True when the foreground window covers the whole of its monitor (a full-screen game,
    /// video or presentation). The desktop and the shell do not count. Feed the result to
    /// [`FullScreenPolicy::shows`](crate::FullScreenPolicy::shows).
    pub fn foreground_fills_monitor() -> bool {
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground.is_null()
                || foreground == GetDesktopWindow()
                || foreground == GetShellWindow()
            {
                return false;
            }
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetWindowRect(foreground, &mut rect) == 0 {
                return false;
            }
            let Some(monitor) = placement(MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST))
            else {
                return false;
            };
            crate::fills_monitor(area(rect), monitor.bounds)
        }
    }

    /// True when the foreground window is a borderless full-screen window of another program:
    /// no `WS_CAPTION` and no `WS_THICKFRAME`, its rectangle exactly the rectangle of its
    /// monitor, visible and not minimised. Windows of this process, and of every pid in
    /// `own_pids` (helper or sidecar processes of the same app), never count, so the app's own
    /// full-screen window does not hide its overlay. The desktop and the shell do not count.
    /// Unlike [`foreground_fills_monitor`] this tests the style, so a maximised or overhanging
    /// window does not count.
    pub fn foreground_is_borderless_fullscreen(own_pids: &[u32]) -> bool {
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground.is_null()
                || foreground == GetDesktopWindow()
                || foreground == GetShellWindow()
                || IsWindowVisible(foreground) == 0
                || IsIconic(foreground) != 0
            {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(foreground, &mut pid);
            let own = crate::is_own_pid(pid, std::process::id(), own_pids);
            let style = GetWindowLongPtrW(foreground, GWL_STYLE) as u32;
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetWindowRect(foreground, &mut rect) == 0 {
                return false;
            }
            let Some(monitor) = placement(MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST))
            else {
                return false;
            };
            crate::is_foreign_borderless_fullscreen(style, area(rect), monitor.bounds, own)
        }
    }
}

#[cfg(not(windows))]
pub mod windows {
    /// Windows-native operations are unavailable on non-Windows hosts.
    pub fn is_out_of_topmost_band(_hwnd: isize) -> bool {
        false
    }
    pub fn make_noactivate(_hwnd: isize) -> bool {
        false
    }
    pub fn reassert_topmost(_hwnd: isize) -> bool {
        false
    }
    pub fn reassert_if_needed(_hwnd: isize) -> bool {
        false
    }
    pub fn apply_style(_hwnd: isize, _style: crate::OverlayStyle) -> bool {
        false
    }
    pub fn set_click_through(_hwnd: isize, _on: bool) -> bool {
        false
    }
    pub fn create_overlay_window(
        _hinstance: isize,
        _class_name: &str,
        _title: &str,
        _rect: crate::OverlayRect,
        _style: crate::OverlayStyle,
    ) -> isize {
        0
    }
    pub fn show_noactivate(_hwnd: isize) -> bool {
        false
    }
    pub fn hide(_hwnd: isize) -> bool {
        false
    }
    pub fn monitor_at(_x: i32, _y: i32) -> Option<crate::MonitorPlacement> {
        None
    }
    pub fn foreground_fills_monitor() -> bool {
        false
    }
    pub fn foreground_is_borderless_fullscreen(_own_pids: &[u32]) -> bool {
        false
    }
}

impl WorkArea {
    pub fn clamp(self, frame: Rect) -> Rect {
        if !frame.finite() {
            return frame;
        }
        let max_x = (self.right - frame.width).max(self.left);
        let max_y = (self.bottom - frame.height).max(self.top);
        Rect {
            x: frame.x.clamp(self.left, max_x),
            y: frame.y.clamp(self.top, max_y),
            ..frame
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoverGate {
    inside_ticks: u32,
    outside_ticks: u32,
    hovering: bool,
}

impl HoverGate {
    pub fn update(&mut self, inside: bool) -> bool {
        if inside {
            self.inside_ticks = self.inside_ticks.saturating_add(1);
            self.outside_ticks = 0;
        } else {
            self.outside_ticks = self.outside_ticks.saturating_add(1);
            self.inside_ticks = 0;
        }
        self.hovering = if self.hovering {
            self.outside_ticks < HOVER_LEAVE_TICKS
        } else {
            inside && self.inside_ticks >= HOVER_ENTER_TICKS
        };
        self.hovering
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn is_hovering(&self) -> bool {
        self.hovering
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlayModel<Kind> {
    /// Caller-owned identity. Serialization follows the caller's type; it selects no native policy.
    pub kind: Kind,
    pub visible: bool,
    pub mode: Option<PillMode>,
    pub frame: Rect,
    pub hitbox: Option<Rect>,
}

impl<Kind> OverlayModel<Kind> {
    pub fn new(kind: Kind, frame: Rect) -> Self {
        Self {
            kind,
            visible: false,
            mode: None,
            frame,
            hitbox: None,
        }
    }
    pub fn apply(&mut self, event: OverlayEvent) {
        self.mode = event.mode();
        self.visible = self.mode.is_some();
        if self.hitbox.is_none() {
            self.hitbox = Some(self.frame);
        }
    }
    pub fn place(&mut self, work: WorkArea) {
        self.frame = work.clamp(self.frame);
    }
    pub fn hit_test(&self, x: f64, y: f64) -> bool {
        self.visible && self.hitbox.unwrap_or(self.frame).contains(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::ex_style::*;
    use super::*;

    #[test]
    fn caller_kinds_share_geometry_and_visibility_without_product_policy() {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum AppKind {
            Compact,
            Expanded,
        }
        let frame = Rect {
            x: -20.0,
            y: 10.0,
            width: 80.0,
            height: 40.0,
        };
        let work = WorkArea {
            left: 0.0,
            top: 0.0,
            right: 100.0,
            bottom: 100.0,
        };
        let mut compact = OverlayModel::new(AppKind::Compact, frame);
        let mut expanded = OverlayModel::new(AppKind::Expanded, frame);
        let mut custom = OverlayModel::new("caller-defined", frame);
        for model in [&mut compact, &mut expanded] {
            model.place(work);
            model.apply(OverlayEvent::Idle);
            assert_eq!(model.frame.x, 0.0);
            assert!(model.hit_test(10.0, 20.0));
            model.apply(OverlayEvent::Hide);
            assert!(!model.hit_test(10.0, 20.0));
        }
        custom.apply(OverlayEvent::Status {
            label: "ready".into(),
        });
        assert_eq!(custom.kind, "caller-defined");
        assert_eq!(compact.kind, AppKind::Compact);
        assert_eq!(expanded.kind, AppKind::Expanded);
        assert_eq!(custom.mode, Some(PillMode::Status));
    }

    fn area(left: f64, top: f64, right: f64, bottom: f64) -> WorkArea {
        WorkArea {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn the_three_overlay_kinds_map_to_their_extended_styles() {
        assert_eq!(
            OverlayStyle::PASSIVE.ex_style(0),
            LAYERED | NOACTIVATE | TOOLWINDOW | TOPMOST
        );
        assert_eq!(
            OverlayStyle::CLICK_THROUGH.ex_style(0),
            LAYERED | NOACTIVATE | TOOLWINDOW | TOPMOST | TRANSPARENT
        );
        // A panel that takes the keyboard is the same without NOACTIVATE.
        assert_eq!(
            OverlayStyle::KEYBOARD.ex_style(0),
            LAYERED | TOOLWINDOW | TOPMOST
        );
    }

    #[test]
    fn an_unlayered_overlay_style_has_no_layered_bit_and_keeps_the_rest() {
        assert_eq!(
            OverlayStyle::PASSIVE.unlayered_ex_style(0),
            TOOLWINDOW | TOPMOST | NOACTIVATE
        );
        assert_eq!(
            OverlayStyle::KEYBOARD.unlayered_ex_style(0),
            TOOLWINDOW | TOPMOST
        );
        // A current LAYERED bit is cleared, an unowned bit survives.
        const WS_EX_COMPOSITED: u32 = 0x0200_0000;
        assert_eq!(
            OverlayStyle::PASSIVE.unlayered_ex_style(LAYERED | WS_EX_COMPOSITED),
            TOOLWINDOW | TOPMOST | NOACTIVATE | WS_EX_COMPOSITED
        );
        // The layered path is unchanged.
        assert_eq!(OverlayStyle::PASSIVE.ex_style(0) & LAYERED, LAYERED);
    }

    #[test]
    fn applying_a_style_clears_what_it_owns_and_keeps_the_rest() {
        const WS_EX_COMPOSITED: u32 = 0x0200_0000;
        let before = APPWINDOW | TRANSPARENT | NOACTIVATE | WS_EX_COMPOSITED;
        let after = OverlayStyle::KEYBOARD.ex_style(before);
        assert_eq!(after, LAYERED | TOOLWINDOW | TOPMOST | WS_EX_COMPOSITED);
        let not_topmost = OverlayStyle {
            topmost: false,
            ..OverlayStyle::PASSIVE
        };
        assert_eq!(
            not_topmost.ex_style(TOPMOST),
            LAYERED | TOOLWINDOW | NOACTIVATE
        );
    }

    #[test]
    fn full_screen_policy_hides_only_when_asked_and_only_under_a_full_screen_window() {
        assert!(FullScreenPolicy::StayAbove.shows(true));
        assert!(FullScreenPolicy::StayAbove.shows(false));
        assert!(!FullScreenPolicy::Hide.shows(true));
        assert!(FullScreenPolicy::Hide.shows(false));
    }

    #[test]
    fn a_window_fills_a_monitor_only_when_it_covers_all_of_it() {
        let monitor = area(1920.0, 0.0, 3840.0, 1080.0);
        assert!(fills_monitor(monitor, monitor));
        // Borderless windows often overhang by a few pixels.
        assert!(fills_monitor(area(1912.0, -8.0, 3848.0, 1088.0), monitor));
        // Maximised: stops above the taskbar.
        assert!(!fills_monitor(area(1920.0, 0.0, 3840.0, 1040.0), monitor));
        // On another monitor.
        assert!(!fills_monitor(area(0.0, 0.0, 1920.0, 1080.0), monitor));
        // Degenerate or non-finite input never counts.
        assert!(!fills_monitor(monitor, area(0.0, 0.0, 0.0, 0.0)));
        assert!(!fills_monitor(area(f64::NAN, 0.0, 3840.0, 1080.0), monitor));
    }

    #[test]
    fn borderless_style_has_no_caption_and_no_thick_frame() {
        // WS_POPUP | WS_VISIBLE
        assert!(style_is_borderless(0x8000_0000 | 0x1000_0000));
        // WS_OVERLAPPEDWINDOW
        assert!(!style_is_borderless(0x00CF_0000));
        // Caption only, thick frame only, and a lone WS_BORDER (half of WS_CAPTION).
        assert!(!style_is_borderless(0x00C0_0000));
        assert!(!style_is_borderless(0x0004_0000));
        assert!(!style_is_borderless(0x0080_0000));
        assert!(!style_is_borderless(0x0040_0000));
    }

    #[test]
    fn the_rect_must_equal_the_monitor_rect() {
        let monitor = area(1920.0, 0.0, 3840.0, 1080.0);
        assert!(rect_equals_monitor(monitor, monitor));
        assert!(!rect_equals_monitor(
            area(1912.0, -8.0, 3848.0, 1088.0),
            monitor
        ));
        assert!(!rect_equals_monitor(
            area(1920.0, 0.0, 3840.0, 1040.0),
            monitor
        ));
        assert!(!rect_equals_monitor(monitor, area(0.0, 0.0, 0.0, 0.0)));
        assert!(!rect_equals_monitor(
            area(f64::NAN, 0.0, 3840.0, 1080.0),
            monitor
        ));
    }

    #[test]
    fn own_pids_are_excluded() {
        assert!(is_own_pid(10, 10, &[]));
        assert!(is_own_pid(22, 10, &[21, 22]));
        assert!(!is_own_pid(23, 10, &[21, 22]));
        assert!(!is_own_pid(0, 10, &[]));
    }

    #[test]
    fn foreign_borderless_fullscreen_verdict() {
        let monitor = area(0.0, 0.0, 1920.0, 1080.0);
        let popup = 0x9000_0000;
        assert!(is_foreign_borderless_fullscreen(
            popup, monitor, monitor, false
        ));
        // The app's own window never counts.
        assert!(!is_foreign_borderless_fullscreen(
            popup, monitor, monitor, true
        ));
        // A bordered window the size of the monitor (maximised on a small display) does not.
        assert!(!is_foreign_borderless_fullscreen(
            0x00CF_0000,
            monitor,
            monitor,
            false
        ));
        // A borderless window that is not the monitor rect does not.
        assert!(!is_foreign_borderless_fullscreen(
            popup,
            area(0.0, 0.0, 1920.0, 1040.0),
            monitor,
            false
        ));
    }

    #[test]
    fn monitor_scale_follows_the_dpi() {
        let placement = |dpi| MonitorPlacement {
            bounds: area(0.0, 0.0, 1920.0, 1080.0),
            work: area(0.0, 0.0, 1920.0, 1040.0),
            dpi,
        };
        assert_eq!(placement(96).scale(), 1.0);
        assert_eq!(placement(144).scale(), 1.5);
        assert!(placement(0).scale() > 0.0);
    }
}

#[cfg(test)]
mod frame_and_cover_tests {
    use super::*;

    const MONITOR: WorkArea = WorkArea {
        left: 0.0,
        top: 0.0,
        right: 1920.0,
        bottom: 1080.0,
    };

    fn rect(left: f64, top: f64, right: f64, bottom: f64) -> WorkArea {
        WorkArea {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn a_lone_border_or_dialog_frame_bit_is_frameless() {
        assert!(style_has_no_frame(0));
        assert!(style_has_no_frame(WS_BORDER_BIT));
        assert!(style_has_no_frame(WS_DLGFRAME_BIT));
        assert!(!style_has_no_frame(WS_CAPTION_BITS));
        assert!(!style_has_no_frame(WS_THICKFRAME_BIT));
        assert!(!style_has_no_frame(WS_BORDER_BIT | WS_THICKFRAME_BIT));
    }

    #[test]
    fn a_frameless_window_larger_than_the_monitor_is_full_screen() {
        let overhang = rect(-40.0, -20.0, 1960.0, 1100.0);
        assert!(is_foreign_fullscreen_overhang(
            WS_BORDER_BIT,
            overhang,
            MONITOR,
            false
        ));
        assert!(!is_foreign_fullscreen_overhang(
            WS_CAPTION_BITS,
            overhang,
            MONITOR,
            false
        ));
        assert!(!is_foreign_fullscreen_overhang(0, overhang, MONITOR, true));
        assert!(!is_foreign_fullscreen_overhang(
            0,
            rect(0.0, 0.0, 960.0, 540.0),
            MONITOR,
            false
        ));
    }

    #[test]
    fn touching_edges_do_not_overlap() {
        let a = rect(0.0, 0.0, 100.0, 100.0);
        assert!(rects_overlap(a, rect(50.0, 50.0, 150.0, 150.0)));
        assert!(!rects_overlap(a, rect(100.0, 0.0, 200.0, 100.0)));
    }

    fn above(
        class: &str,
        visible: bool,
        own: bool,
        cloaked: bool,
        r: WorkArea,
    ) -> CoveringCandidate {
        CoveringCandidate {
            class_name: class.to_string(),
            visible,
            own_process: own,
            cloaked,
            rect: r,
        }
    }

    #[test]
    fn covering_verdict_applies_the_filters_in_z_order() {
        let overlay = rect(100.0, 100.0, 300.0, 300.0);
        let far = rect(900.0, 900.0, 1000.0, 1000.0);
        let near = rect(150.0, 150.0, 350.0, 350.0);
        let list = [
            above("Hidden", false, false, false, near),
            above("Own", true, true, false, near),
            above("Cloaked", true, false, true, near),
            above("Far", true, false, false, far),
            above("Cover", true, false, false, near),
            above("Later", true, false, false, near),
        ];
        assert_eq!(
            covering_window(true, overlay, &list),
            Some("Cover".to_string())
        );
        assert_eq!(covering_window(true, overlay, &list[..4]), None);
    }

    #[test]
    fn lost_topmost_is_reported_before_any_window() {
        assert_eq!(
            covering_window(false, rect(0.0, 0.0, 1.0, 1.0), &[]),
            Some("not_topmost".to_string())
        );
    }
}
