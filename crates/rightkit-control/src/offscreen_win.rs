//! Windows backend for off-screen launch (cfg(windows) only; UNCOMPILED on the Mac
//! lead's host, for the Windows lead to build and qualify).
//!
//! Raw user32/kernel32 FFI (no `windows` crate generation coupling).
//! - `SetWindowPos` to off-monitor virtual-screen coordinates, then `ShowWindow(SW_SHOWNOACTIVATE)`:
//!   a window hidden at start is moved off-screen while still hidden and shown without activation,
//!   matching macOS (never on a display). A minimized window is refused; `SW_HIDE` is restore-only.
//! - EcoQoS / efficiency-mode opt-out: `SetProcessInformation(ProcessPowerThrottling)`
//!   with ControlMask = EXECUTION_SPEED, StateMask = 0 (the App Nap analogue), reset to
//!   system-managed (ControlMask = 0) at session end.
#![allow(non_snake_case)]

use super::*;
use std::ffi::c_void;

type Hwnd = isize;
// Same signatures as embedded/win_view.rs: one extern declaration per symbol per crate.
type RawHwnd = *mut std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct PROCESS_POWER_THROTTLING_STATE {
    Version: u32,
    ControlMask: u32,
    StateMask: u32,
}

const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const SW_HIDE: i32 = 0;
const SW_SHOWNOACTIVATE: i32 = 4;
const PROCESS_POWER_THROTTLING_CURRENT_VERSION: u32 = 1;
const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: u32 = 1;
const PROCESS_POWER_THROTTLING_CLASS: i32 = 4; // ProcessPowerThrottling

#[link(name = "user32")]
extern "system" {
    fn GetWindowRect(h: RawHwnd, r: *mut WinRect) -> i32;
    fn SetWindowPos(
        h: RawHwnd,
        after: RawHwnd,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn IsWindow(h: Hwnd) -> i32;
    fn IsWindowVisible(h: Hwnd) -> i32;
    fn IsIconic(h: Hwnd) -> i32;
    fn ShowWindow(h: Hwnd, cmd: i32) -> i32;
    fn GetDpiForWindow(h: Hwnd) -> u32;
    fn GetWindowThreadProcessId(h: Hwnd, pid: *mut u32) -> u32;
    fn EnumDisplayMonitors(
        hdc: isize,
        clip: *const WinRect,
        cb: extern "system" fn(isize, isize, *const WinRect, isize) -> i32,
        data: isize,
    ) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn GetLastError() -> u32;
    fn SetProcessInformation(h: isize, class: i32, info: *const c_void, size: u32) -> i32;
}

extern "system" fn monitor_cb(_: isize, _: isize, r: *const WinRect, data: isize) -> i32 {
    let out = unsafe { &mut *(data as *mut Vec<Rect>) };
    let r = unsafe { *r };
    out.push(Rect::new(
        f64::from(r.left),
        f64::from(r.top),
        f64::from(r.right - r.left),
        f64::from(r.bottom - r.top),
    ));
    1
}

fn set_throttling(control: u32) -> Result<(), OffscreenError> {
    let st = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: control,
        StateMask: 0,
    };
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            PROCESS_POWER_THROTTLING_CLASS,
            &st as *const _ as *const c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if ok == 0 {
        Err(OffscreenError::ThrottlingOptOutFailed(unsafe {
            GetLastError()
        }))
    } else {
        Ok(())
    }
}

struct WinWindow(Hwnd);

impl WinWindow {
    fn rect(&self) -> Result<Rect, OffscreenError> {
        let mut r = WinRect::default();
        if unsafe { IsWindow(self.0) } == 0
            || unsafe { GetWindowRect(self.0 as RawHwnd, &mut r) } == 0
        {
            return Err(OffscreenError::WindowGone);
        }
        Ok(Rect::new(
            f64::from(r.left),
            f64::from(r.top),
            f64::from(r.right - r.left),
            f64::from(r.bottom - r.top),
        ))
    }
}

impl WindowOps for WinWindow {
    fn displays(&self) -> Result<Vec<Rect>, OffscreenError> {
        let mut v: Vec<Rect> = vec![];
        unsafe { EnumDisplayMonitors(0, std::ptr::null(), monitor_cb, &mut v as *mut _ as isize) };
        Ok(v)
    }
    fn frame(&self) -> Result<Rect, OffscreenError> {
        self.rect()
    }
    fn backing_scale(&self) -> Result<f64, OffscreenError> {
        Ok(f64::from(unsafe { GetDpiForWindow(self.0) }) / 96.0)
    }
    fn is_visible(&self) -> Result<bool, OffscreenError> {
        Ok(unsafe { IsWindowVisible(self.0) } != 0)
    }
    fn is_miniaturized(&self) -> Result<bool, OffscreenError> {
        Ok(unsafe { IsIconic(self.0) } != 0)
    }
    fn precheck(&self) -> Result<(), OffscreenError> {
        if self.is_miniaturized()? {
            return Err(OffscreenError::WindowMiniaturized);
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(self.0, &mut pid) };
        if pid != std::process::id() {
            return Err(OffscreenError::WindowGone);
        }
        Ok(())
    }
    fn set_origin(&mut self, x: f64, y: f64) -> Result<(), OffscreenError> {
        let ok = unsafe {
            SetWindowPos(
                self.0 as RawHwnd,
                std::ptr::null_mut(),
                x as i32,
                y as i32,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
        if ok == 0 {
            return Err(OffscreenError::Native(format!(
                "SetWindowPos failed: {}",
                unsafe { GetLastError() }
            )));
        }
        Ok(())
    }
    /// Called after `set_origin`, so the window becomes visible only once it is off every display.
    /// `SW_SHOWNOACTIVATE` does not activate or change the foreground window.
    fn show(&mut self) -> Result<(), OffscreenError> {
        unsafe { ShowWindow(self.0, SW_SHOWNOACTIVATE) }; // previous visibility, not success
        if !self.is_visible()? {
            return Err(OffscreenError::Native(
                "ShowWindow(SW_SHOWNOACTIVATE) left the window hidden".to_string(),
            ));
        }
        Ok(())
    }
    /// Hide before moving back, like macOS `orderOut:`, so the restore never flashes the
    /// recording window onto a display. The window stays hidden after the session.
    fn hide_for_restore(&mut self) -> Result<(), OffscreenError> {
        // ShowWindow returns the previous visibility, not success; the restore then verifies the frame.
        unsafe { ShowWindow(self.0, SW_HIDE) };
        Ok(())
    }
}

pub struct Session {
    hwnd: Hwnd,
    placement: Placement,
}

/// # Safety
/// `hwnd` carries a live HWND value; main (UI) thread.
pub(super) unsafe fn start(
    hwnd: *mut c_void,
    _unused: *mut c_void,
) -> Result<Session, OffscreenError> {
    if hwnd.is_null() {
        return Err(OffscreenError::NullWindow);
    }
    let hwnd = hwnd as Hwnd;
    set_throttling(PROCESS_POWER_THROTTLING_EXECUTION_SPEED)?;
    let mut w = WinWindow(hwnd);
    let outcome = place(&mut w).and_then(|p| {
        let s = Session { hwnd, placement: p };
        qualify(&snapshot(&w, SpiStatus::NotApplicable)?, Platform::Windows).into_result()?;
        Ok(s)
    });
    match outcome {
        Ok(s) => Ok(s),
        Err(e) => {
            let _ = set_throttling(0);
            Err(e)
        }
    }
}

pub(super) fn report(s: &Session) -> SessionReport {
    SessionReport {
        pid: std::process::id(),
        window_id: s.hwnd as u64,
        placed: s.placement.placed,
        original: s.placement.original,
        backing_scale: s.placement.backing_scale,
        qualified: true,
    }
}

pub(super) fn session_qualification(s: &Session) -> Result<QualificationReport, OffscreenError> {
    let w = WinWindow(s.hwnd);
    Ok(super::qualify(
        &snapshot(&w, SpiStatus::NotApplicable)?,
        Platform::Windows,
    ))
}

pub(super) fn end(s: Session) -> Result<(), OffscreenError> {
    let mut w = WinWindow(s.hwnd);
    let r = restore(&mut w, &s.placement);
    let t = set_throttling(0); // ControlMask 0: back to system-managed
    r.and(t)
}
