//! Durable ledger bytes. Failures are reported, never swallowed: a pass that
//! cannot persist its progress must not advance the change token.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub trait LedgerStorage: Send + Sync {
    fn load(&self) -> Result<Option<Vec<u8>>, String>;
    fn save(&self, bytes: &[u8]) -> Result<(), String>;
}

/// Atomic write (temp file + rename) to one JSON file.
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
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

/// Clonable in-memory storage; clones share bytes (simulates an app restart).
#[derive(Clone, Default)]
pub struct MemoryStorage(Arc<Mutex<Option<Vec<u8>>>>);

impl LedgerStorage for MemoryStorage {
    fn load(&self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(bytes.to_vec());
        Ok(())
    }
}
