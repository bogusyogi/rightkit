//! Rich clipboard for the shell: the typed "what did the person copy" contract for paste and
//! open-from-clipboard, over the runtime-neutral `rightkit-clipboard` (rightkit#956), which owns the
//! raw transaction API (text, HTML, PNG, file lists and app-defined formats on macOS and Windows).
//!
//! Donors: HeardRight `clipboard` + delivery (text/HTML/image, paste-sequence discipline),
//! ViewRight file grants (paths, not bytes) and ViewRight `commands/clipboard.rs` (typed
//! files/image/text/empty result, CF_DIB to BMP, temp-file grants, one-hour purge).
//!
//! * [`read`] / [`write()`] are the raw transaction API, re-exported from `rightkit-clipboard`.
//! * [`take_item`] is the typed contract the `rk_shell_clipboard_item` command returns. Priority is
//!   files, then a bitmap (written to a granted temp file), then text, then nothing. Files and
//!   temp images are granted through the [`PathBroker`]; nothing else reaches the renderer.
//!   [`item_from_snapshot`] runs the same policy on any snapshot, so it is testable on every host.
//! * [`take_item_from_with`] is [`take_item_with`] for an explicit [`Board`]. A `Board::Named` read
//!   touches only that private pasteboard (macOS); Windows has no private clipboard, so a named
//!   board is rejected there.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::broker::PathBroker;
use crate::error::ShellError;

use rightkit_clipboard::platform_snapshot;
pub use rightkit_clipboard::{
    clipboard_temp_dir, dib_to_bmp, formats, named_board_refusal, purge_stale, read, read_with,
    write, Board, ClipboardContent, ClipboardImage, ClipboardOptions, ClipboardSnapshot,
    ImagePreference, DEFAULT_PURGE_AFTER,
};

/// Typed result of reading the clipboard for an "open" or "paste" action. Serialises as
/// `{"kind": "files" | "image" | "text" | "none", ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ClipboardItem {
    /// Files copied in the file manager. Each path is granted to the broker.
    Files { paths: Vec<String> },
    /// A bitmap, written to a granted temp file (`.png` when the clipboard had PNG, else `.bmp`).
    Image { path: String },
    /// Plain Unicode text.
    Text { text: String },
    /// Nothing usable.
    None,
}

/// Clipboard temp-file policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicy {
    /// Temp files older than this are removed before a new one is written.
    pub purge_after: Duration,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            purge_after: DEFAULT_PURGE_AFTER,
        }
    }
}

/// Read the clipboard and apply the priority policy: files, then a bitmap, then text.
pub fn take_item(
    broker: &PathBroker,
    temp_dir: &Path,
    policy: ClipboardPolicy,
) -> Result<ClipboardItem, ShellError> {
    take_item_with(broker, temp_dir, policy, &ClipboardOptions::default())
}

/// [`take_item`] with explicit [`ClipboardOptions`] (for example `DibFirst` on Windows). Reads the
/// general clipboard: it is [`take_item_from_with`] for `Board::General`.
pub fn take_item_with(
    broker: &PathBroker,
    temp_dir: &Path,
    policy: ClipboardPolicy,
    options: &ClipboardOptions,
) -> Result<ClipboardItem, ShellError> {
    take_item_from_with(&Board::General, broker, temp_dir, policy, options)
}

/// [`take_item_with`] for an explicit [`Board`].
///
/// `Board::General` is the user's clipboard and behaves exactly as [`take_item_with`].
/// `Board::Named` reads only that private pasteboard (macOS): the AppleScript fallbacks for a file
/// URL without a path and for a PNG coerced from a TIFF are skipped, because AppleScript can only
/// see the general clipboard, and no failure or empty result falls through to it. A missing
/// representation yields [`ClipboardItem::None`] or an error for the named board, never the user's
/// clipboard. A name in the `Apple CFPasteboard ...` family (the system boards, including the
/// general one) is rejected. On Windows and other platforms a named board is rejected with the
/// reader's error, since there is no private clipboard.
pub fn take_item_from_with(
    board: &Board,
    broker: &PathBroker,
    temp_dir: &Path,
    policy: ClipboardPolicy,
    options: &ClipboardOptions,
) -> Result<ClipboardItem, ShellError> {
    let snapshot = platform_snapshot(board, temp_dir, options).map_err(ShellError::from)?;
    item_from_snapshot(&snapshot, broker, temp_dir, policy)
}

/// QA-only control-command helpers for private pasteboards (feature `qa-probes`).
#[cfg(feature = "qa-probes")]
pub mod qa;

/// Apply the priority policy to a snapshot: grant the files that exist, write a bitmap to a granted
/// temp file, otherwise return non-empty text. Old temp files are purged first.
pub fn item_from_snapshot(
    snapshot: &ClipboardSnapshot,
    broker: &PathBroker,
    temp_dir: &Path,
    policy: ClipboardPolicy,
) -> Result<ClipboardItem, ShellError> {
    if temp_dir.is_dir() {
        purge_stale(temp_dir, policy.purge_after);
    }
    let granted: Vec<String> = snapshot
        .files
        .iter()
        .filter_map(|p| broker.grant_file(p).ok())
        .collect();
    if !granted.is_empty() {
        return Ok(ClipboardItem::Files { paths: granted });
    }
    if let Some(image) = snapshot.image.as_ref().filter(|i| !i.bytes.is_empty()) {
        let path = write_temp(temp_dir, &image.extension, &image.bytes)?;
        return match broker.grant_file(&path) {
            Ok(display) => Ok(ClipboardItem::Image { path: display }),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        };
    }
    match snapshot.text.as_deref() {
        Some(text) if !text.is_empty() => Ok(ClipboardItem::Text {
            text: text.to_owned(),
        }),
        _ => Ok(ClipboardItem::None),
    }
}

/// Write `bytes` to a fresh `clipboard-<stamp>.<ext>` file in `dir` (created if needed).
fn write_temp(dir: &Path, extension: &str, bytes: &[u8]) -> Result<PathBuf, ShellError> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let ext: String = extension
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    for _ in 0..8 {
        let path = dir.join(format!("clipboard-{}.{ext}", unique_stamp()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ShellError::Other(
        "could not create a clipboard temp file".into(),
    ))
}

fn unique_stamp() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}
