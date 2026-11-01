//! What the clipboard history needs from the application that embeds it: its data folder, the
//! user's limits and switches, translation, the application version and event logging. The
//! application installs one [`Host`] at start-up; until it does the history has no folder, uses
//! the default limits, English text and no logging.

use std::path::PathBuf;
use std::sync::OnceLock;

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

    /// Records an informational event with `fields`. Never called with clipboard content.
    fn log_info(&self, _event: &str, _fields: &[(&str, &str)]) {}
}

static HOST: OnceLock<Box<dyn Host>> = OnceLock::new();

/// Installs the host. Returns `false` when one was already installed (the first one stays).
pub fn install(host: Box<dyn Host>) -> bool {
    HOST.set(host).is_ok()
}

/// The text for `key`, translated by the host when one is installed.
pub fn t(key: &str) -> String {
    HOST.get()
        .map_or_else(|| key.to_string(), |host| host.translate(key))
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
