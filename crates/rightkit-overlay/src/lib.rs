//! Platform-neutral overlay state and placement policy.
//!
//! Native hosts are responsible for creating windows. This crate keeps the policy identical for
//! the React pill, native pill, and Cockpit notch: finite geometry, work-area clamping, delayed
//! hover collapse, and the HR TOPMOST/status level contract.

use serde::{Deserialize, Serialize};

pub const HR_STATUS_WINDOW_LEVEL: i32 = 25;
/// Windows native hosts must keep both the extended style and actual z-order in this band.
pub const WINDOWS_TOPMOST_REQUIRED: bool = true;
pub const HOVER_ENTER_TICKS: u32 = 1;
pub const HOVER_LEAVE_TICKS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind { HeardRightPill, CockpitNotch }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PillMode { Idle, Starting, Recording, Processing, Status, Error }

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
pub struct Rect { pub x: f64, pub y: f64, pub width: f64, pub height: f64 }

impl Rect {
    pub fn finite(self) -> bool {
        [self.x, self.y, self.width, self.height].into_iter().all(f64::is_finite)
            && self.width >= 0.0 && self.height >= 0.0
    }
    pub fn contains(self, x: f64, y: f64) -> bool {
        self.finite() && x.is_finite() && y.is_finite()
            && x >= self.x && x <= self.x + self.width
            && y >= self.y && y <= self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkArea { pub left: f64, pub top: f64, pub right: f64, pub bottom: f64 }

/// A watchdog calls this after foreground/z-order notifications. Reassert only when either the
/// style bit or observed band is lost; preserving focus is the caller's responsibility.
pub fn should_reassert_windows_topmost(style_topmost: bool, in_topmost_band: bool) -> bool {
    !style_topmost || !in_topmost_band
}

/// Windows-native overlay operations, ported from HeardRight's mature overlay host at
/// `/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/pill/state.rs`. Hosts pass the raw
/// `HWND` value as an `isize` so the crate stays independent of a Tauri/WebView window type while
/// still owning the locked no-activation and TOPMOST behavior. Hosts must install the WNDPROC
/// guard on the thread that owns the HWND, before that window receives UI messages.
#[cfg(windows)]
pub mod windows {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, EnumWindows, GetWindowLongPtrW, IsWindowVisible,
        SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, GWLP_WNDPROC, HWND_TOPMOST,
        MA_NOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        WM_MOUSEACTIVATE, WM_NCDESTROY, WNDPROC, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
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
        let original = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, noactivate_wndproc as *const () as isize);
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

            let mut state = State { target: hwnd as isize, found: false, band_broken: false };
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
        if is_out_of_topmost_band(hwnd) { reassert_topmost(hwnd) } else { false }
    }
}

#[cfg(not(windows))]
pub mod windows {
    /// Windows-native operations are unavailable on non-Windows hosts.
    pub fn is_out_of_topmost_band(_hwnd: isize) -> bool { false }
    pub fn make_noactivate(_hwnd: isize) -> bool { false }
    pub fn reassert_topmost(_hwnd: isize) -> bool { false }
    pub fn reassert_if_needed(_hwnd: isize) -> bool { false }
}

impl WorkArea {
    pub fn clamp(self, frame: Rect) -> Rect {
        if !frame.finite() { return frame; }
        let max_x = (self.right - frame.width).max(self.left);
        let max_y = (self.bottom - frame.height).max(self.top);
        Rect { x: frame.x.clamp(self.left, max_x), y: frame.y.clamp(self.top, max_y), ..frame }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoverGate { inside_ticks: u32, outside_ticks: u32, hovering: bool }

impl HoverGate {
    pub fn update(&mut self, inside: bool) -> bool {
        if inside { self.inside_ticks = self.inside_ticks.saturating_add(1); self.outside_ticks = 0; }
        else { self.outside_ticks = self.outside_ticks.saturating_add(1); self.inside_ticks = 0; }
        self.hovering = if self.hovering { self.outside_ticks < HOVER_LEAVE_TICKS }
            else { inside && self.inside_ticks >= HOVER_ENTER_TICKS };
        self.hovering
    }
    pub fn reset(&mut self) { *self = Self::default(); }
    pub fn is_hovering(&self) -> bool { self.hovering }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlayModel {
    pub kind: OverlayKind,
    pub visible: bool,
    pub mode: Option<PillMode>,
    pub frame: Rect,
    pub hitbox: Option<Rect>,
}

impl OverlayModel {
    pub fn new(kind: OverlayKind, frame: Rect) -> Self {
        Self { kind, visible: false, mode: None, frame, hitbox: None }
    }
    pub fn apply(&mut self, event: OverlayEvent) {
        self.mode = event.mode();
        self.visible = self.mode.is_some();
        if self.hitbox.is_none() { self.hitbox = Some(self.frame); }
    }
    pub fn place(&mut self, work: WorkArea) { self.frame = work.clamp(self.frame); }
    pub fn hit_test(&self, x: f64, y: f64) -> bool {
        self.visible && self.hitbox.unwrap_or(self.frame).contains(x, y)
    }
}
