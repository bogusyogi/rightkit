// Raw clipboard access: transcript/fallback writes, Windows read/clear, and
// the copy-based selected-text fetch used by the summarize-on-selection
// fallback.

use super::*;

#[cfg(target_os = "windows")]
pub(crate) fn write_clipboard_text(text: &str) -> Result<(), CopyFallbackReason> {
    if rightkit_automation::desktop::windows::write_clipboard_text(text) {
        Ok(())
    } else {
        Err(CopyFallbackReason::ClipboardUnavailable)
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn write_clipboard_text(text: &str) -> Result<(), CopyFallbackReason> {
    if crate::macos_input::set_clipboard(text) {
        Ok(())
    } else {
        Err(CopyFallbackReason::ClipboardUnavailable)
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn write_clipboard_text(_text: &str) -> Result<(), CopyFallbackReason> {
    Err(CopyFallbackReason::UnsupportedPlatform)
}

/// Read the current clipboard as a UTF-16 text string. Returns `None` if the
/// clipboard is empty, holds non-text data, or cannot be opened.
#[cfg(target_os = "windows")]
fn read_clipboard_text() -> Option<String> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT as u32) == 0 {
            return None;
        }
        if OpenClipboard(null_mut::<std::ffi::c_void>() as HWND) == 0 {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT as u32);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }
        let ptr = GlobalLock(handle) as *const u16;
        let text = if ptr.is_null() {
            None
        } else {
            // Walk to the null terminator.
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            Some(String::from_utf16_lossy(slice))
        };
        GlobalUnlock(handle);
        // NOTE: do NOT GlobalFree — the clipboard still owns this handle.
        CloseClipboard();
        text
    }
}

#[cfg(target_os = "windows")]
fn clear_clipboard() -> Result<(), ()> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};

    unsafe {
        if OpenClipboard(null_mut::<std::ffi::c_void>() as HWND) == 0 {
            return Err(());
        }
        let emptied = EmptyClipboard() != 0;
        CloseClipboard();
        if !emptied {
            return Err(());
        }
    }
    Ok(())
}

/// Copy-based selection fetch for the summarize-on-selection fallback:
/// selections in READ-ONLY regions (chat transcripts,
/// rendered web pages) are invisible to the focused-control UIA read, so when
/// the user says bare "summarize" with such a selection, the engine grabs it
/// with a synthetic Ctrl+C. The user's clipboard is saved first and restored
/// after — the fetch is invisible to them. Returns None when nothing was
/// selected (the Ctrl+C produced no new clipboard text within the poll
/// window), letting the caller fall through to ordinary dictation.
#[cfg(target_os = "windows")]
pub fn fetch_selected_text_via_copy() -> Option<String> {
    let saved = rightkit_automation::desktop::windows::snapshot_general_clipboard()?;
    let _ = clear_clipboard();
    let mut copied_sequence = rightkit_automation::desktop::windows::clipboard_sequence_number();
    if !rightkit_automation::desktop::windows::send_copy_keystroke() {
        let _ = rightkit_automation::desktop::windows::restore_general_clipboard_if_unchanged(
            saved,
            copied_sequence,
        );
        return None;
    }
    // The target app processes Ctrl+C asynchronously — poll briefly (mirrors
    // the paste-settle reasoning: SendInput only QUEUES the keystroke).
    let mut got = None;
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(30));
        if let Some(text) = read_clipboard_text() {
            if !text.trim().is_empty() {
                copied_sequence =
                    rightkit_automation::desktop::windows::clipboard_sequence_number();
                got = Some(text);
                break;
            }
        }
    }
    // Restore every prior clipboard format unless a newer writer won.
    let _ = rightkit_automation::desktop::windows::restore_general_clipboard_if_unchanged(
        saved,
        copied_sequence,
    );
    got
}

/// macOS counterpart to the Windows read-only selection fallback. AX can
/// expose no selected text for rendered browser/chat content, while the
/// target still handles Cmd+C. Snapshot every pasteboard item/type first,
/// require AppKit's change count to advance, then restore only if no other
/// writer changed the pasteboard during the operation.
#[cfg(target_os = "macos")]
pub fn fetch_selected_text_via_copy() -> Option<String> {
    if !crate::macos_input::accessibility_trusted(false) {
        return None;
    }
    let _clipboard_transaction =
        rightkit_automation::desktop::macos::acquire_clipboard_transaction();
    let snapshot = rightkit_automation::desktop::macos::snapshot_general_pasteboard()?;
    if !crate::macos_input::copy_combo() {
        return None;
    }

    let mut selected = None;
    let mut copied_change_count = snapshot.change_count();
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(30));
        copied_change_count = rightkit_automation::desktop::macos::pasteboard_change_count();
        if copied_change_count != snapshot.change_count() {
            selected = crate::macos_input::get_clipboard()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
            if selected.is_some() {
                break;
            }
        }
    }

    if copied_change_count != snapshot.change_count()
        && !rightkit_automation::desktop::macos::restore_general_pasteboard_if_unchanged_owned(
            snapshot,
            copied_change_count,
        )
    {
        tracing::warn!(
            expected_change_count = copied_change_count,
            "macos_selection_copy_clipboard_changed_during_restore"
        );
    }
    selected
}
