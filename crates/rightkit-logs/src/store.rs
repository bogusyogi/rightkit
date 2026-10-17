use crate::rotate;
use crate::EventRecord;
use serde_json::Value;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a rotation waits for another process's rotation lock before it skips this rotation.
const ROTATION_LOCK_WAIT: Duration = Duration::from_millis(500);
const ROTATION_LOCK_POLL: Duration = Duration::from_millis(25);

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
    rotate_before_overflow: bool,
    owner_only: bool,
    cross_process_lock: bool,
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
            rotate_before_overflow: false,
            owner_only: false,
            cross_process_lock: false,
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

    /// Create the file owner-only (`0600` on Unix, an owner-only DACL on Windows) when this store
    /// creates it, for logs that hold personal data. An existing file keeps its permissions.
    /// The file is opened as [`rightkit_fs::open_private_append`] opens it, so a symlink or a
    /// non-regular file at the path is an error. Default is off.
    pub fn with_owner_only(mut self, owner_only: bool) -> Self {
        self.owner_only = owner_only;
        self
    }

    /// Serialise size rotation across processes. Each rotation takes an exclusive lock on the
    /// sidecar file `<name>.lock`, re-reads the size under it, and only then renames generations,
    /// so two processes appending to one log cannot each remove the other's rotated file. A
    /// rotation that cannot get the lock within 500 ms is skipped; the append still happens, and
    /// the next append tries again. Appends themselves take no lock.
    ///
    /// The lock is advisory `std` file locking (`flock` on Unix, `LockFileEx` on Windows). It
    /// excludes only callers that take this same sidecar lock: a `rightkit-fs` `FileLock` on the
    /// same log does not share that lock.
    pub fn with_cross_process_lock(mut self) -> Self {
        self.cross_process_lock = true;
        self
    }

    /// Rotate when the current file plus the incoming line would exceed `max_file_bytes`, instead
    /// of only once the file has reached it. A file then never grows past the limit, except for a
    /// single line longer than the limit, which is written alone. Default is off (rotate once the
    /// limit is reached).
    pub fn with_rotate_before_overflow(mut self, enabled: bool) -> Self {
        self.rotate_before_overflow = enabled;
        self
    }

    /// Append one event, rotating first if it would overflow the limit, whatever
    /// [`JsonlStore::with_rotate_before_overflow`] says.
    pub fn append_bounded(&self, value: &Value) -> io::Result<()> {
        let _guard = self
            .append_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.append_serialized_locked(value, true)
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

    /// The last `max_bytes` bytes of the current file, without reading the rest. See [`tail_bytes`].
    pub fn tail_bytes(&self, max_bytes: u64) -> io::Result<Vec<u8>> {
        tail_bytes(&self.path, max_bytes)
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
        self.append_serialized_locked(value, self.rotate_before_overflow)
    }

    fn append_serialized_locked(&self, value: &Value, before_overflow: bool) -> io::Result<()> {
        let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
        line.push(b'\n');
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // One cross-process lock covers the size check, rotation, open and append, so another
        // process cannot rotate and evict this file between our open and our write.
        let lock = if self.cross_process_lock {
            match self.acquire_rotation_lock() {
                Ok(Some(lock)) => Some(lock),
                Ok(None) => {
                    self.record_rotation_failure("rotation lock held by another process");
                    self.refuse_at_hard_cap()?;
                    None
                }
                Err(error) => {
                    self.record_rotation_failure(&error.to_string());
                    self.refuse_at_hard_cap()?;
                    None
                }
            }
        } else {
            None
        };
        if lock.is_some() || !self.cross_process_lock {
            if let Err(error) = self.rotate_files(before_overflow, line.len() as u64) {
                self.record_rotation_failure(&error.to_string());
                self.refuse_at_hard_cap()?;
            }
        }
        let mut file = if self.owner_only {
            rightkit_fs::open_private_append(&self.path)?
        } else {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?
        };
        let result = file.write_all(&line);
        drop(file);
        drop(lock);
        result
    }

    fn record_rotation_failure(&self, error: &str) {
        self.record(|status| {
            status.failed_rotations = status.failed_rotations.saturating_add(1);
            status.last_error = Some(format!(
                "rotate {} failed ({error}); event kept in the current file",
                self.path.display()
            ));
        });
    }

    /// After a failed rotation: refuse the append once the current file is at the hard cap.
    fn refuse_at_hard_cap(&self) -> io::Result<()> {
        rotate::refuse_at_hard_cap(&self.path, self.max_file_bytes)
    }

    fn rotate_files(&self, before_overflow: bool, incoming: u64) -> io::Result<()> {
        if before_overflow {
            rotate::rotate_before_overflow(
                &self.path,
                self.max_file_bytes,
                self.generations,
                DEFAULT_NAME,
                incoming,
            )
        } else {
            rotate::rotate_if_needed(
                &self.path,
                self.max_file_bytes,
                self.generations,
                DEFAULT_NAME,
            )
        }
    }

    /// Exclusive lock on the sidecar lock file, held until the returned `File` is dropped. `None`
    /// when another holder kept it past [`ROTATION_LOCK_WAIT`].
    fn acquire_rotation_lock(&self) -> io::Result<Option<File>> {
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(rotation_lock_path(&self.path))?;
        let deadline = Instant::now() + ROTATION_LOCK_WAIT;
        loop {
            let outcome = lock.try_lock();
            match outcome {
                Ok(()) => return Ok(Some(lock)),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(ROTATION_LOCK_POLL);
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
    }
}

const DEFAULT_NAME: &str = "events.jsonl";

/// The last `max_bytes` bytes of the file at `path`, read without loading the rest of it. The
/// first byte may fall inside a line. A missing file reads as empty; other errors are returned.
pub fn tail_bytes(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    // Bound the read to the snapshot so concurrent appends cannot exceed `max_bytes`.
    file.take(len - start).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// `<file name>.lock` beside the log, the file that carries the rotation lock.
fn rotation_lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(DEFAULT_NAME));
    name.push(".lock");
    path.with_file_name(name)
}

pub(crate) fn rotated_path(path: &Path) -> PathBuf {
    rotate::generation_path(path, 1, DEFAULT_NAME)
}

use rotate::remove_if_present;

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rightkit-logs-store-{label}-{}-{}",
            std::process::id(),
            crate::now_ms()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn event(n: i64) -> EventRecord {
        EventRecord::new(n, crate::Level::Info, "tick").with_field("n", serde_json::json!(n))
    }

    #[test]
    fn tail_bytes_reads_only_the_last_bytes() {
        let d = dir("tail");
        let path = d.join("log.txt");
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(tail_bytes(&path, 4).unwrap(), b"6789");
        assert_eq!(tail_bytes(&path, 100).unwrap(), b"0123456789");
        assert_eq!(
            tail_bytes(&d.join("missing.txt"), 4).unwrap(),
            Vec::<u8>::new()
        );
        assert_eq!(
            JsonlStore::new(path.clone(), 0).tail_bytes(3).unwrap(),
            b"789"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_store_creates_a_0600_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("owner");
        let path = d.join("events.jsonl");
        let store = JsonlStore::new(path.clone(), 0).with_owner_only(true);
        store.append(&event(1)).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn rotation_is_skipped_while_another_holder_keeps_the_lock() {
        let d = dir("lock");
        let path = d.join("events.jsonl");
        let store = JsonlStore::new(path.clone(), 1).with_cross_process_lock();
        store.append(&event(1)).unwrap();
        let holder = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(rotation_lock_path(&path))
            .unwrap();
        holder.lock().unwrap();
        // Over the hard cap with no lock: the append is refused, never silently written.
        assert!(store.append(&event(2)).is_err());
        assert!(!d.join("events.jsonl.1").exists());
        drop(holder);
        store.append(&event(3)).unwrap();
        assert!(d.join("events.jsonl.1").exists());
    }

    #[test]
    fn tail_bytes_stays_within_the_snapshot_bound() {
        let d = dir("tailbound");
        let path = d.join("log.txt");
        fs::write(&path, b"0123456789").unwrap();
        // A file grown after the snapshot cannot push the read past `max_bytes`.
        let out = tail_bytes(&path, 4).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"abcdef")
            .unwrap();
        assert!(out.len() <= 4);
        assert_eq!(tail_bytes(&path, 4).unwrap().len(), 4);
    }

    #[test]
    fn lock_is_held_across_the_append() {
        let d = dir("heldlock");
        let path = d.join("events.jsonl");
        let store =
            std::sync::Arc::new(JsonlStore::new(path.clone(), 400).with_cross_process_lock());
        let handles: Vec<_> = (0..2)
            .map(|t| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for n in 0..40 {
                        store.append(&event(t * 100 + n)).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        // The lock is released after every append, and no line was torn.
        let probe = OpenOptions::new()
            .write(true)
            .open(rotation_lock_path(&path))
            .unwrap();
        assert!(probe.try_lock().is_ok());
        let text = fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
    }

    #[test]
    fn lock_failure_still_enforces_the_hard_cap() {
        let d = dir("lockcap");
        let path = d.join("events.jsonl");
        let store = JsonlStore::new(path.clone(), 100).with_cross_process_lock();
        let holder = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(rotation_lock_path(&path))
            .unwrap();
        holder.lock().unwrap();
        fs::write(&path, vec![b'x'; 250]).unwrap();
        assert!(store.append(&event(1)).is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), 250);
    }
}
