//! Read-only macOS Accessibility queries for delivery and window presentation.

use std::time::Duration;

use core_foundation::base::TCFType;

use super::ffi::{ax_bool, ax_string, AXUIElementRef};
use super::focus::{focused_target_hash, focused_window_with_timeout};
use crate::macos::ax::{self, AxOptions, El};

/// Timing and diagnostics for focus queries. Diagnostics never include field text.
#[derive(Clone, Copy)]
pub struct FocusReadConfig {
    pub ax_messaging_timeout: Duration,
    /// Additional reads after an initial false or unknown editability result.
    pub text_input_retry_count: usize,
    pub text_input_retry_delay_ms: u64,
    pub event_sink: fn(&str, &str),
}

fn read_element(parent: &El, name: &str, options: &AxOptions) -> Option<El> {
    let element = parent.el_attr(name)?;
    element.set_timeout(options.timeout);
    Some(element)
}

/// Privacy-free identity for the currently focused AX element and its window.
pub fn focused_target_identity(config: &FocusReadConfig) -> Option<isize> {
    let options = AxOptions::with_timeout(config.ax_messaging_timeout);
    let system = ax::system_wide_with(&options).ok()?;
    let focused = read_element(&system, "AXFocusedUIElement", &options)?;
    let window = read_element(&focused, "AXWindow", &options)?;
    Some(unsafe { focused_target_hash(window.as_raw(), focused.as_raw()) })
}

/// Title of the process's focused window, falling back to its main window.
pub fn focused_window_title(config: &FocusReadConfig, pid: i32) -> Option<String> {
    let window = focused_window_with_timeout(pid, config.ax_messaging_timeout.as_secs_f32())?;
    let title = unsafe { ax_string(window.as_CFTypeRef() as AXUIElementRef, "AXTitle") }?;
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// Native fullscreen state. `None` means the window does not expose that state.
pub fn focused_window_is_fullscreen(config: &FocusReadConfig, pid: i32) -> Option<bool> {
    let window = focused_window_with_timeout(pid, config.ax_messaging_timeout.as_secs_f32())?;
    unsafe { ax_bool(window.as_CFTypeRef() as AXUIElementRef, "AXFullScreen") }
}

/// Read editability without changing the target's Accessibility mode.
pub fn focused_text_input(config: &FocusReadConfig) -> Option<bool> {
    let mut result = focused_text_input_once(config);
    for _ in 0..config.text_input_retry_count {
        if result == Some(true) {
            break;
        }
        std::thread::sleep(Duration::from_millis(config.text_input_retry_delay_ms));
        result = focused_text_input_once(config);
    }
    result
}

fn focused_text_input_once(config: &FocusReadConfig) -> Option<bool> {
    const TEXT_ROLES: [&str; 4] = ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];
    let options = AxOptions::with_timeout(config.ax_messaging_timeout);
    let system = ax::system_wide_with(&options).ok()?;
    let focused = read_element(&system, "AXFocusedUIElement", &options).or_else(|| {
        // Some browsers expose the control through the app while their window is not key.
        let pid = super::frontmost()?.pid;
        let app = El::app_with(pid, &options).ok()?;
        read_element(&app, "AXFocusedUIElement", &options)
    });
    let Some(focused) = focused else {
        (config.event_sink)("focused_text_input", "no focused element -> unknown");
        return None;
    };
    let role = focused.str_attr("AXRole").unwrap_or_default();
    let editable = TEXT_ROLES.contains(&role.as_str())
        || focused.settable("AXValue")
        || focused.settable("AXSelectedText");
    (config.event_sink)(
        "focused_text_input",
        &format!("role={role:?} editable={editable}"),
    );
    Some(editable)
}
