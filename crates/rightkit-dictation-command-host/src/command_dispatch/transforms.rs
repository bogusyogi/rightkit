// Special ops (backspace-last / clear-clipboard / always-on-top) + last-paste
// transform (undo + clipboard rewrite + paste).

#[cfg(any(target_os = "windows", target_os = "macos"))]
use super::entry::DispatchOutcome;
use super::entry::{DispatchError, DispatchResult};

// ---------------------------------------------------------------------------
// Special — small set of misc actions
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(super) fn dispatch_special(op: &str, last_text: Option<&str>) -> DispatchResult {
    match op {
        "backspace_last" => {
            // Best-effort: undo the last paste via Ctrl+Z rather than
            // tracking last-paste length to backspace exactly that many chars.
            verify_last_paste_before_undo(last_text)?;
            super::keys::send_chord("ctrl+z")
                .map(|_| DispatchOutcome::new("special", "backspace_last (Ctrl+Z)"))
                .map_err(|e| DispatchError::new("E_BACKSPACE_LAST_FAILED", e.message))
        }
        "clear_clipboard" => {
            clear_clipboard_win32().map(|_| DispatchOutcome::new("special", "clear_clipboard"))
        }
        "always_on_top" => toggle_always_on_top().map(|topmost| {
            DispatchOutcome::new("special", format!("always_on_top topmost={topmost}"))
        }),
        other => Err(DispatchError::new(
            "E_BAD_SPECIAL_OP",
            format!("unknown special op {:?}", other),
        )),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn dispatch_special(op: &str, last_text: Option<&str>) -> DispatchResult {
    match op {
        // Undo the last paste via ⌘Z (the chord maps ctrl→⌘).
        "backspace_last" => {
            verify_last_paste_before_undo(last_text)?;
            super::keys::send_chord_macos("ctrl+z")
                .map(|_| DispatchOutcome::new("special", "backspace_last (Cmd+Z)"))
                .map_err(|e| DispatchError::new("E_BACKSPACE_LAST_FAILED", e.message))
        }
        "clear_clipboard" => {
            if crate::macos_input::set_clipboard("") {
                Ok(DispatchOutcome::new("special", "clear_clipboard"))
            } else {
                Err(DispatchError::new(
                    "E_CLIPBOARD_EMPTY",
                    "could not clear pasteboard",
                ))
            }
        }
        // always_on_top toggles the app's own pill/hub window level — that's the
        // Tauri NSPanel work (port plan Phase 4), not wired yet.
        "always_on_top" => Err(DispatchError::new(
            "E_UNSUPPORTED",
            "always_on_top is not wired on macOS yet",
        )),
        "mac_force_quit" => super::keys::send_chord_macos("cmd+opt+escape")
            .map(|_| DispatchOutcome::new("special", "force_quit"))
            .map_err(|e| DispatchError::new("E_FORCE_QUIT_FAILED", e.message)),
        other => Err(DispatchError::new(
            "E_BAD_SPECIAL_OP",
            format!("unknown special op {:?}", other),
        )),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(super) fn dispatch_special(op: &str, _last_text: Option<&str>) -> DispatchResult {
    Err(DispatchError::new(
        "E_UNSUPPORTED",
        format!("special op not supported: {}", op),
    ))
}

#[cfg(target_os = "windows")]
fn clear_clipboard_win32() -> Result<(), DispatchError> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err(DispatchError::new(
                "E_CLIPBOARD_OPEN",
                "OpenClipboard failed",
            ));
        }
        let ok = EmptyClipboard() != 0;
        let _ = CloseClipboard();
        if !ok {
            return Err(DispatchError::new(
                "E_CLIPBOARD_EMPTY",
                "EmptyClipboard failed",
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn toggle_always_on_top() -> Result<bool, DispatchError> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowLongW, SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST,
        HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_TOPMOST,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return Err(DispatchError::new(
                "E_NO_FOREGROUND_WINDOW",
                "no foreground window to toggle",
            ));
        }
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let currently_topmost = (ex_style & WS_EX_TOPMOST as i32) != 0;
        let insert_after = if currently_topmost {
            HWND_NOTOPMOST
        } else {
            HWND_TOPMOST
        };
        let ok = SetWindowPos(
            hwnd,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        if ok == 0 {
            return Err(DispatchError::new(
                "E_ALWAYS_ON_TOP_FAILED",
                "SetWindowPos failed",
            ));
        }
        Ok(!currently_topmost)
    }
}

/// Refuse a blind Ctrl/Cmd+Z: the focused field must still end with exactly
/// the text we last inserted (AX on macOS, UIA on Windows). Anything else
/// would undo whatever the user did since, so the caller gets a visible error.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn verify_last_paste_before_undo(last_text: Option<&str>) -> Result<(), DispatchError> {
    use rightkit_automation::desktop::{verify_last_paste_undoable, UndoVerdict};
    // No recorded paste: refuse before touching AX/UIA at all.
    let field = if last_text.map(str::trim).is_some_and(|t| !t.is_empty()) {
        crate::focus::focused_field_text_for_undo()
    } else {
        None
    };
    match verify_last_paste_undoable(field.as_deref(), last_text, None) {
        UndoVerdict::Safe => Ok(()),
        UndoVerdict::NoRecordedPaste => Err(DispatchError::new(
            "E_UNDO_NO_RECORDED_PASTE",
            "Undo skipped: no recent dictation paste to undo",
        )),
        UndoVerdict::FieldUnreadable => Err(DispatchError::new(
            "E_UNDO_FIELD_UNREADABLE",
            "Undo skipped: could not verify the text field",
        )),
        UndoVerdict::TextChanged => Err(DispatchError::new(
            "E_UNDO_TEXT_CHANGED",
            "Undo skipped: the text field changed since the last paste",
        )),
    }
}

// ---------------------------------------------------------------------------
// Last paste transform — undo + clipboard rewrite + paste
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(super) fn dispatch_last_paste_transform(
    transform: &str,
    last_text: Option<&str>,
) -> DispatchResult {
    let Some(prev) = last_text.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DispatchOutcome::new(
            "last_paste_transform",
            "nothing to transform",
        ));
    };
    let new_text = transform_last_text(prev, transform)?;
    verify_last_paste_before_undo(last_text)?;
    super::keys::send_chord("ctrl+z")
        .map_err(|e| DispatchError::new("E_TRANSFORM_UNDO_FAILED", e.message))?;
    std::thread::sleep(std::time::Duration::from_millis(50));
    crate::delivery::paste_text(&new_text)
        .map_err(|e| DispatchError::new("E_TRANSFORM_PASTE_FAILED", e.to_string()))?;
    Ok(DispatchOutcome::new(
        "last_paste_transform",
        format!("transform={transform:?} chars={}", new_text.chars().count()),
    ))
}

#[cfg(target_os = "macos")]
pub(super) fn dispatch_last_paste_transform(
    transform: &str,
    last_text: Option<&str>,
) -> DispatchResult {
    let Some(prev) = last_text.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DispatchOutcome::new(
            "last_paste_transform",
            "nothing to transform",
        ));
    };
    let new_text = transform_last_text(prev, transform)?;
    verify_last_paste_before_undo(last_text)?;
    super::keys::send_chord_macos("ctrl+z")
        .map_err(|e| DispatchError::new("E_TRANSFORM_UNDO_FAILED", e.message))?;
    std::thread::sleep(std::time::Duration::from_millis(50));
    crate::delivery::paste_text(&new_text)
        .map_err(|e| DispatchError::new("E_TRANSFORM_PASTE_FAILED", e.to_string()))?;
    Ok(DispatchOutcome::new(
        "last_paste_transform",
        format!("transform={transform:?} chars={}", new_text.chars().count()),
    ))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(super) fn dispatch_last_paste_transform(
    _transform: &str,
    _last_text: Option<&str>,
) -> DispatchResult {
    Err(DispatchError::new(
        "E_UNSUPPORTED",
        "last_paste_transform not implemented for this OS",
    ))
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn transform_last_text(text: &str, transform: &str) -> Result<String, DispatchError> {
    match transform {
        "upper" => Ok(text.to_uppercase()),
        "lower" => Ok(text.to_lowercase()),
        "title" => {
            let mut out = String::with_capacity(text.len());
            let mut new_word = true;
            for ch in text.chars() {
                if ch.is_alphabetic() {
                    if new_word {
                        out.push_str(&ch.to_uppercase().collect::<String>());
                    } else {
                        out.push_str(&ch.to_lowercase().collect::<String>());
                    }
                    new_word = false;
                } else {
                    out.push(ch);
                    new_word = true;
                }
            }
            Ok(out)
        }
        "capitalize" => {
            let mut chars = text.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_uppercase().collect::<String>();
                    out.push_str(chars.as_str());
                    Ok(out)
                }
                None => Ok(String::new()),
            }
        }
        other => Err(DispatchError::new(
            "E_BAD_TRANSFORM",
            format!("unknown last-paste transform {:?}", other),
        )),
    }
}
