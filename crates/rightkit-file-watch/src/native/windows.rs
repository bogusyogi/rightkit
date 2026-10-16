//! Windows native backend: STUB. The real `ReadDirectoryChangesW` arm is another lane's work
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
