//! The loop's records on disk: [`AssistState`] in `<dir>/state.json` and the poll record in `<dir>/poll.json`, each replaced
//! atomically and readable by the owner only. One process-wide lock is held for the length of one read or write of either file.
//!
//! The directory is the host's choice (it passes it in). A file that cannot be read is an error and is never overwritten.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::state::{AssistState, AssistStore};

/// The state file's name inside the records directory.
pub const STATE_FILE: &str = "state.json";
/// The poll record's file name inside the records directory.
pub const POLL_FILE: &str = "poll.json";

/// Held for the length of one read or write of a file here.
static FILE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn lock() -> MutexGuard<'static, ()> {
    FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `Ok(None)` when the file does not exist; an error when it exists and cannot be read or parsed.
pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "a record file could not be read".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Writes `value` to `path` (inside `dir`) atomically, creating `dir` and its parent owner-only.
pub fn write_json<T: Serialize>(dir: &Path, path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if let Some(parent) = dir.parent() {
        rightkit_fs::ensure_private_dir(parent).map_err(|e| e.to_string())?;
    }
    rightkit_fs::ensure_private_dir(dir).map_err(|e| e.to_string())?;
    rightkit_fs::atomic_write_private(path, &bytes).map_err(|e| e.to_string())
}

/// `AssistState` in `<dir>/state.json`: loaded, changed and written back under one lock.
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: impl Into<PathBuf>) -> FileStore {
        FileStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl AssistStore for FileStore {
    fn update<R>(&self, f: impl FnOnce(&mut AssistState) -> R) -> Result<R, String> {
        let _guard = lock();
        let path = self.dir.join(STATE_FILE);
        let mut state: AssistState = read_json(&path)?.unwrap_or_default();
        let out = f(&mut state);
        write_json(&self.dir, &path, &state)?;
        Ok(out)
    }

    fn snapshot(&self) -> Result<AssistState, String> {
        let _guard = lock();
        Ok(read_json(&self.dir.join(STATE_FILE))?.unwrap_or_default())
    }
}
