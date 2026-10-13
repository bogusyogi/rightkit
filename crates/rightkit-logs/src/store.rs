use crate::rotate;
use crate::EventRecord;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Failures a [`JsonlStore`] has absorbed instead of returning or printing them. Read with
/// [`JsonlStore::status`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreStatus {
    /// Best-effort appends that did not write their event (including refusals at the hard cap).
    pub failed_appends: u64,
    /// Rotations that failed. The event was still appended to the current file while that file was
    /// below the hard cap (see [`JsonlStore::new`]).
    pub failed_rotations: u64,
    /// Message of the most recent failure, append or rotation.
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct JsonlStore {
    path: PathBuf,
    max_file_bytes: u64,
    generations: u32,
    append_lock: Mutex<()>,
    failures: Mutex<StoreStatus>,
}

impl JsonlStore {
    /// `max_file_bytes` is the rotation size; `0` disables rotation.
    ///
    /// If rotation fails (for example the rotated name is blocked), the event is still appended to
    /// the current file, and the failure is counted in [`StoreStatus`]. That fallback is bounded:
    /// once the current file reaches twice `max_file_bytes` (the hard cap), appends are refused,
    /// counted in `failed_appends`, and the file does not grow further until rotation succeeds.
    pub fn new(path: impl Into<PathBuf>, max_file_bytes: u64) -> Self {
        Self {
            path: path.into(),
            max_file_bytes,
            generations: 1,
            append_lock: Mutex::new(()),
            failures: Mutex::new(StoreStatus::default()),
        }
    }

    /// Keep `generations` rotated predecessors (`.1` newest through `.N` oldest) instead of one.
    /// Default is 1, which is the behaviour before this option existed; `0` behaves as 1.
    pub fn with_generations(mut self, generations: u32) -> Self {
        self.generations = generations.max(1);
        self
    }

    /// Counts and last-error for failures this store has absorbed. Never blocks on a write.
    pub fn status(&self) -> StoreStatus {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
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

    /// Append, absorbing failure: the event is not written, the failure is counted and its message
    /// kept in [`JsonlStore::status`]. Nothing is printed.
    pub fn append_best_effort(&self, event: &EventRecord) {
        if let Err(error) = self.append(event) {
            self.record(|status| {
                status.failed_appends = status.failed_appends.saturating_add(1);
                status.last_error = Some(format!(
                    "append to {} failed ({error}); event={}",
                    self.path.display(),
                    event.event
                ));
            });
        }
    }

    fn record(&self, update: impl FnOnce(&mut StoreStatus)) {
        let mut status = self
            .failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut status);
    }

    pub fn clear(&self) -> io::Result<()> {
        let _guard = self
            .append_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_if_present(&self.path)?;
        for n in 1..=self.generations {
            remove_if_present(&rotate::generation_path(&self.path, n, DEFAULT_NAME))?;
        }
        Ok(())
    }

    fn append_value_locked(&self, value: &Value) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Err(error) = self.rotate_if_needed() {
            self.record(|status| {
                status.failed_rotations = status.failed_rotations.saturating_add(1);
                status.last_error = Some(format!(
                    "rotate {} failed ({error}); event kept in the current file",
                    self.path.display()
                ));
            });
            self.refuse_at_hard_cap()?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }

    /// After a failed rotation: refuse the append once the current file is at the hard cap.
    fn refuse_at_hard_cap(&self) -> io::Result<()> {
        rotate::refuse_at_hard_cap(&self.path, self.max_file_bytes)
    }

    fn rotate_if_needed(&self) -> io::Result<()> {
        rotate::rotate_if_needed(
            &self.path,
            self.max_file_bytes,
            self.generations,
            DEFAULT_NAME,
        )
    }
}

const DEFAULT_NAME: &str = "events.jsonl";

pub(crate) fn rotated_path(path: &Path) -> PathBuf {
    rotate::generation_path(path, 1, DEFAULT_NAME)
}

use rotate::remove_if_present;
