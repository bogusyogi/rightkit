//! Durable ledger bytes. Failures are reported, never swallowed: a pass that
//! cannot persist its progress must not advance the change token.

use std::path::PathBuf;

pub trait LedgerStorage: Send + Sync {
    fn load(&self) -> Result<Option<Vec<u8>>, String>;
    fn save(&self, bytes: &[u8]) -> Result<(), String>;
}

/// Atomic, durable, owner-only write (temp file, fsync, rename, fsync directory) to one file,
/// through `rightkit_fs::atomic_write_private`.
pub struct FileStorage {
    path: PathBuf,
}

impl FileStorage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl LedgerStorage for FileStorage {
    fn load(&self) -> Result<Option<Vec<u8>>, String> {
        match std::fs::read(&self.path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        rightkit_fs::atomic_write_private(&self.path, bytes).map_err(|e| e.to_string())
    }
}
