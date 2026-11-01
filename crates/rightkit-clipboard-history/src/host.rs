//! What the clipboard history needs from the application that embeds it: its data folder, the
//! user's limits and switches, translation, the application version and event logging. The
//! application installs one [`Host`] at start-up; until it does the history has no folder, uses
//! the default limits, English text and no logging.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

/// How much history is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    /// Most items kept.
    pub max_items: u32,
    /// Most megabytes of payload kept.
    pub max_mb: u32,
    /// Days main-lane items are kept; zero keeps them for ever.
    pub max_age_days: u32,
    /// Hours screenshots are kept; zero keeps them until another cap.
    pub screenshot_hours: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            max_items: 500,
            max_mb: 200,
            max_age_days: 30,
            screenshot_hours: 24,
        }
    }
}

/// Services the embedding application provides.
pub trait Host: Send + Sync + 'static {
    /// The folder the history lives in; `None` disables the on-disk store.
    fn data_dir(&self) -> Option<PathBuf>;

    /// The retention limits the user chose.
    fn retention(&self) -> Retention {
        Retention::default()
    }

    /// Whether the named user switch is on. `clipboardEncryption` is asked for by [`crate::seal`].
    fn switch_on(&self, _key: &str) -> bool {
        true
    }

    /// The text for `key` in the user's language; the key itself when there is no translation.
    fn translate(&self, key: &str) -> String {
        key.to_string()
    }

    /// The application version, written into the index as the writer.
    fn app_version(&self) -> String {
        "0".to_string()
    }

    /// Name of the clipboard format that marks the application's own writes.
    fn own_write_format(&self) -> String {
        "RightKitOwnWrite".to_string()
    }

    /// Name of the temporary folder (under the system temp directory) for copies a drag made.
    fn drag_copy_dir(&self) -> String {
        "RightKitClip".to_string()
    }

    /// Lower-case executable names whose copies are never kept.
    fn excluded_apps(&self) -> Vec<String> {
        Vec::new()
    }

    /// A copy was just stored under `id`.
    fn copied(&self, _id: &str) {}

    /// The stored history changed (the application may refresh counts).
    fn store_changed(&self) {}

    /// True when choosing an item only copies it (the "copy" paste mode).
    fn paste_copies_only(&self) -> bool {
        false
    }

    /// Lower-case executable names of terminals that paste with Ctrl+Shift+V.
    fn shift_paste_apps(&self) -> Vec<String> {
        Vec::new()
    }

    /// Shows a short notice (a toast) with `title`, translated by the host; `off` marks the
    /// "history is off" variant.
    fn note(&self, _title: &str, _off: bool) {}

    /// Sends `events` as one tagged batch; `ctx` names the sender in the log.
    #[cfg(windows)]
    fn send_input(&self, events: &[rightkit_automation::win::input::RawInput], ctx: &str);

    /// Whether the user physically holds the side-specific modifier `vk`; `None` when unknown.
    #[cfg(windows)]
    fn physically_down(&self, _vk: u32) -> Option<bool> {
        None
    }

    /// The card size the user chose: `small`, `medium`, `large` or empty for medium.
    fn card_size(&self) -> String {
        String::new()
    }

    /// Where the strip opens: `notch`, `bottom` or empty for the notch.
    fn placement(&self) -> String {
        String::new()
    }

    /// The strip's hot key in the clipboard spec's grammar; empty for the default.
    fn hotkey_text(&self) -> String {
        String::new()
    }

    /// The notch under `point` (screen pixels), when the strip can come out of one.
    #[cfg(windows)]
    fn notch_spot_at(&self, _point: (i32, i32)) -> Option<crate::strip::NotchSpot> {
        None
    }

    /// Wakes the application's window thread (the hub's counts follow).
    fn wake_ui(&self) {}

    /// Opens the launcher in its clipboard mode.
    fn open_launcher_clip(&self) {}

    /// Records an informational event with `fields`. Never called with clipboard content.
    fn log_info(&self, _event: &str, _fields: &[(&str, &str)]) {}
}

static HOST: OnceLock<Box<dyn Host>> = OnceLock::new();

/// Installs the host. Returns `false` when one was already installed (the first one stays).
pub fn install(host: Box<dyn Host>) -> bool {
    HOST.set(host).is_ok()
}

/// The text for `key`, translated by the host when one is installed. Each distinct translation is
/// kept for the life of the process (the catalog is finite), so the result is `'static`.
pub fn t(key: &str) -> &'static str {
    static KEPT: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let text = HOST
        .get()
        .map_or_else(|| key.to_string(), |host| host.translate(key));
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(found) = kept.get(text.as_str()) {
        return found;
    }
    let leaked: &'static str = Box::leak(text.into_boxed_str());
    kept.insert(leaked);
    leaked
}

pub(crate) fn data_dir() -> Option<PathBuf> {
    HOST.get()?.data_dir()
}

pub(crate) fn retention() -> Retention {
    HOST.get()
        .map_or_else(Retention::default, |host| host.retention())
}

pub(crate) fn switch_on(key: &str) -> bool {
    HOST.get().is_none_or(|host| host.switch_on(key))
}

pub(crate) fn own_write_format() -> String {
    HOST.get().map_or_else(
        || "RightKitOwnWrite".to_string(),
        |host| host.own_write_format(),
    )
}

pub(crate) fn drag_copy_dir() -> String {
    HOST.get()
        .map_or_else(|| "RightKitClip".to_string(), |host| host.drag_copy_dir())
}

pub(crate) fn app_version() -> String {
    HOST.get()
        .map_or_else(|| "0".to_string(), |host| host.app_version())
}

pub(crate) fn log_info(event: &str, fields: &[(&str, &str)]) {
    if let Some(host) = HOST.get() {
        host.log_info(event, fields);
    }
}

pub(crate) fn excluded_apps() -> Vec<String> {
    HOST.get()
        .map_or_else(Vec::new, |host| host.excluded_apps())
}

#[cfg(windows)]
pub(crate) fn log_error(op: &str, error: &windows::core::Error, ctx: &str) {
    log_info(ctx, &[("operation", op), ("error", &error.message())]);
}

#[cfg(windows)]
pub(crate) fn copied(id: &str) {
    if let Some(host) = HOST.get() {
        host.copied(id);
    }
}

#[cfg(windows)]
pub(crate) fn store_changed() {
    if let Some(host) = HOST.get() {
        host.store_changed();
    }
}

#[cfg(windows)]
pub(crate) fn paste_copies_only() -> bool {
    HOST.get().is_some_and(|host| host.paste_copies_only())
}

#[cfg(windows)]
pub(crate) fn shift_paste_apps() -> Vec<String> {
    HOST.get()
        .map_or_else(Vec::new, |host| host.shift_paste_apps())
}

#[cfg(windows)]
pub(crate) fn note(title: &str, off: bool) {
    if let Some(host) = HOST.get() {
        host.note(title, off);
    }
}

#[cfg(windows)]
pub(crate) fn send_input(events: &[rightkit_automation::win::input::RawInput], ctx: &str) {
    if let Some(host) = HOST.get() {
        host.send_input(events, ctx);
    }
}

#[cfg(windows)]
pub(crate) fn physically_down(vk: u32) -> Option<bool> {
    HOST.get()?.physically_down(vk)
}

/// The translated template for `key` with each `{}` filled from `args` in order.
pub fn tf(key: &str, args: &[&str]) -> String {
    let template = t(key);
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0;
    let mut rest = template;
    while let Some(open) = rest.find("{}") {
        out.push_str(&rest[..open]);
        match args.get(next) {
            Some(arg) => out.push_str(arg),
            None => out.push_str("{}"),
        }
        next += 1;
        rest = &rest[open + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(windows)]
pub(crate) fn card_size() -> String {
    HOST.get().map(|host| host.card_size()).unwrap_or_default()
}

#[cfg(windows)]
pub(crate) fn placement() -> String {
    HOST.get().map(|host| host.placement()).unwrap_or_default()
}

#[cfg(windows)]
pub(crate) fn hotkey_text() -> String {
    HOST.get()
        .map(|host| host.hotkey_text())
        .unwrap_or_default()
}

#[cfg(windows)]
pub(crate) fn notch_spot_at(point: (i32, i32)) -> Option<crate::strip::NotchSpot> {
    HOST.get()?.notch_spot_at(point)
}

#[cfg(windows)]
pub(crate) fn wake_ui() {
    if let Some(host) = HOST.get() {
        host.wake_ui();
    }
}

#[cfg(windows)]
pub(crate) fn open_launcher_clip() {
    if let Some(host) = HOST.get() {
        host.open_launcher_clip();
    }
}
