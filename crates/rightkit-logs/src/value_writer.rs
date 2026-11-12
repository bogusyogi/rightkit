//! Queued writer for arbitrary JSON events over several JSONL files (issues #1026, #1027).
//!
//! One bounded queue and one worker thread serve every registered [`JsonlStore`], so:
//!
//! * a [`ValueWriter::snapshot`] copies the log files only after every earlier-queued event is on
//!   disk, and a [`ValueWriter::clear`] removes the files in queue order;
//! * an event whose serialized form exceeds [`ValueWriterConfig::max_event_bytes`] is replaced by
//!   a small drop record naming its size and limit;
//! * events a full queue refused are counted, and the count is written as one overflow record to
//!   the target file just before the next appended event.
//!
//! Failed writes follow the crate's existing reporting: with a [`StderrReporter`] the first
//! failure of the run prints one line (a per-failure line is deliberately not offered: a failing
//! disk would flood stderr from a hot thread); without one each failure is counted in the
//! store's [`JsonlStore::status`].

use crate::writer::StderrReporter;
use crate::JsonlStore;
use serde_json::{json, Map, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A destination for JSON log records that a host can swap for its own queue (issue #1027).
/// `rightkit-tauri-shell`'s `logs` commands take an `Arc<dyn LogSink>` instead of building their
/// own writer.
pub trait LogSink: Send + Sync {
    /// Queue one record on the primary file without blocking. False when it was dropped or
    /// recording is off.
    fn append(&self, value: Value) -> bool;
    /// Write one record on the primary file before returning, bypassing the queue (so it is not
    /// ordered against queued records).
    fn append_sync(&self, value: &Value) -> io::Result<()>;
    /// Wait up to `timeout` for everything queued so far to be processed.
    fn flush(&self, timeout: Duration) -> bool;
    /// Remove every file the sink owns, in queue order after everything queued so far.
    fn clear(&self) -> io::Result<()>;
}

/// Why a barrier operation failed.
#[derive(Debug)]
pub enum WriterError {
    /// The queue stayed full, or the worker did not answer, within the timeout. A barrier already
    /// queued still runs later.
    Timeout,
    /// The worker stopped.
    Disconnected,
    /// A path is not one of this writer's files.
    UnknownPath(PathBuf),
    /// Two files share a name, so a snapshot would overwrite one with the other.
    DuplicateName(PathBuf),
    /// The file operation failed.
    Io(io::Error),
}

impl std::fmt::Display for WriterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("log writer did not answer in time"),
            Self::Disconnected => f.write_str("log writer stopped"),
            Self::UnknownPath(path) => {
                write!(f, "not a log file of this writer: {}", path.display())
            }
            Self::DuplicateName(path) => {
                write!(f, "two log files share the name of {}", path.display())
            }
            Self::Io(error) => write!(f, "log file operation failed: {error}"),
        }
    }
}

impl std::error::Error for WriterError {}

impl From<WriterError> for io::Error {
    fn from(error: WriterError) -> Self {
        match error {
            WriterError::Timeout => io::Error::new(io::ErrorKind::TimedOut, error.to_string()),
            WriterError::Io(inner) => inner,
            other => io::Error::other(other.to_string()),
        }
    }
}

/// Settings of a [`ValueWriter`].
#[derive(Debug, Clone)]
pub struct ValueWriterConfig {
    /// Records the queue holds before new ones are dropped and counted. Default 1024; 0 behaves
    /// as 1.
    pub queue_capacity: usize,
    /// Largest serialized event kept whole, in bytes. Default 64 KiB.
    pub max_event_bytes: usize,
    /// Deadline [`LogSink::clear`] waits for the worker. Default 5 s.
    pub barrier_timeout: Duration,
    /// `event` name of the record that replaces an oversized event. Default `event_dropped`.
    pub drop_event: String,
    /// `event` name of the overflow record. Default `events_overflow`.
    pub overflow_event: String,
    /// One stderr line for the first failed write of the run. `None` counts failures in each
    /// store's status instead.
    pub reporter: Option<StderrReporter>,
}

impl Default for ValueWriterConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 1024,
            max_event_bytes: 64 * 1024,
            barrier_timeout: Duration::from_secs(5),
            drop_event: "event_dropped".into(),
            overflow_event: "events_overflow".into(),
            reporter: None,
        }
    }
}

enum Command {
    Append {
        store: usize,
        value: Value,
    },
    Flush(Sender<()>),
    Snapshot {
        stores: Vec<usize>,
        destination: PathBuf,
        reply: Sender<Result<Vec<PathBuf>, WriterError>>,
    },
    Clear {
        stores: Vec<usize>,
        reply: Sender<Result<(), WriterError>>,
    },
    Stop,
}

/// The queued multi-file writer. Dropping it processes what is queued and joins the worker.
pub struct ValueWriter {
    sender: SyncSender<Command>,
    stores: Vec<Arc<JsonlStore>>,
    config: ValueWriterConfig,
    dropped: Arc<AtomicU64>,
    pending_overflow: Arc<AtomicU64>,
    oversized: AtomicU64,
    recording: AtomicBool,
    handle: Option<JoinHandle<()>>,
}

impl ValueWriter {
    /// Start the worker over `stores`. The first store is the primary file ([`LogSink`] writes
    /// there). At least one store is required.
    pub fn spawn(stores: Vec<Arc<JsonlStore>>, config: ValueWriterConfig) -> io::Result<Self> {
        if stores.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a value writer needs at least one log file",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<Command>(config.queue_capacity.max(1));
        let pending_overflow = Arc::new(AtomicU64::new(0));
        let worker = Worker {
            stores: stores.clone(),
            overflow: Arc::clone(&pending_overflow),
            overflow_event: config.overflow_event.clone(),
            reporter: config.reporter.clone(),
        };
        let handle = std::thread::Builder::new()
            .name("rightkit-value-writer".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Append { store, value } => worker.append(store, value),
                        Command::Flush(reply) => {
                            let _ = reply.send(());
                        }
                        Command::Snapshot {
                            stores,
                            destination,
                            reply,
                        } => {
                            let _ = reply.send(worker.snapshot(&stores, &destination));
                        }
                        Command::Clear { stores, reply } => {
                            let _ = reply.send(worker.clear(&stores));
                        }
                        Command::Stop => break,
                    }
                }
            })?;
        Ok(Self {
            sender,
            stores,
            config,
            dropped: Arc::new(AtomicU64::new(0)),
            pending_overflow,
            oversized: AtomicU64::new(0),
            recording: AtomicBool::new(true),
            handle: Some(handle),
        })
    }

    /// Turn recording on or off (on by default). While off, appends queue nothing and are not
    /// counted as drops.
    pub fn set_recording(&self, enabled: bool) {
        self.recording.store(enabled, Ordering::Relaxed);
    }

    /// Whether appends are currently accepted.
    pub fn recording(&self) -> bool {
        self.recording.load(Ordering::Relaxed)
    }

    /// Events a full queue refused, since the writer started.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Events replaced by a drop record because they exceeded the size bound.
    pub fn oversized(&self) -> u64 {
        self.oversized.load(Ordering::Relaxed)
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.stores.iter().position(|store| store.path() == path)
    }

    /// `value` replaced by a drop record when its serialized form exceeds the bound.
    fn bounded(&self, value: Value) -> Value {
        let size = serde_json::to_vec(&value).map_or(usize::MAX, |bytes| bytes.len());
        if size <= self.config.max_event_bytes {
            return value;
        }
        self.oversized.fetch_add(1, Ordering::Relaxed);
        let mut record = Map::new();
        record.insert(
            "schema_version".into(),
            value.get("schema_version").cloned().unwrap_or(json!(1)),
        );
        record.insert("ts_ms".into(), json!(crate::now_ms()));
        record.insert("level".into(), json!("warn"));
        record.insert("event".into(), json!(self.config.drop_event));
        record.insert("reason".into(), json!("event_too_large"));
        record.insert("bytes".into(), json!(size));
        record.insert("limit".into(), json!(self.config.max_event_bytes));
        if let Some(name) = value.get("event").and_then(Value::as_str) {
            let name: String = name.chars().take(64).collect();
            record.insert("dropped_event".into(), json!(name));
        }
        Value::Object(record)
    }

    /// Queue `value` for the file at `path` without blocking. False when the path is not one of
    /// this writer's files, recording is off, or the queue was full (the drop is counted and
    /// reported by the next overflow record).
    pub fn append_to(&self, path: &Path, value: Value) -> bool {
        match self.index_of(path) {
            Some(store) => self.enqueue(store, value),
            None => false,
        }
    }

    fn enqueue(&self, store: usize, value: Value) -> bool {
        if !self.recording() {
            return false;
        }
        let value = self.bounded(value);
        match self.sender.try_send(Command::Append { store, value }) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                self.pending_overflow.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    fn send_barrier(&self, mut command: Command, deadline: Instant) -> Result<(), WriterError> {
        loop {
            match self.sender.try_send(command) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return Err(WriterError::Timeout);
                    }
                    command = returned;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(TrySendError::Disconnected(_)) => return Err(WriterError::Disconnected),
            }
        }
    }

    fn indices(&self, paths: &[PathBuf]) -> Result<Vec<usize>, WriterError> {
        if paths.is_empty() {
            return Ok((0..self.stores.len()).collect());
        }
        paths
            .iter()
            .map(|path| {
                self.index_of(path)
                    .ok_or_else(|| WriterError::UnknownPath(path.clone()))
            })
            .collect()
    }

    /// Wait up to `timeout` for everything queued before this call to be processed. True means
    /// every earlier append was attempted, not that it reached stable storage.
    pub fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (reply, done) = mpsc::channel();
        if self.send_barrier(Command::Flush(reply), deadline).is_err() {
            return false;
        }
        done.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }

    /// Queue-ordered export: after every earlier-queued event is written, copy the current file
    /// of each of `paths` (empty means all) and its rotated generations (`name.1` newest) into `destination`, and return the copies. A file
    /// that does not exist yet is skipped. A pending overflow count is written to the primary
    /// file first so the copy carries it.
    pub fn snapshot(
        &self,
        paths: &[PathBuf],
        destination: &Path,
        timeout: Duration,
    ) -> Result<Vec<PathBuf>, WriterError> {
        let stores = self.indices(paths)?;
        let mut names = Vec::new();
        for &index in &stores {
            let path = self.stores[index].path();
            let name = path.file_name().map(|name| name.to_owned());
            if names.contains(&name) {
                return Err(WriterError::DuplicateName(path.to_path_buf()));
            }
            names.push(name);
        }
        let deadline = Instant::now() + timeout;
        let (reply, done) = mpsc::channel();
        self.send_barrier(
            Command::Snapshot {
                stores,
                destination: destination.to_path_buf(),
                reply,
            },
            deadline,
        )?;
        done.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| WriterError::Timeout)?
    }

    /// Queue-ordered clear: after every earlier-queued event, remove each of `paths` (empty means
    /// all) with its rotated predecessors, and forget a pending overflow count.
    pub fn clear_paths(&self, paths: &[PathBuf], timeout: Duration) -> Result<(), WriterError> {
        let stores = self.indices(paths)?;
        let deadline = Instant::now() + timeout;
        let (reply, done) = mpsc::channel();
        self.send_barrier(Command::Clear { stores, reply }, deadline)?;
        done.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| WriterError::Timeout)?
    }
}

impl LogSink for ValueWriter {
    fn append(&self, value: Value) -> bool {
        self.enqueue(0, value)
    }

    fn append_sync(&self, value: &Value) -> io::Result<()> {
        if !self.recording() {
            return Ok(());
        }
        self.stores[0].append_value(&self.bounded(value.clone()))
    }

    fn flush(&self, timeout: Duration) -> bool {
        ValueWriter::flush(self, timeout)
    }

    fn clear(&self) -> io::Result<()> {
        self.clear_paths(&[], self.config.barrier_timeout)
            .map_err(io::Error::from)
    }
}

impl Drop for ValueWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct Worker {
    stores: Vec<Arc<JsonlStore>>,
    overflow: Arc<AtomicU64>,
    overflow_event: String,
    reporter: Option<StderrReporter>,
}

impl Worker {
    fn write(&self, store: usize, value: &Value) {
        let store = &self.stores[store];
        match &self.reporter {
            Some(reporter) => {
                if let Err(error) = store.append_value(value) {
                    reporter.report_once(&format!(
                        "append to {} failed ({error})",
                        store.path().display()
                    ));
                }
            }
            None => store.append_value_best_effort(value),
        }
    }

    fn write_overflow(&self, store: usize) {
        let dropped = self.overflow.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            self.write(
                store,
                &json!({
                    "schema_version": 1,
                    "ts_ms": crate::now_ms(),
                    "level": "warn",
                    "event": self.overflow_event,
                    "dropped": dropped,
                }),
            );
        }
    }

    fn append(&self, store: usize, value: Value) {
        self.write_overflow(store);
        self.write(store, &value);
    }

    fn snapshot(&self, stores: &[usize], destination: &Path) -> Result<Vec<PathBuf>, WriterError> {
        self.write_overflow(0);
        std::fs::create_dir_all(destination).map_err(WriterError::Io)?;
        let mut copied = Vec::new();
        for &index in stores {
            let store = &self.stores[index];
            // The current file, then each rotated generation (`name.1` newest), same names.
            let sources =
                std::iter::once(store.path().to_path_buf()).chain(store.generation_paths());
            for source in sources {
                let Some(name) = source.file_name() else {
                    continue;
                };
                let target = destination.join(name);
                match std::fs::copy(&source, &target) {
                    Ok(_) => copied.push(target),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(WriterError::Io(error)),
                }
            }
        }
        Ok(copied)
    }

    fn clear(&self, stores: &[usize]) -> Result<(), WriterError> {
        self.overflow.store(0, Ordering::Relaxed);
        for &index in stores {
            self.stores[index].clear().map_err(WriterError::Io)?;
        }
        Ok(())
    }
}
