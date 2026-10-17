//! Windows evidence for the capture-safety probe (see `crate::capture_safety`): each window's
//! display affinity, and whether the focused control is a password field (`ES_PASSWORD` on a
//! classic edit control, UI Automation `IsPassword` for everything else). Reads only.

use super::{sys, uia};
use crate::capture_safety::{
    affinity_outcome, is_password_edit, uia_password_outcome, GapKind, SafetyCheck, SafetyEvidence,
    SafetyOutcome, SafetyReason,
};
use crate::model::Target;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetGUIThreadInfo, GetWindowDisplayAffinity, GetWindowLongPtrW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, GUITHREADINFO, GWL_STYLE,
};

pub fn collect(target: &Target) -> Vec<SafetyEvidence> {
    let mut out = window_affinity(target);
    out.push(SafetyEvidence::new(
        SafetyCheck::FocusedSecureField,
        focus_outcome(target),
    ));
    out
}

fn affinity_of(h: HWND) -> Option<u32> {
    let mut a = 0u32;
    unsafe { GetWindowDisplayAffinity(h, &mut a) }
        .ok()
        .map(|_| a)
}

fn window_affinity(target: &Target) -> Vec<SafetyEvidence> {
    let ev = |o| SafetyEvidence::new(SafetyCheck::WindowCapturable, o);
    if let Some(id) = target.window_id {
        let h = HWND(id as isize as *mut _);
        let ours = unsafe { IsWindow(Some(h)) }.as_bool() && sys::pid_of(h) == Some(target.pid);
        return if ours {
            vec![ev(affinity_outcome(id, affinity_of(h)))]
        } else {
            vec![ev(SafetyOutcome::Unavailable(
                GapKind::TargetWindowNotFound,
            ))]
        };
    }
    let out: Vec<_> = sys::all_top_level()
        .into_iter()
        .filter(|&h| sys::pid_of(h) == Some(target.pid))
        .filter(|&h| unsafe { IsWindowVisible(h) }.as_bool() && !unsafe { IsIconic(h) }.as_bool())
        .map(|h| ev(affinity_outcome(h.0 as u64, affinity_of(h))))
        .collect();
    if out.is_empty() {
        vec![ev(SafetyOutcome::Unavailable(GapKind::NoVisibleWindow))]
    } else {
        out
    }
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Class and style of the control holding keyboard focus in the target window's thread.
/// `None` when the thread has no focus window or the read fails.
fn win32_focus(top: HWND) -> Option<(String, u32)> {
    let tid = unsafe { GetWindowThreadProcessId(top, None) };
    if tid == 0 {
        return None;
    }
    let mut gi = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(tid, &mut gi) }.ok()?;
    let f = gi.hwndFocus;
    if f.0.is_null() {
        return None;
    }
    let style = unsafe { GetWindowLongPtrW(f, GWL_STYLE) } as u32;
    Some((class_name(f), style))
}

fn focus_outcome(target: &Target) -> SafetyOutcome {
    let Some(top) = sys::main_window(target.pid, target.window_id) else {
        return SafetyOutcome::Unavailable(GapKind::NoVisibleWindow);
    };
    let top_h = HWND(top as *mut _);
    // Positive evidence from the cheap path: a focused classic edit control with ES_PASSWORD.
    let classic = win32_focus(top_h);
    if let Some((class, style)) = &classic {
        if is_password_edit(class, *style) {
            return SafetyOutcome::Found(SafetyReason::PasswordFieldFocused);
        }
    }
    // Everything else (WPF, XAML, browsers, Electron) needs UI Automation.
    let root = uia::element_from_hwnd(top).ok();
    match uia::focused_editor(target.pid, root) {
        Some(el) => uia_password_outcome(el.password_state()),
        None => match classic {
            // UIA found no focus, but the thread's focus is a classic edit without ES_PASSWORD.
            Some((class, _)) if class.eq_ignore_ascii_case("edit") => SafetyOutcome::Clear,
            _ => SafetyOutcome::Unavailable(GapKind::ReadFailed("UIA focused element")),
        },
    }
}
