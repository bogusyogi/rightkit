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
    /// The window is excluded from screen capture (`SetWindowDisplayAffinity` with
    /// `WDA_EXCLUDEFROMCAPTURE`), so screenshots and recordings do not show it. `false` leaves the
    /// window's current affinity alone; clear an exclusion with
    /// [`windows::set_capture_excluded`]`(hwnd, false)`.
    pub exclude_from_capture: bool,
}

impl OverlayStyle {
    /// A topmost panel that is seen and clicked but never takes focus (a notch, a hover card).
    pub const PASSIVE: Self = Self {
        topmost: true,
        takes_focus: false,
        click_through: false,
        exclude_from_capture: false,
    };
    /// A topmost drawing surface the pointer passes through (a radial menu drawn over the app
    /// whose clicks are read some other way).
    pub const CLICK_THROUGH: Self = Self {
        topmost: true,
        takes_focus: false,
        click_through: true,
        exclude_from_capture: false,
    };
    /// A topmost panel that takes the keyboard while it is open (a launcher).
    pub const KEYBOARD: Self = Self {
        topmost: true,
        takes_focus: true,
        click_through: false,
        exclude_from_capture: false,
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

/// Window classes of the shell's own overlays: the Alt-Tab and Task View hosts (borderless,
/// monitor-sized, and not a program's full-screen window) and the desktop and taskbar windows.
/// [`FullScreenProbe::default`] ignores them.
pub const SHELL_OVERLAY_CLASSES: &[&str] = &[
    "MultitaskingViewFrame",
    "XamlExplorerHostIslandWindow",
    "ForegroundStaging",
    "TaskListThumbnailWnd",
    "TaskListOverlayWnd",
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// What a full-screen check on the foreground window reads. Hosts pass [`FullScreenProbe::default`]
/// unless the app has its own helper processes (`own_pids`) or its own tolerance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FullScreenProbe {
    /// Helper or sidecar processes of this app. A foreground window owned by one of them, or by
    /// this process, never counts.
    pub own_pids: Vec<u32>,
    /// Window class names that never count (compared without ASCII case).
    pub ignore_classes: Vec<String>,
    /// Also count the foreground window when `SHQueryUserNotificationState` reports an exclusive
    /// Direct3D full-screen, a presentation or a Store app, even if its frame or bounds are not
    /// exactly the monitor.
    pub use_shell_notification_state: bool,
    /// How far, in pixels, an edge of the window may fall short of the monitor edge and still
    /// count as covering it.
    pub edge_tolerance_px: i32,
}

impl Default for FullScreenProbe {
    fn default() -> Self {
        Self {
            own_pids: Vec::new(),
            ignore_classes: SHELL_OVERLAY_CLASSES
                .iter()
                .map(|class| (*class).to_string())
                .collect(),
            use_shell_notification_state: true,
            edge_tolerance_px: 2,
        }
    }
}

/// True when `window` covers `monitor` on every edge, each edge allowed to fall short by
/// `tolerance_px`. A window that overhangs the monitor covers it. Both rectangles must be finite
/// and the monitor non-empty; a negative tolerance counts as zero.
pub fn covers_monitor_within(window: WorkArea, monitor: WorkArea, tolerance_px: i32) -> bool {
    let tolerance = f64::from(tolerance_px.max(0));
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
        && window.left <= monitor.left + tolerance
        && window.top <= monitor.top + tolerance
        && window.right >= monitor.right - tolerance
        && window.bottom >= monitor.bottom - tolerance
}

/// Why a native overlay or listener window was not created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCreateError {
    /// The class name is empty or holds a NUL.
    InvalidClassName,
    /// `CreateWindowExW` failed.
    CreateFailed,
    /// Native window creation is Windows-only; this target has none.
    Unavailable,
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
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows_sys::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_APP, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::WS_POPUP;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, EnumWindows,
        GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowLongPtrW,
        GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, GWLP_WNDPROC, GWL_EXSTYLE,
        GWL_STYLE, HWND_MESSAGE, HWND_TOPMOST, MA_NOACTIVATE, SWP_FRAMECHANGED, SWP_HIDEWINDOW,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
        WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_MOUSEACTIVATE, WM_NCDESTROY, WNDPROC, WS_EX_APPWINDOW,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };

    use crate::{
        ex_style, FullScreenProbe, MonitorPlacement, OverlayRect, OverlayStyle, WindowCreateError,
        WorkArea,
    };

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
    /// [`show_noactivate`] or [`reassert_topmost`]. `style.exclude_from_capture` is applied
    /// through [`set_capture_excluded`] when set; a `false` flag leaves the affinity alone.
    pub fn apply_style(hwnd: isize, style: OverlayStyle) -> bool {
        if hwnd == 0 {
            return false;
        }
        let hwnd_native = hwnd as HWND;
        let current = unsafe { GetWindowLongPtrW(hwnd_native, GWL_EXSTYLE) };
        let changed = write_ex_style(hwnd_native, current, style.ex_style(current as u32));
        if style.exclude_from_capture {
            set_capture_excluded(hwnd, true);
        }
        changed
    }

    /// Creates a hidden, topmost, non-activating tool window of `style` at `rect` **without**
    /// `WS_EX_LAYERED` (`WS_POPUP`, no taskbar button). Show it with [`show_noactivate`]. The
    /// caller registers `class_name` with `hinstance` first. Returns the handle, or `0` when it
    /// cannot be made: an empty name or one with an interior NUL, `style.click_through` (click-through
    /// needs a layered window: use [`create_layered_overlay_window`]), a failed `CreateWindowExW`,
    /// or a failed capture exclusion when `style.exclude_from_capture` is set (the window is
    /// destroyed).
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
        create_popup(
            hinstance,
            class_name,
            title,
            rect,
            style.unlayered_ex_style(0),
            style.exclude_from_capture,
        )
    }

    /// The `WS_POPUP` creation shared by [`create_overlay_window`] and
    /// [`create_layered_overlay_window`]. Returns `0` when the window cannot be made or cannot be
    /// excluded from capture as asked.
    fn create_popup(
        hinstance: isize,
        class_name: &str,
        title: &str,
        rect: OverlayRect,
        ex: u32,
        exclude_from_capture: bool,
    ) -> isize {
        let (Some(class_name), Some(title)) = (wide(class_name), wide(title)) else {
            return 0;
        };
        let hwnd = unsafe {
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
            )
        };
        if hwnd.is_null() {
            return 0;
        }
        let raw = hwnd as isize;
        if exclude_from_capture && !set_capture_excluded(raw, true) {
            unsafe {
                DestroyWindow(hwnd);
            }
            return 0;
        }
        raw
    }

    /// Creates a hidden, topmost, non-activating tool window of `style` at `rect` **with**
    /// `WS_EX_LAYERED`. Use it for a window the caller paints with per-pixel alpha
    /// (`UpdateLayeredWindow` with `ULW_ALPHA`) before it is shown: a layered window that has no
    /// pixels does not paint, which is why [`create_overlay_window`] leaves the bit out.
    ///
    /// `style.click_through` is created only when `pixel_alpha_empty_hit_regions` is true. That
    /// flag is the caller's assertion of both facts: the window is per-pixel-alpha layered and it
    /// has no hit regions. Then `WS_EX_TRANSPARENT` passes every pointer event through the
    /// window. A layered window with hit regions must instead clip its shape to them (see
    /// [`set_click_through`] for the run-time switch), and a click-through layered window with
    /// hit regions is refused, returning `0`, because `WS_EX_TRANSPARENT` would also pass the
    /// clicks meant for those regions (the `#361` case).
    pub fn create_layered_overlay_window(
        hinstance: isize,
        class_name: &str,
        title: &str,
        rect: OverlayRect,
        style: OverlayStyle,
        pixel_alpha_empty_hit_regions: bool,
    ) -> isize {
        if class_name.is_empty() || (style.click_through && !pixel_alpha_empty_hit_regions) {
            return 0;
        }
        create_popup(
            hinstance,
            class_name,
            title,
            rect,
            style.ex_style(0),
            style.exclude_from_capture,
        )
    }

    /// Creates a message-only window (parent `HWND_MESSAGE`) of class `class_name`, for a listener
    /// such as a clipboard-format listener. It is never shown and has no z-order or geometry. The
    /// caller registers `class_name`, with its window procedure, on this process's module first,
    /// and destroys the window with `DestroyWindow`.
    pub fn create_message_only_window(class_name: &str) -> Result<isize, WindowCreateError> {
        if class_name.is_empty() {
            return Err(WindowCreateError::InvalidClassName);
        }
        let Some(class_name) = wide(class_name) else {
            return Err(WindowCreateError::InvalidClassName);
        };
        let title = [0u16];
        // SAFETY: a null name returns the handle of the calling process's executable module.
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        // SAFETY: every pointer is valid for the call: the class and title are NUL-terminated.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                hinstance as HINSTANCE,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            Err(WindowCreateError::CreateFailed)
        } else {
            Ok(hwnd as isize)
        }
    }

    /// Excludes `hwnd` from screen capture (`WDA_EXCLUDEFROMCAPTURE`) or restores normal capture
    /// (`WDA_NONE`). Returns whether Windows accepted the affinity. Windows applies it only to a
    /// window of the calling process.
    pub fn set_capture_excluded(hwnd: isize, excluded: bool) -> bool {
        if hwnd == 0 {
            return false;
        }
        let affinity = if excluded {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        // SAFETY: `hwnd` is the caller's window handle; the call only reads it.
        unsafe { SetWindowDisplayAffinity(hwnd as HWND, affinity) != 0 }
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

    /// The class name of `hwnd`, or an empty string when it cannot be read.
    fn class_name_of(hwnd: HWND) -> String {
        let mut buffer = [0u16; 256];
        // SAFETY: the buffer is valid for `buffer.len()` UTF-16 units, as the count argument says.
        let len = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buffer[..len as usize])
    }

    /// True when `SHQueryUserNotificationState` reports an exclusive Direct3D full-screen, a
    /// presentation mode, a busy full-screen state or a Store app. The state is process-wide; the
    /// caller ties it to the foreground window and to the monitor.
    fn shell_reports_fullscreen() -> bool {
        let mut state: i32 = 0;
        // SAFETY: `state` is a valid out-parameter for the call.
        let hr = unsafe { SHQueryUserNotificationState(&mut state) };
        hr >= 0
            && matches!(
                state,
                QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE | QUNS_APP
            )
    }

    /// True when the foreground window is a full-screen window of another program on `monitor`
    /// (`monitor` is that monitor's bounds in screen pixels, for example
    /// [`MonitorPlacement::bounds`]). The foreground window counts when all hold:
    ///
    /// - it is visible, not minimised, and neither the desktop nor the shell window;
    /// - it belongs to neither this process nor a pid in `probe.own_pids`;
    /// - its class is not in `probe.ignore_classes` (Alt-Tab and Task View hosts are borderless and
    ///   monitor-sized, so they are excluded this way);
    /// - it overlaps `monitor`, so a full-screen program on another monitor does not count; and
    /// - either it covers `monitor` on every edge within `probe.edge_tolerance_px`
    ///   ([`covers_monitor_within`]), or `probe.use_shell_notification_state` is set and
    ///   `SHQueryUserNotificationState` reports a busy full-screen, Direct3D full-screen,
    ///   presentation or Store-app state.
    ///
    /// Unlike [`foreground_is_borderless_fullscreen`] this does not require a borderless style, so a
    /// full-screen window that keeps a caption still counts. The non-Windows stub returns `false`.
    pub fn foreground_fullscreen_on(monitor: WorkArea, probe: &FullScreenProbe) -> bool {
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
            if crate::is_own_pid(pid, std::process::id(), &probe.own_pids) {
                return false;
            }
            let class = class_name_of(foreground);
            if probe
                .ignore_classes
                .iter()
                .any(|ignored| ignored.eq_ignore_ascii_case(&class))
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
            let window = area(rect);
            if !crate::rects_overlap(window, monitor) {
                return false;
            }
            if crate::covers_monitor_within(window, monitor, probe.edge_tolerance_px) {
                return true;
            }
            probe.use_shell_notification_state && shell_reports_fullscreen()
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
    /// Full-screen verdicts come from the host on macOS: the host's per-overlay callback supplies
    /// the verdict (see CHANGELOG 0.2.1), so this crate's stub is always `false`.
    pub fn foreground_fullscreen_on(
        _monitor: crate::WorkArea,
        _probe: &crate::FullScreenProbe,
    ) -> bool {
        false
    }
    pub fn create_layered_overlay_window(
        _hinstance: isize,
        _class_name: &str,
        _title: &str,
        _rect: crate::OverlayRect,
        _style: crate::OverlayStyle,
        _pixel_alpha_empty_hit_regions: bool,
    ) -> isize {
        0
    }
    pub fn create_message_only_window(
        _class_name: &str,
    ) -> Result<isize, crate::WindowCreateError> {
        Err(crate::WindowCreateError::Unavailable)
    }
    pub fn set_capture_excluded(_hwnd: isize, _excluded: bool) -> bool {
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
