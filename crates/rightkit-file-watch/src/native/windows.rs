//! Windows native backend: STUB.
//!
//! Finding (0.4.1): notify 8.2.0's Windows backend already uses `ReadDirectoryChangesW`, so this
//! stub is not needed for ordinary watching and `Auto` keeps using notify. What notify does not
//! do is report a buffer overflow: it allocates a fixed 16 KiB buffer per watch and its
//! completion routine ignores the byte count (a successful completion with 0 bytes is how
//! Windows signals overflow), so no `Flag::Rescan` event ever reaches this crate. Only this
//! native arm can deliver that signal on Windows. The stub is referenced by the platform seam
//! in `native.rs`, so it stays until that arm is written.
//! The real `ReadDirectoryChangesW` arm is another lane's work
//! (design lane W). Until it lands, `WatchBackend::Native` returns `WatchError::Backend` and
//! `WatchBackend::Auto` uses notify on Windows (`AUTO_IS_NATIVE_ON_WINDOWS` in `native.rs`).
//!
//! What the lane gets from the common seam: a `BackendSink` per watch (`event`, `expire`,
//! `next_deadline`, `commit`, `finish`) and `TypedOptions::windows_buffer` /
//! `windows_network_buffer`. Windows has no resume cursor: leave `Handle::volume` as `None`.

use std::path::Path;

use super::{BackendSink, TypedOptions, WatchCursor};
use crate::WatchError;

pub(crate) struct Handle;

impl Handle {
    pub(crate) fn volume(&self) -> Option<[u8; 16]> {
        None
    }
}

pub(crate) fn start(
    root: &Path,
    options: &TypedOptions,
    sink: BackendSink,
) -> Result<Handle, WatchError> {
    // Referenced so the seam stays exercised by the compiler until the lane lands.
    let _ = (
        options.windows_buffer(),
        options.windows_network_buffer(),
        sink.queue(),
    );
    Err(WatchError::Backend {
        message:
            "the Windows native watch backend is not implemented yet; use WatchBackend::Notify"
                .into(),
        paths: vec![root.to_path_buf()],
    })
}

pub(crate) fn current_cursor(_path: &Path) -> Option<WatchCursor> {
    None
}
