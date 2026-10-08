//! Window state checks, DPI and OS build. Hidden and minimized windows are refused
//! because Spike B measured no frames delivered for either state.

use ::windows::core::{w, PCWSTR};
use ::windows::Win32::Foundation::{HWND, RECT};
use ::windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
use ::windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_DWORD, RRF_RT_REG_SZ,
};
use ::windows::Win32::UI::HiDpi::GetDpiForWindow;
use ::windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, IsIconic, IsWindow, IsWindowVisible,
};

use crate::error::{Code, RecordError};

pub struct WindowState {
    pub rect: RECT,
    pub dpi: u32,
    /// False when the window lies entirely off every monitor (off-screen but visible).
    pub on_monitor: bool,
}

/// Refuses a window that cannot deliver frames. Returns its geometry otherwise.
pub fn check(hwnd: HWND) -> Result<WindowState, RecordError> {
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(RecordError::new(
            Code::NotFound,
            "no window for the given HWND",
        ));
    }
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return Err(RecordError::new(
            Code::WindowHidden,
            "window is hidden (!IsWindowVisible); HWND capture delivers no frames for hidden windows",
        ));
    }
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Err(RecordError::new(
            Code::WindowMinimized,
            "window is minimized (IsIconic); HWND capture delivers no frames for minimized windows",
        ));
    }
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }
        .map_err(|e| RecordError::new(Code::NotFound, format!("GetWindowRect failed: {e}")))?;
    let dpi = match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        value => value,
    };
    let monitor = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) };
    Ok(WindowState {
        rect,
        dpi,
        on_monitor: !monitor.is_invalid(),
    })
}

/// "26300.9550" style build string from the registry. "unknown" when unreadable.
pub fn os_build() -> String {
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let build = reg_string(key, w!("CurrentBuild")).unwrap_or_else(|| "unknown".to_string());
    match reg_dword(key, w!("UBR")) {
        Some(ubr) => format!("{build}.{ubr}"),
        None => build,
    }
}

fn reg_string(key: PCWSTR, name: PCWSTR) -> Option<String> {
    let mut bytes: u32 = 0;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    }
    .ok()
    .ok()?;
    let mut buf = vec![0u16; bytes as usize / 2 + 1];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    }
    .ok()
    .ok()?;
    let units = (len as usize / 2).min(buf.len());
    Some(
        String::from_utf16_lossy(&buf[..units])
            .trim_end_matches('\0')
            .to_string(),
    )
}

fn reg_dword(key: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut value: u32 = 0;
    let mut len: u32 = 4;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            name,
            RRF_RT_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut len),
        )
    }
    .ok()
    .ok()?;
    Some(value)
}
