//! A one-shot no-activate sweep over this process's top-level windows (Windows).
//!
//! Some windows a host creates itself (a webview, a splash) can come up without the
//! "never take foreground" styles. [`sweep_titles`] walks the top-level windows of this process,
//! and for each whose title is exactly one of `titles` stamps the no-activate style through
//! `rightkit_overlay::windows::make_noactivate`. The titles are the host's (the crate has none of
//! its own); it returns how many windows were changed so the host can log it. It reads window
//! titles only to compare them and never keeps, logs or returns them.

use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

struct Ctx {
    pid: u32,
    titles: Vec<Vec<u16>>,
    changed: usize,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    let ctx = unsafe { &mut *(lparam as *mut Ctx) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid != ctx.pid {
        return 1;
    }

    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return 1;
    }
    let mut title = vec![0u16; len as usize + 1];
    let read = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    if read <= 0 {
        return 1;
    }
    title.truncate(read as usize);
    if ctx
        .titles
        .iter()
        .any(|candidate| candidate.as_slice() == title.as_slice())
        && rightkit_overlay::windows::make_noactivate(hwnd as isize)
    {
        ctx.changed += 1;
    }
    1
}

/// Stamp the no-activate style on every top-level window of this process whose title equals one of
/// `titles`. Returns the number of windows changed.
pub fn sweep_titles(titles: &[&str]) -> usize {
    let mut ctx = Ctx {
        pid: std::process::id(),
        titles: titles
            .iter()
            .map(|title| title.encode_utf16().collect::<Vec<u16>>())
            .collect(),
        changed: 0,
    };
    unsafe {
        EnumWindows(Some(enum_proc), &mut ctx as *mut Ctx as LPARAM);
    }
    ctx.changed
}
