//! The system clipboard as raw data: every format's bytes, a change token, and a change
//! listener. (Staging text for a paste with exact generation tracking is the `desktop`
//! module's job; this module is for hosts that watch or archive the clipboard.)
//!
//! * [`sequence`]: the OS change counter. It moves on every write by any app, so two equal
//!   values mean nothing was written in between.
//! * [`read_raw`]: each format the clipboard holds, by name, with its bytes. Windows names are
//!   the `CF_*` name, the registered format name, or `format:N`; macOS names are the pasteboard
//!   types (UTIs). Handle-only formats (GDI bitmaps, metafiles, palettes) carry no bytes and are
//!   listed under `skipped`, as is anything over the limits.
//! * [`watch`]: calls back with the new sequence on every change. Windows uses
//!   `AddClipboardFormatListener` on a message-only window; macOS has no change notification, so
//!   the watcher reads the change count every 100 ms on its own thread. The callback runs on that
//!   thread and must not block. Dropping the [`ClipboardWatcher`] stops it and joins the thread.
//!
//! **Private content is withheld.** When the copying app marked the content as not for
//! monitors (Windows `ExcludeClipboardContentFromMonitorProcessing`, or a `CanIncludeInClipboardHistory`
//! / `CanUploadToCloudClipboard` of zero; macOS `org.nspasteboard.ConcealedType` or 1Password's
//! marker), [`read_raw`] returns `concealed: true` with the format names under `skipped` and no
//! bytes. Password managers set these; a clipboard history that stored the bytes would defeat them.
//!
//! Like the other raw primitives here, reading refuses while the kill switch is set and writes no
//! receipt. A host with an admission hook asks it before calling.

use crate::model::{Error, Result};

/// Size limits for [`read_raw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawLimits {
    /// Most bytes copied from one format; a longer one is cut and marked `truncated`.
    pub max_format_bytes: usize,
    /// Most bytes copied across all formats; later formats are listed under `skipped`.
    pub max_total_bytes: usize,
}

impl Default for RawLimits {
    fn default() -> Self {
        RawLimits {
            max_format_bytes: 16 * 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

/// One clipboard format and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFormat {
    pub name: String,
    pub bytes: Vec<u8>,
    /// The format held more than `max_format_bytes`; `bytes` is the leading part.
    pub truncated: bool,
}

/// A full read of the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawClipboard {
    /// The change token the read is consistent with.
    pub sequence: u64,
    pub formats: Vec<RawFormat>,
    /// Formats present but not copied, each with the reason.
    pub skipped: Vec<String>,
    /// The copying app marked the content private; no bytes were read.
    pub concealed: bool,
}

/// The clipboard change token. `0` on an OS without a backend.
pub fn sequence() -> u64 {
    #[cfg(windows)]
    {
        crate::win::clip::sequence()
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::pasteboard::sequence()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        0
    }
}

/// Read every format on the clipboard. See the module docs for what is withheld.
pub fn read_raw(limits: &RawLimits) -> Result<RawClipboard> {
    if crate::policy::killed() {
        return Err(Error::Blocked("kill switch engaged".into()));
    }
    #[cfg(windows)]
    {
        crate::win::clip::read_raw(limits)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::pasteboard::read_raw(limits)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = limits;
        Err(Error::Unsupported("clipboard read on this platform"))
    }
}

/// A running clipboard change listener. Dropping it stops the listener.
pub struct ClipboardWatcher {
    #[cfg(windows)]
    _inner: crate::win::clip::Watcher,
    #[cfg(target_os = "macos")]
    _inner: crate::macos::pasteboard::Watcher,
}

/// Call `on_change(sequence)` after every clipboard change until the returned watcher is dropped.
/// The callback runs on the watcher's own thread, so it must be quick; a panic in it is caught
/// and ignored. Refuses while the kill switch is set.
pub fn watch<F>(on_change: F) -> Result<ClipboardWatcher>
where
    F: Fn(u64) + Send + 'static,
{
    if crate::policy::killed() {
        return Err(Error::Blocked("kill switch engaged".into()));
    }
    #[cfg(windows)]
    {
        Ok(ClipboardWatcher {
            _inner: crate::win::clip::watch(Box::new(on_change))?,
        })
    }
    #[cfg(target_os = "macos")]
    {
        Ok(ClipboardWatcher {
            _inner: crate::macos::pasteboard::watch(Box::new(on_change))?,
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = on_change;
        Err(Error::Unsupported("clipboard watch on this platform"))
    }
}
