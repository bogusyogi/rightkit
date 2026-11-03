//! What the notch drawing code needs from the application: user switches, the weekly ring
//! choice, shell thumbnails and translation. The application installs one [`Host`]; until it
//! does, switches are off, the weekly ring is inside, there are no thumbnails and text is
//! English.

use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock, PoisonError};

/// Where the second (weekly) ring sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeeklyRing {
    /// No weekly ring.
    Off,
    /// Inside the main ring.
    Inside,
    /// Outside the main ring.
    Outside,
}

/// Services the embedding application provides.
pub trait Host: Send + Sync + 'static {
    /// Whether the named user switch is on.
    fn switch_on(&self, _key: &str) -> bool {
        false
    }

    /// Where the weekly ring is drawn.
    fn weekly_ring(&self) -> WeeklyRing {
        WeeklyRing::Inside
    }

    /// A file's picture for a card tile (`large`) or a shelf row, when the shell has one.
    #[cfg(windows)]
    fn thumbnail(
        &self,
        _path: &str,
        _large: bool,
    ) -> Option<std::sync::Arc<rightkit_win_ui::fileicon::Icon>> {
        None
    }

    /// The label of the menu row that quits the application, such as "Quit App".
    fn quit_label(&self) -> String {
        "Quit".to_string()
    }

    /// The text for `key` in the user's language.
    fn translate(&self, key: &str) -> String {
        key.to_string()
    }
}

static HOST: OnceLock<Box<dyn Host>> = OnceLock::new();

/// Installs the host. Returns `false` when one was already installed (the first one stays).
pub fn install(host: Box<dyn Host>) -> bool {
    HOST.set(host).is_ok()
}

pub(crate) fn switch_on(key: &str) -> bool {
    HOST.get().is_some_and(|host| host.switch_on(key))
}

#[cfg(windows)]
pub(crate) fn weekly_ring() -> WeeklyRing {
    HOST.get()
        .map_or(WeeklyRing::Inside, |host| host.weekly_ring())
}

#[cfg(windows)]
pub(crate) fn thumbnail(
    path: &str,
    large: bool,
) -> Option<std::sync::Arc<rightkit_win_ui::fileicon::Icon>> {
    HOST.get()?.thumbnail(path, large)
}

/// Whether the path names a picture the shell decodes itself.
#[cfg(windows)]
pub(crate) fn is_image_file(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff"
            )
        })
}

/// The text for `key`, translated by the host when one is installed. Each distinct translation
/// is kept for the life of the process (the catalog is finite), so the result is `'static`.
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

pub(crate) fn quit_label() -> String {
    HOST.get()
        .map_or_else(|| "Quit".to_string(), |host| host.quit_label())
}
