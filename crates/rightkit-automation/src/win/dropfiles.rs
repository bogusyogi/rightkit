//! Receiving dropped files: make a window of this process a `WM_DROPFILES` target and decode one
//! drop. This is the receiving side only; it never sends a drop into another app (an `HDROP` is
//! a block of this process's memory, so it cannot be handed to another process anyway).
//!
//! ```text
//! drop::accept_drops(hwnd, true)?;           // once, after the window exists
//! // in the window procedure, on WM_DROPFILES:
//! let dropped = unsafe { drop::take_drop(wparam) };
//! ```

use crate::model::*;
use std::path::PathBuf;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP,
};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

/// Most files one drop may carry; a drop of more is decoded up to this many and the rest ignored.
pub const MAX_DROPPED_FILES: u32 = 4096;

/// One decoded drop.
#[derive(Debug, Clone, PartialEq)]
pub struct Dropped {
    /// The dropped paths, in the order the shell listed them.
    pub paths: Vec<PathBuf>,
    /// The drop point in client pixels of the window that received it.
    pub point: (i32, i32),
    /// The drop landed on the non-client area (title bar, border) rather than the client area.
    pub non_client: bool,
}

/// Start (`accept` true) or stop (`false`) `hwnd`, a window of this process, receiving
/// `WM_DROPFILES`. `NotFound` for a handle that is not a window.
pub fn accept_drops(hwnd: u64, accept: bool) -> Result<()> {
    let h = HWND(hwnd as isize as *mut _);
    if hwnd == 0 || !unsafe { IsWindow(Some(h)) }.as_bool() {
        return Err(Error::NotFound(format!("window {hwnd}")));
    }
    unsafe { DragAcceptFiles(h, accept) };
    Ok(())
}

/// Decode the `HDROP` of a `WM_DROPFILES` (its `wParam`) and release it. The paths come back as
/// `PathBuf`s (no lossy conversion: unpaired surrogates survive).
///
/// # Safety
///
/// `hdrop` must be the `wParam` of a `WM_DROPFILES` message delivered to a window of this
/// process, and must not be used again afterwards: the call releases the drop (`DragFinish`),
/// so it is called exactly once per message.
pub unsafe fn take_drop(hdrop: usize) -> Dropped {
    use std::os::windows::ffi::OsStringExt;
    let h = HDROP(hdrop as *mut _);
    let mut pt = POINT::default();
    let client = unsafe { DragQueryPoint(h, &mut pt) }.as_bool();
    let count = unsafe { DragQueryFileW(h, u32::MAX, None) }.min(MAX_DROPPED_FILES);
    let mut paths = Vec::new();
    for i in 0..count {
        let len = unsafe { DragQueryFileW(h, i, None) } as usize;
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len + 1];
        let n = unsafe { DragQueryFileW(h, i, Some(&mut buf)) } as usize;
        if n > 0 && n <= len {
            paths.push(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])));
        }
    }
    unsafe { DragFinish(h) };
    Dropped {
        paths,
        point: (pt.x, pt.y),
        non_client: !client,
    }
}
