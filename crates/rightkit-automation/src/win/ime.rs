//! Window-targeted IME composition (IMM32): set, commit and cancel the composition string of one
//! window, and find where the IME put its candidate list. No `SendInput`, no focus change, no
//! global key state.
//!
//! IMM32 input contexts belong to the process that owns the window. `compose`, `commit` and
//! `cancel` therefore work on windows of **this** process (a probe or test harness that hosts the
//! window it drives); for a window of another process `ImmGetContext` returns no context and the
//! calls fail with `Error::Unsupported`. Driving a real IME in a foreign process takes key input
//! (the foreground tier) plus [`candidate_rect`], which only reads window geometry and works
//! across processes.
//!
//! Every call that changes a composition honours the kill switch (`policy::kill`); like the
//! other raw primitives here it writes no receipt, so a host with an admission hook asks it first.

use crate::model::*;
use crate::policy::killed;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::Ime::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The state the IME holds after [`compose`], read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Composition {
    /// Length of the composition string the IME now holds, in UTF-16 units.
    pub len_utf16: usize,
    /// The IME's cursor position inside it, in UTF-16 units (`GCS_CURSORPOS`).
    pub cursor: usize,
}

/// The window's input context, released on drop.
struct Context {
    hwnd: HWND,
    himc: HIMC,
}

impl Context {
    fn of(hwnd: u64) -> Result<Context> {
        if killed() {
            return Err(Error::Blocked("kill switch engaged".into()));
        }
        if hwnd == 0 {
            return Err(Error::NotFound("IME window 0".into()));
        }
        let h = HWND(hwnd as isize as *mut _);
        if !unsafe { IsWindow(Some(h)) }.as_bool() {
            return Err(Error::NotFound(format!("window {hwnd}")));
        }
        let himc = unsafe { ImmGetContext(h) };
        if himc.0.is_null() {
            return Err(Error::Unsupported(
                "no IME input context for this window (IMM32 contexts are per process: only windows of this process can be driven)",
            ));
        }
        Ok(Context { hwnd: h, himc })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let _ = unsafe { ImmReleaseContext(self.hwnd, self.himc) };
    }
}

/// Set the composition string of `hwnd` to `text` (`ImmSetCompositionStringW` with
/// `SCS_SETSTR`), as if the IME had produced it, and read back what the IME holds. IMM32 has no
/// call to place the composition cursor: the IME decides it, so it is returned, not set. The
/// IME must be open for the window's thread; a closed IME refuses and the call fails.
pub fn compose(hwnd: u64, text: &str) -> Result<Composition> {
    if text.is_empty() {
        return Err(Error::Invalid("composition text is empty".into()));
    }
    let ctx = Context::of(hwnd)?;
    let units: Vec<u16> = text.encode_utf16().collect();
    let set = unsafe {
        ImmSetCompositionStringW(
            ctx.himc,
            SCS_SETSTR,
            Some(units.as_ptr() as *const core::ffi::c_void),
            (units.len() * 2) as u32,
            None,
            0,
        )
    };
    if !set.as_bool() {
        return Err(Error::Platform(
            0,
            "ImmSetCompositionStringW (is the IME open?)",
        ));
    }
    // Both reads return a byte length, or a character position for the cursor.
    let bytes = unsafe { ImmGetCompositionStringW(ctx.himc, GCS_COMPSTR, None, 0) };
    let cursor = unsafe { ImmGetCompositionStringW(ctx.himc, GCS_CURSORPOS, None, 0) };
    Ok(Composition {
        len_utf16: usize::try_from(bytes).unwrap_or(0) / 2,
        cursor: usize::try_from(cursor).unwrap_or(0),
    })
}

fn notify(hwnd: u64, index: NOTIFY_IME_INDEX, op: &'static str) -> Result<()> {
    let ctx = Context::of(hwnd)?;
    if unsafe { ImmNotifyIME(ctx.himc, NI_COMPOSITIONSTR, index, 0) }.as_bool() {
        Ok(())
    } else {
        Err(Error::Platform(0, op))
    }
}

/// Commit the current composition of `hwnd`: the IME's result string is delivered to the window
/// (`ImmNotifyIME`, `NI_COMPOSITIONSTR` with `CPS_COMPLETE`).
pub fn commit(hwnd: u64) -> Result<()> {
    notify(hwnd, CPS_COMPLETE, "ImmNotifyIME(CPS_COMPLETE)")
}

/// Cancel the current composition of `hwnd` without delivering text (`CPS_CANCEL`).
pub fn cancel(hwnd: u64) -> Result<()> {
    notify(hwnd, CPS_CANCEL, "ImmNotifyIME(CPS_CANCEL)")
}

/// Whether a window class names an IME candidate list: the class contains "candidate", ignoring
/// case (the classic `CandidateWindow`, the TSF candidate UI, and the modern IME's
/// `...CandidateWindow.Host`).
fn is_candidate_class(class: &str) -> bool {
    class.to_ascii_lowercase().contains("candidate")
}

/// Screen rectangle of the IME candidate list currently shown, or `None` when no candidate
/// window is visible. Read-only (window enumeration and geometry), so it works for any process
/// and is not gated by the kill switch. `hwnd` is only a sanity check that the window the caller
/// drives still exists: the modern candidate window belongs to the system's text-input host, not
/// to the target, so a visible candidate window anywhere on the desktop is the one returned (the
/// frontmost in z-order if several).
pub fn candidate_rect(hwnd: u64) -> Option<Rect> {
    if hwnd != 0 && !unsafe { IsWindow(Some(HWND(hwnd as isize as *mut _))) }.as_bool() {
        return None;
    }
    // Top-level windows front to back; a candidate list may also be an owned popup, which
    // EnumWindows still lists.
    for h in super::sys::all_top_level() {
        if !unsafe { IsWindowVisible(h) }.as_bool() {
            continue;
        }
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(h, &mut buf) };
        if n <= 0 {
            continue;
        }
        if !is_candidate_class(&String::from_utf16_lossy(&buf[..n as usize])) {
            continue;
        }
        if let Some(r) = super::sys::window_rect(h) {
            if r.w > 0.0 && r.h > 0.0 {
                return Some(r);
            }
        }
    }
    None
}
