//! Durable ledger bytes. Failures are reported, never swallowed: a pass that
//! cannot persist its progress must not advance the change token.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_storage_round_trips_replaces_and_leaves_no_temp_files() {
        let dir = std::env::temp_dir().join(format!("rk-sync-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileStorage::new(dir.join("nested").join("ledger.json"));
        assert_eq!(store.load().unwrap(), None);
        store.save(b"one").unwrap();
        store.save(b"two").unwrap();
        assert_eq!(store.load().unwrap().as_deref(), Some(&b"two"[..]));
        let names: Vec<_> = std::fs::read_dir(dir.join("nested"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["ledger.json".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
