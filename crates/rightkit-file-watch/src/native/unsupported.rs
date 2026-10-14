//! Hosts without a native backend (`WatchBackend::Native` is rejected before reaching here).

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
    _root: &Path,
    _options: &TypedOptions,
    _sink: BackendSink,
) -> Result<Handle, WatchError> {
    Err(WatchError::Backend {
        message: "no native watch backend on this platform".into(),
        paths: Vec::new(),
    })
}

pub(crate) fn current_cursor(_path: &Path) -> Option<WatchCursor> {
    None
}
