use crate::EventRecord;
use serde_json::Value;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug)]
pub struct JsonlStore {
    path: PathBuf,
    max_file_bytes: u64,
    append_lock: Mutex<()>,
}

impl JsonlStore {
    pub fn new(path: impl Into<PathBuf>, max_file_bytes: u64) -> Self {
        Self {
            path: path.into(),
            max_file_bytes,
            append_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, event: &EventRecord) -> io::Result<()> {
        let value = serde_json::to_value(event).map_err(io::Error::other)?;
        self.append_value(&value)
    }

    pub fn append_value(&self, value: &Value) -> io::Result<()> {
        let _guard = self
            .append_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.append_value_locked(value)
    }

    pub fn append_best_effort(&self, event: &EventRecord) {
        if let Err(error) = self.append(event) {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = writeln!(
                stderr,
                "[rightkit-logs] append to {} failed ({error}); event={}",
                self.path.display(),
                event.event
            );
        }
    }

    pub fn clear(&self) -> io::Result<()> {
        let _guard = self
            .append_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_if_present(&self.path)?;
        remove_if_present(&rotated_path(&self.path))
    }

    fn append_value_locked(&self, value: &Value) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.rotate_if_needed()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }

    fn rotate_if_needed(&self) -> io::Result<()> {
        if self.max_file_bytes == 0 {
            return Ok(());
        }
        let Ok(metadata) = fs::metadata(&self.path) else {
            return Ok(());
        };
        if metadata.len() < self.max_file_bytes {
            return Ok(());
        }

        let predecessor = rotated_path(&self.path);
        remove_if_present(&predecessor)?;
        fs::rename(&self.path, predecessor)
    }
}

pub(crate) fn rotated_path(path: &Path) -> PathBuf {
    let mut filename = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("events.jsonl"));
    filename.push(".1");
    path.with_file_name(filename)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
