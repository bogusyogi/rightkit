//! macOS low-level input + window helpers.
//!
//! Shared by `delivery` (text insertion), the fn-key hotkey tap, and the pill
//! focus helpers. Keyboard posting goes through CoreGraphics (`CGEvent`);
//! clipboard, frontmost-app identity, and app activation go through AppKit
//! (`NSPasteboard` / `NSWorkspace` / `NSRunningApplication`).
//!
//! IMPORTANT: posting `CGEvent`s requires the macOS **Accessibility** permission.
//! Without it the OS silently drops the events (no error, no insertion). The
//! permission onboarding is covered by the current architecture docs.
#![cfg(target_os = "macos")]

use crate::macos::ax::{self, AxOptions, El};
use std::time::Duration;

pub use crate::desktop::macos::{
    accessibility_trusted, activate_pid, frontmost, frontmost_fast, frontmost_pid,
    frontmost_window_identity, get_clipboard, paste_combo, paste_combo_tagged, post_key,
    press_return, promote_thread_qos, set_clipboard, type_unicode, KEY_DELETE, KEY_RETURN, KEY_V,
};

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFHash(cf: *const std::ffi::c_void) -> usize;
}

const AX_MESSAGING_TIMEOUT: Duration = Duration::from_millis(100);

fn ax_options() -> AxOptions {
    AxOptions::with_timeout(AX_MESSAGING_TIMEOUT)
}

/// An element-valued attribute of `element`, with a short messaging timeout.
fn ax_element_attr(element: &El, name: &str) -> Option<El> {
    let child = element.el_attr(name)?;
    child.set_timeout(AX_MESSAGING_TIMEOUT);
    Some(child)
}

fn ax_hash(element: &El) -> usize {
    unsafe { CFHash(element.as_raw()) }
}

/// Stable, privacy-free identity for the exact macOS focused target. It mixes
/// AX's focused-element hash with its focused-window hash, so changing fields or
/// windows within one process fails pre-Enter validation. It intentionally
/// avoids raw AX pointers: copied AX references need not have same address.
pub fn focused_target_identity() -> Option<isize> {
    let system = ax::system_wide_with(&ax_options()).ok()?;
    let focused = ax_element_attr(&system, "AXFocusedUIElement")?;
    let window = ax_element_attr(&focused, "AXWindow")?;
    let window_hash = ax_hash(&window) as u64;
    let element_hash = ax_hash(&focused) as u64;
    Some((window_hash.rotate_left(17) ^ element_hash) as isize)
}

/// Stable, content-free identity for frontmost app's focused window.
/// Chromium may withhold focused element until its AX tree is activated, but
/// still exposes focused window. Keep this read-only so browser input is never
/// captured or blocked by this app.
pub fn focused_window_identity(pid: i32) -> Option<isize> {
    let app = El::app_with(pid, &ax_options()).ok()?;
    let window = ax_element_attr(&app, "AXFocusedWindow")
        .or_else(|| ax_element_attr(&app, "AXMainWindow"))?;
    Some(ax_hash(&window) as isize)
}

/// Field-content-free title of frontmost app's focused window. Unlike reading
/// title through focused field, this still works for empty Safari web editors.
pub fn focused_window_title(pid: i32) -> Option<String> {
    let app = El::app_with(pid, &ax_options()).ok()?;
    let window = ax_element_attr(&app, "AXFocusedWindow")
        .or_else(|| ax_element_attr(&app, "AXMainWindow"))?;
    let title = window.str_attr("AXTitle")?;
    Some(title.trim().to_string()).filter(|value| !value.is_empty())
}

/// Native macOS fullscreen truth for the frontmost app's focused window.
///
/// `AXFullScreen` tracks AppKit fullscreen state without inferring it from
/// window geometry. `None` means the target app does not expose the attribute;
/// callers may then use a bounded compatibility fallback.
pub fn focused_window_is_fullscreen(pid: i32) -> Option<bool> {
    let app = El::app_with(pid, &ax_options()).ok()?;
    let window = ax_element_attr(&app, "AXFocusedWindow")
        .or_else(|| ax_element_attr(&app, "AXMainWindow"))?;
    window.bool_attr("AXFullScreen")
}

/// Best-effort: is the focused UI element an editable text field?
///   Some(true)  — paste will land (text role, or a settable AXValue)
///   Some(false) — nothing focused / a known non-text control → show the transcript
///   None        — couldn't tell → caller should paste (don't lose the text)
///
/// Read-only by design: probing a browser must never change its accessibility
/// mode. Chromium, Claude, and ChatGPT can expose a lazy/incomplete tree while
/// their window is becoming ready. Retry those transient results briefly, with
/// a hard bound, so a cold AX tree does not become a false non-editable target.
pub fn focused_text_input() -> Option<bool> {
    const RETRY_COUNT: usize = 3;
    const RETRY_DELAY_MS: u64 = 40;

    let first = focused_text_input_once();
    if first == Some(true) {
        return first;
    }

    // A definitive false can be transient for lazy browser trees, while None
    // means the focused element was not available yet. Keep the most recent
    // result, but prefer true as soon as AX exposes the editor.
    let mut result = first;
    for _ in 0..RETRY_COUNT {
        std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
        match focused_text_input_once() {
            Some(true) => return Some(true),
            next => result = next,
        }
    }
    result
}

fn focused_text_input_once() -> Option<bool> {
    const TEXT_ROLES: [&str; 4] = ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];
    // System-wide focused element. No AX attribute is ever written here.
    let system = ax::system_wide_with(&ax_options()).ok()?;
    let mut focused = ax_element_attr(&system, "AXFocusedUIElement");
    if focused.is_none() {
        // System-wide focus follows the key window. A frontmost app whose
        // window is not key (measured on ChatGPT/Codex after a scripted
        // activation) still reports its focused control at the app
        // level; read that before giving up, as the pin and the
        // secure-field probe already do.
        if let Some(pid) = crate::desktop::macos::frontmost().map(|f| f.pid) {
            if let Ok(app) = El::app_with(pid, &ax_options()) {
                focused = ax_element_attr(&app, "AXFocusedUIElement");
            }
        }
    }
    let Some(focused) = focused else {
        rightkit_dictation_telemetry::telemetry::log_message(
            "focused_text_input",
            "no focused element -> unknown",
        );
        return None;
    };
    // Role-based decision; fall back to settable-AXValue for web/custom editors.
    let role_str = focused.str_attr("AXRole").unwrap_or_default();
    // The reliable editable signal (Wispr's approach): a real text input exposes
    // a SETTABLE AXValue or AXSelectedText — true even for web/Electron editors
    // whose role is AXTextArea/AXTextField OR a non-standard contenteditable.
    let editable = TEXT_ROLES.contains(&role_str.as_str())
        || focused.settable("AXValue")
        || focused.settable("AXSelectedText");
    // Definitive: editable → paste; otherwise (button, web container, image,
    // nothing useful) → no field → show the transcript popup.
    rightkit_dictation_telemetry::telemetry::log_message(
        "focused_text_input",
        &format!("role={role_str:?} editable={editable}"),
    );
    Some(editable)
}
