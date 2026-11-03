//! Where the data lives, as the host decides it.
use std::path::PathBuf;

use crate::host::host;
pub use crate::host::DirKind;

/// The data folder and how it was chosen, or the host's refusal.
pub(crate) fn data_dir_checked() -> Result<(PathBuf, DirKind), String> {
    host().data_dir_checked()
}

/// The resolved data directory; the database and the logs live here. After a refusal it is a
/// per-process temp folder, never the real one.
pub fn data_dir() -> PathBuf {
    match data_dir_checked() {
        Ok((dir, _)) => dir,
        Err(_) => std::env::temp_dir().join(format!(
            "{}-no-data-dir-{}",
            host().app_slug(),
            std::process::id()
        )),
    }
}
