//! The file-watch seam. The host owns the OS watchers and the event channel to the renderer;
//! the workspace only decides which paths to watch.

use std::path::Path;

/// Document and folder watching as the host provides it.
pub trait WorkspaceWatcher {
    /// Watch these (already authorized) files, replacing any previous file watch. Emission must
    /// stay gated on the host's live grants.
    fn watch_files(&self, paths: &[String]) -> Result<(), String>;
    /// Drop the file watcher entirely (nothing left to watch).
    fn clear_files(&self);
    /// Watch a canonicalized folder non-recursively; `display_dir` is the string the renderer
    /// passed and what change events must carry back.
    fn watch_dir(&self, canon_dir: &Path, display_dir: &str) -> Result<(), String>;
    /// Stop watching the folders registered under any of these keys. Idempotent.
    fn unwatch_dirs(&self, keys: &[String]);
}
