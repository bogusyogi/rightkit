//! Non-blocking JSONL writer.
//!
//! Ported from HeardRight `telemetry_writer.rs`: callers on hot or UI threads
//! enqueue into a bounded channel; one worker thread owns file I/O. When the
//! queue is full the event is counted as dropped rather than blocking.

use crate::{EventRecord, JsonlStore};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How an [`AsyncWriter`] reports failed writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureReporting {
    /// Every failed write is counted in [`JsonlStore::status`]. The default.
    #[default]
    EveryFailure,
    /// The first failure of a run is reported; later failures of that run are suppressed and
    /// counted; the first successful write after the run is reported as a recovery. Read the state
    /// with [`AsyncWriter::failure_status`]. In this mode failed writes are not counted in
    /// [`JsonlStore::status`].
    Once,
}

/// Failure reports of an [`AsyncWriter`] in [`FailureReporting::Once`] mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailureStatus {
    /// True while the most recent write failed and no write has succeeded since.
    pub failing: bool,
    /// Reports emitted: one per run of failures, plus one per recovery.
    pub reports: u64,
    /// Failures after the reported first one, in the most recent run of failures.
    pub suppressed: u64,
    /// Runs of failures that ended with a successful write.
    pub recoveries: u64,
    /// Message of the most recent reported failure.
    pub last_error: Option<String>,
}

enum Command {
    Append(Box<EventRecord>),
    Flush(Sender<()>),
    Stop,
}

pub struct AsyncWriter {
    sender: SyncSender<Command>,
    dropped: Arc<AtomicU64>,
    failures: Arc<Mutex<FailureStatus>>,
    handle: Option<JoinHandle<()>>,
}

impl AsyncWriter {
    /// Spawn the worker with [`FailureReporting::EveryFailure`] (the default behaviour).
    pub fn spawn(store: Arc<JsonlStore>, queue_capacity: usize) -> std::io::Result<Self> {
        Self::spawn_reporting(store, queue_capacity, FailureReporting::EveryFailure)
    }

    /// Spawn the worker with an explicit failure-reporting mode.
    pub fn spawn_reporting(
        store: Arc<JsonlStore>,
        queue_capacity: usize,
        reporting: FailureReporting,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Command>(queue_capacity.max(1));
        let failures = Arc::new(Mutex::new(FailureStatus::default()));
        let worker_failures = Arc::clone(&failures);
        let handle = std::thread::Builder::new()
            .name("rightkit-log-writer".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Append(event) => match reporting {
                            FailureReporting::EveryFailure => store.append_best_effort(&event),
                            FailureReporting::Once => match store.append(&event) {
                                Ok(()) => record_success(&worker_failures),
                                Err(error) => record_failure(
                                    &worker_failures,
                                    &format!(
                                        "append to {} failed ({error})",
                                        store.path().display()
                                    ),
                                ),
                            },
                        },
                        Command::Flush(reply) => {
                            let _ = reply.send(());
                        }
                        Command::Stop => break,
                    }
                }
            })?;
        Ok(Self {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
            failures,
            handle: Some(handle),
        })
    }

    /// Failure reports in [`FailureReporting::Once`] mode. Zeroed in the default mode.
    pub fn failure_status(&self) -> FailureStatus {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Never blocks. Returns false when the event was dropped.
    pub fn append(&self, event: EventRecord) -> bool {
        match self.sender.try_send(Command::Append(Box::new(event))) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Wait until everything enqueued before this call is on disk.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (reply, done) = mpsc::channel();
        if self.sender.send(Command::Flush(reply)).is_err() {
            return false;
        }
        done.recv_timeout(timeout).is_ok()
    }
}

fn record_failure(failures: &Mutex<FailureStatus>, message: &str) {
    let mut status = failures
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if status.failing {
        status.suppressed = status.suppressed.saturating_add(1);
    } else {
        status.failing = true;
        status.suppressed = 0;
        status.reports = status.reports.saturating_add(1);
        status.last_error = Some(message.to_owned());
    }
}

fn record_success(failures: &Mutex<FailureStatus>) {
    let mut status = failures
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if status.failing {
        status.failing = false;
        status.reports = status.reports.saturating_add(1);
        status.recoveries = status.recoveries.saturating_add(1);
    }
}

impl Drop for AsyncWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
