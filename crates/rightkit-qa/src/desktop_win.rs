//! Real Windows desktop input and top-level window facts for journeys about native overlays.
//!
//! Overlays are native windows of their own, outside the app's web view the control server drives, and
//! the regressions they guard are about the real desktop: the window that is hit at a screen point, and
//! a window's title, size and visibility. So a journey needs real pointer input and a full top-level
//! window listing. `rightkit-automation`'s `list_windows` leaves out tool windows
//! (`WS_EX_TOOLWINDOW`), which overlays are, so this module calls Win32 directly.
//!
//! Coordinates are the ones the journey process sees. The process declares no DPI awareness, so the
//! window rectangles it reads and the points it clicks agree, and both match the logical pixels an app
//! reports for its own windows.
#![cfg(windows)]

use std::time::Duration;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    SetCursorPos, WindowFromPoint,
};

/// One top-level window of the process under test.
#[derive(Clone, Debug)]
pub struct TopWindow {
    /// Handle as `0x...` text.
    pub hwnd: String,
    /// Window text.
    pub title: String,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}

/// The window under a screen point.
#[derive(Clone, Debug)]
pub struct PointTarget {
    /// Handle as `0x...` text.
    pub hwnd: String,
    /// Owning process id (0 when no window).
    pub pid: u32,
    /// The window belongs to the process asked about.
    pub own_process: bool,
    /// Window text.
    pub title: String,
}

// `mouse_event` lives in a Win32 feature this crate does not enable; the declaration is the documented
// user32 signature, so no dependency feature is added for one call.
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
#[link(name = "user32")]
extern "system" {
    fn mouse_event(flags: u32, dx: i32, dy: i32, data: i32, extra: usize);
}

fn hwnd_text(hwnd: HWND) -> String {
    format!("0x{:X}", hwnd as isize)
}

fn title_of(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: `buffer` is valid for `buffer.len()` UTF-16 units for the whole call.
    let count = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if count <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..count as usize])
}

struct Collect {
    pid: u32,
    found: Vec<TopWindow>,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Collect` that `top_windows` passes and keeps alive for the
    // whole `EnumWindows` call.
    let state = unsafe { &mut *(lparam as *mut Collect) };
    let mut owner = 0u32;
    // SAFETY: `hwnd` comes from the enumeration; `owner` is a valid out pointer.
    unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    if owner == state.pid {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: `rect` is a valid out pointer.
        unsafe { GetWindowRect(hwnd, &mut rect) };
        state.found.push(TopWindow {
            hwnd: hwnd_text(hwnd),
            title: title_of(hwnd),
            // SAFETY: plain query on an enumerated handle.
            visible: unsafe { IsWindowVisible(hwnd) } != 0,
            x: rect.left,
            y: rect.top,
            w: rect.right - rect.left,
            h: rect.bottom - rect.top,
        });
    }
    1
}

/// Every top-level window of process `pid`, visible or not, tool windows included.
pub fn top_windows(pid: u32) -> Vec<TopWindow> {
    let mut state = Collect {
        pid,
        found: Vec::new(),
    };
    // SAFETY: `collect` only reads `state` through the pointer, which outlives the call.
    unsafe { EnumWindows(Some(collect), &mut state as *mut Collect as LPARAM) };
    state.found
}

/// The window at screen point `(x, y)` and whether it belongs to process `pid`.
pub fn window_at(x: i32, y: i32, pid: u32) -> PointTarget {
    // SAFETY: plain query.
    let hwnd = unsafe { WindowFromPoint(POINT { x, y }) };
    let mut owner = 0u32;
    if !hwnd.is_null() {
        // SAFETY: `hwnd` is non-null; `owner` is a valid out pointer.
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    }
    PointTarget {
        hwnd: hwnd_text(hwnd),
        pid: owner,
        own_process: owner == pid,
        title: if hwnd.is_null() {
            String::new()
        } else {
            title_of(hwnd)
        },
    }
}

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Move the real cursor and let hover handling see it.
pub fn move_to(x: i32, y: i32) {
    // SAFETY: plain call.
    unsafe { SetCursorPos(x, y) };
    pause(220);
}

/// Move to `(x, y)` and press and release the left button there, with the same pauses the
/// PowerShell script this replaces used.
pub fn click_at(x: i32, y: i32) {
    // SAFETY: plain call.
    unsafe { SetCursorPos(x, y) };
    pause(80);
    // SAFETY: plain calls; no pointer arguments.
    unsafe { mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0) };
    pause(70);
    // SAFETY: as above.
    unsafe { mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0) };
    pause(180);
}
