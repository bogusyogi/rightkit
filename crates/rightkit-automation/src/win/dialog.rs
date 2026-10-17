//! Window-targeted TaskDialog input: press one button of a Win32 task dialog (tier 3).
//!
//! `TDM_CLICK_BUTTON` makes a TaskDialog press the button whose id is in `wParam`, as if the
//! user had clicked it. The message is posted to the dialog window, so no foreground window,
//! focus change or `SendInput` is involved, and the call never waits for the dialog to answer.

use crate::model::*;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_USER};

/// `TDM_CLICK_BUTTON` from `commctrl.h` (`WM_USER + 102`).
pub const TDM_CLICK_BUTTON: u32 = WM_USER + 102;

/// Press the button `button_id` (a `TaskDialog` `nButtonID`, such as `IDOK` = 1) of the task
/// dialog whose window handle is `hwnd`. Returns once the message is queued; the dialog acts on
/// it on its own thread.
pub fn task_dialog_click(hwnd: u64, button_id: i32) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::NotFound("task dialog window 0".into()));
    }
    let handle = windows::Win32::Foundation::HWND(hwnd as isize as *mut _);
    unsafe {
        PostMessageW(
            Some(handle),
            TDM_CLICK_BUTTON,
            WPARAM(button_id as isize as usize),
            LPARAM(0),
        )
    }
    .map_err(|e| Error::Platform(i64::from(e.code().0), "PostMessage(TDM_CLICK_BUTTON)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_message_is_wm_user_plus_102() {
        assert_eq!(TDM_CLICK_BUTTON, 0x0400 + 102);
    }

    #[test]
    fn null_window_is_refused_without_posting() {
        assert!(matches!(task_dialog_click(0, 1), Err(Error::NotFound(_))));
    }
}
