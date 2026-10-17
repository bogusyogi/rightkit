//! Non-blocking JSONL writer.
//!
//! Ported from HeardRight `telemetry_writer.rs`: callers on hot or UI threads
//! enqueue into a bounded channel; one worker thread owns file I/O. When the
//! queue is full the event is counted as dropped rather than blocking.

use crate::{EventRecord, JsonlStore};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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

/// One stderr failure report shared by every clone, across setup and append failures of a run.
///
/// The output is `[prefix] message` and a newline. Success never resets the report, so a run
/// prints at most one line however many writes fail. Share one reporter per app run.
#[derive(Clone, Debug)]
pub struct StderrReporter {
    prefix: Arc<str>,
    reported: Arc<AtomicBool>,
}

impl StderrReporter {
    /// A reporter that has not yet reported. `prefix` is printed in brackets before each message.
    pub fn new(prefix: impl Into<Arc<str>>) -> Self {
        Self {
            prefix: prefix.into(),
            reported: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Print `[prefix] message` to stderr if no clone has reported yet. Returns whether this call
    /// claimed the report. Stderr write errors are ignored; this never panics.
    pub fn report_once(&self, message: &str) -> bool {
        self.report_to(message, &mut io::stderr())
    }

    fn report_to(&self, message: &str, output: &mut impl Write) -> bool {
        if self.reported.swap(true, Ordering::Relaxed) {
            return false;
        }
        let _ = writeln!(output, "[{}] {message}", self.prefix);
        true
    }
}

enum Reporting {
    Status(FailureReporting),
    Stderr(StderrReporter),
}

enum Command {
    Append(Box<EventRecord>),
    Flush(Sender<()>),
    Stop,
}

pub struct AsyncWriter {
    sender: SyncSender<Command>,
    dropped: Arc<AtomicU64>,
    recording: AtomicBool,
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
        Self::spawn_worker(store, queue_capacity, Reporting::Status(reporting))
    }

    /// Spawn the worker with one failure report on stderr, shared through `reporter`.
    ///
    /// The first failed append prints `append to <path> failed (<error>)` as `[prefix] ...`;
    /// later failures stay silent, including after successful writes. [`Self::failure_status`]
    /// stays zeroed in this mode, and failed appends are not counted in [`JsonlStore::status`].
    pub fn spawn_with_reporter(
        store: Arc<JsonlStore>,
        queue_capacity: usize,
        reporter: StderrReporter,
    ) -> io::Result<Self> {
        Self::spawn_worker(store, queue_capacity, Reporting::Stderr(reporter))
    }

    fn spawn_worker(
        store: Arc<JsonlStore>,
        queue_capacity: usize,
        reporting: Reporting,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Command>(queue_capacity.max(1));
        let failures = Arc::new(Mutex::new(FailureStatus::default()));
        let worker_failures = Arc::clone(&failures);
        let handle = std::thread::Builder::new()
            .name("rightkit-log-writer".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Append(event) => match &reporting {
                            Reporting::Status(FailureReporting::EveryFailure) => {
                                store.append_best_effort(&event);
                            }
                            Reporting::Status(FailureReporting::Once) => {
                                record_result(&worker_failures, &store, store.append(&event));
                            }
                            Reporting::Stderr(reporter) => {
                                report_result(reporter, &store, store.append(&event));
                            }
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
            recording: AtomicBool::new(true),
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

    /// Turn recording on or off (on by default). While off, [`Self::append`] queues nothing and
    /// returns false, but that is not a drop: [`Self::dropped`] does not change. Use it for an
    /// app's diagnostics switch. Records already queued are still written.
    pub fn set_recording(&self, enabled: bool) {
        self.recording.store(enabled, Ordering::Relaxed);
    }

    /// Whether [`Self::append`] currently accepts records. See [`Self::set_recording`].
    pub fn recording(&self) -> bool {
        self.recording.load(Ordering::Relaxed)
    }

    /// Never blocks. Returns false when the event was dropped, or was not taken because recording
    /// is off.
    pub fn append(&self, event: EventRecord) -> bool {
        if !self.recording() {
            return false;
        }
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

    /// Wait up to `timeout` for everything enqueued before this call to be processed.
    ///
    /// The flush marker is queued without blocking: while the queue is full the call retries
    /// until the deadline, then returns false. Once queued, it waits for the worker's
    /// acknowledgement for the rest of the timeout. A disconnected worker returns false at once.
    /// `true` means every earlier append was attempted, not that it reached stable storage, and
    /// a flush marker never counts as a dropped record.
    pub fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (reply, done) = mpsc::channel();
        let mut command = Command::Flush(reply);
        loop {
            match self.sender.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    command = returned;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        done.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }
}

fn record_result(failures: &Mutex<FailureStatus>, store: &JsonlStore, result: io::Result<()>) {
    match result {
        Ok(()) => record_success(failures),
        Err(error) => record_failure(
            failures,
            &format!("append to {} failed ({error})", store.path().display()),
        ),
    }
}

fn report_result(reporter: &StderrReporter, store: &JsonlStore, result: io::Result<()>) {
    if let Err(error) = result {
        reporter.report_once(&format!(
            "append to {} failed ({error})",
            store.path().display()
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Level;

    fn controlled_writer(capacity: usize) -> (AsyncWriter, mpsc::Receiver<Command>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        (
            AsyncWriter {
                sender,
                dropped: Arc::new(AtomicU64::new(0)),
                recording: AtomicBool::new(true),
                failures: Arc::new(Mutex::new(FailureStatus::default())),
                handle: None,
            },
            receiver,
        )
    }

    #[test]
    fn full_queue_counts_drops_and_flush_gives_up_at_its_timeout() {
        let (writer, receiver) = controlled_writer(1);
        assert!(writer.append(EventRecord::new(1, Level::Info, "first")));
        assert!(!writer.append(EventRecord::new(2, Level::Info, "second")));
        assert_eq!(writer.dropped(), 1);
        let started = Instant::now();
        assert!(!writer.flush(Duration::from_millis(30)));
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(30));
        assert!(elapsed < Duration::from_secs(5));
        assert_eq!(writer.dropped(), 1);
        drop(receiver);
    }

    #[test]
    fn flush_is_acknowledged_after_earlier_records() {
        let (writer, receiver) = controlled_writer(4);
        assert!(writer.append(EventRecord::new(1, Level::Info, "first")));
        let worker = std::thread::spawn(move || {
            assert!(matches!(receiver.recv(), Ok(Command::Append(_))));
            match receiver.recv() {
                Ok(Command::Flush(reply)) => reply.send(()).unwrap(),
                _ => panic!("expected a flush after the append"),
            }
        });
        assert!(writer.flush(Duration::from_secs(5)));
        worker.join().unwrap();
    }

    #[test]
    fn recording_gate_refuses_quietly_and_counts_no_drop() {
        let (writer, receiver) = controlled_writer(4);
        assert!(writer.recording());
        writer.set_recording(false);
        assert!(!writer.recording());
        assert!(!writer.append(EventRecord::new(1, Level::Info, "muted")));
        assert_eq!(writer.dropped(), 0);
        writer.set_recording(true);
        assert!(writer.append(EventRecord::new(2, Level::Info, "heard")));
        drop(receiver);
    }

    #[test]
    fn disconnected_queue_counts_drops_and_flush_fails_at_once() {
        let (writer, receiver) = controlled_writer(1);
        drop(receiver);
        assert!(!writer.append(EventRecord::new(1, Level::Info, "lost")));
        assert!(!writer.flush(Duration::from_secs(1)));
        assert_eq!(writer.dropped(), 1);
    }

    #[test]
    fn stderr_reporter_writes_one_line_across_clones() {
        let reporter = StderrReporter::new("cutright-logs");
        let clone = reporter.clone();
        let mut output: Vec<u8> = Vec::new();
        assert!(reporter.report_to("first failure", &mut output));
        assert!(!clone.report_to("second failure", &mut output));
        assert_eq!(output, b"[cutright-logs] first failure\n");
        let mut next: Vec<u8> = Vec::new();
        assert!(StderrReporter::new("cutright-logs").report_to("new run", &mut next));
    }

    #[test]
    fn stderr_write_failure_does_not_panic_or_retry() {
        struct BrokenStderr;
        impl Write for BrokenStderr {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("closed stderr"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let reporter = StderrReporter::new("app");
        assert!(reporter.report_to("failed", &mut BrokenStderr));
        let mut sink: Vec<u8> = Vec::new();
        assert!(!reporter.report_to("failed again", &mut sink));
        assert!(sink.is_empty());
    }

    #[test]
    fn once_status_still_reports_recovery_and_new_failure_runs() {
        let status = Mutex::new(FailureStatus::default());
        record_failure(&status, "first");
        record_failure(&status, "second");
        record_success(&status);
        record_failure(&status, "third");
        let status = status.into_inner().unwrap();
        assert!(status.failing);
        assert_eq!(status.reports, 3);
        assert_eq!(status.recoveries, 1);
        assert_eq!(status.suppressed, 0);
        assert_eq!(status.last_error.as_deref(), Some("third"));
    }
}
