//! Non-blocking JSONL writer.
//!
//! Ported from HeardRight `telemetry_writer.rs`: callers on hot or UI threads
//! enqueue into a bounded channel; one worker thread owns file I/O. When the
//! queue is full the event is counted as dropped rather than blocking.

use crate::{EventRecord, JsonlStore};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

enum Command {
    Append(Box<EventRecord>),
    Flush(Sender<()>),
    Stop,
}

pub struct AsyncWriter {
    sender: SyncSender<Command>,
    dropped: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl AsyncWriter {
    pub fn spawn(store: Arc<JsonlStore>, queue_capacity: usize) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Command>(queue_capacity.max(1));
        let handle = std::thread::Builder::new()
            .name("rightkit-log-writer".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Append(event) => store.append_best_effort(&event),
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
            handle: Some(handle),
        })
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

impl Drop for AsyncWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
