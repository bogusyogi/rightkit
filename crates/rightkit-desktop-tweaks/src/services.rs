//! What the keyboard layer needs from the application: the screenshot service, the snippet
//! keyword service, the tool wheel's mouse handler, the copy burst, the strip's focus state, user
//! switches and logging. The application installs one [`Services`]; until it does, every hook
//! passes through.

use crate::keys::ScreenshotRequest;
use rightkit_hotkeys::MouseEvent;
use std::sync::OnceLock;

/// Services the embedding application provides.
pub trait Services: Send + Sync + 'static {
    /// Whether the named user switch is on.
    fn switch_on(&self, _key: &str) -> bool {
        false
    }

    /// Records an informational event.
    fn log_info(&self, _event: &str, _fields: &[(&str, &str)]) {}

    /// Starts the screenshot thread; the returned value stops it when dropped.
    fn screenshot_start(&self) -> Option<Box<dyn std::any::Any + Send>> {
        None
    }

    /// Whether a screenshot session is open.
    fn screenshot_active(&self) -> bool {
        false
    }

    /// Escape was pressed during a screenshot session.
    fn screenshot_key_escape(&self) {}

    /// Space was pressed during a screenshot session.
    fn screenshot_key_space(&self) {}

    /// Asks for a screenshot picker.
    fn screenshot_request(&self, _request: ScreenshotRequest) {}

    /// A left click happened (snippet keywords clear their buffer).
    fn snippets_clicked(&self) {}

    /// A physical key went down (snippet keywords see it).
    fn snippets_key_down(&self, _vk: u32, _scan: u32) {}

    /// The tool wheel's mouse handler; true swallows the event.
    fn wheel_mouse_event(&self, _event: &MouseEvent) -> bool {
        false
    }

    /// The V key came up while a copy burst may be active.
    fn burst_v_up(&self) {}

    /// Whether a copy burst is active.
    fn burst_active(&self) -> bool {
        false
    }

    /// Pastes the next burst item; true when the paste key was consumed.
    fn burst_paste_key(&self) -> bool {
        false
    }

    /// Whether the clipboard strip is the foreground window.
    fn strip_is_foreground(&self) -> bool {
        false
    }
}

static SERVICES: OnceLock<Box<dyn Services>> = OnceLock::new();

/// Installs the services. Returns `false` when some were already installed.
pub fn install(services: Box<dyn Services>) -> bool {
    SERVICES.set(services).is_ok()
}

pub(crate) fn switch_on(key: &str) -> bool {
    SERVICES.get().is_some_and(|s| s.switch_on(key))
}

pub(crate) fn log_info(event: &str, fields: &[(&str, &str)]) {
    if let Some(s) = SERVICES.get() {
        s.log_info(event, fields);
    }
}

pub(crate) fn screenshot_start() -> Option<Box<dyn std::any::Any + Send>> {
    SERVICES.get()?.screenshot_start()
}

pub(crate) fn screenshot_active() -> bool {
    SERVICES.get().is_some_and(|s| s.screenshot_active())
}

pub(crate) fn screenshot_key_escape() {
    if let Some(s) = SERVICES.get() {
        s.screenshot_key_escape();
    }
}

pub(crate) fn screenshot_key_space() {
    if let Some(s) = SERVICES.get() {
        s.screenshot_key_space();
    }
}

pub(crate) fn screenshot_request(request: ScreenshotRequest) {
    if let Some(s) = SERVICES.get() {
        s.screenshot_request(request);
    }
}

pub(crate) fn snippets_clicked() {
    if let Some(s) = SERVICES.get() {
        s.snippets_clicked();
    }
}

pub(crate) fn snippets_key_down(vk: u32, scan: u32) {
    if let Some(s) = SERVICES.get() {
        s.snippets_key_down(vk, scan);
    }
}

pub(crate) fn wheel_mouse_event(event: &MouseEvent) -> bool {
    SERVICES.get().is_some_and(|s| s.wheel_mouse_event(event))
}

pub(crate) fn burst_v_up() {
    if let Some(s) = SERVICES.get() {
        s.burst_v_up();
    }
}

pub(crate) fn burst_active() -> bool {
    SERVICES.get().is_some_and(|s| s.burst_active())
}

pub(crate) fn burst_paste_key() -> bool {
    SERVICES.get().is_some_and(|s| s.burst_paste_key())
}

pub(crate) fn strip_is_foreground() -> bool {
    SERVICES.get().is_some_and(|s| s.strip_is_foreground())
}
