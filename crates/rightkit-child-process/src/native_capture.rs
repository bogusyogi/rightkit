//! Cancellable capture of native child pipes. Generic `Read` capture remains separate.

use crate::CapturedOutput;
use std::{
    fs::File,
    io,
    process::{ChildStderr, ChildStdout},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[cfg(unix)]
#[path = "native_capture/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "native_capture/windows.rs"]
pub(crate) mod platform;

const CHUNK: usize = 8 * 1024;
const IDLE_WAIT: Duration = Duration::from_millis(20);

#[derive(Default)]
struct State {
    output: CapturedOutput,
    error: Option<io::Error>,
    eof: bool,
    finished: bool,
}

type Shared = (Mutex<State>, Condvar);

/// Which of a child's pipes a capture reads. Names the stream for the test seam only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

/// Test seam: make the next capture of `stream` that the calling thread starts fail its first
/// read with `kind`, which the reader stores exactly as it stores a real read failure.
#[cfg(test)]
pub(crate) fn inject_read_fault(stream: Stream, kind: io::ErrorKind) {
    test_fault::arm(stream, kind);
}

/// Receives each chunk the reader thread reads, on that thread, together with the capture's
/// stop flag. A sink that can wait (a full channel) must give up once the flag is set, so a
/// join never waits on it. Only [`crate::run_streaming`] installs one.
pub(crate) type ChunkSink = Box<dyn FnMut(&[u8], &AtomicBool) + Send + 'static>;

struct Done(Arc<Shared>);

impl Drop for Done {
    fn drop(&mut self) {
        self.0 .0.lock().unwrap_or_else(|e| e.into_inner()).finished = true;
        self.0 .1.notify_all();
    }
}

/// An owned reader of a native child stdout/stderr pipe, with bounded byte retention.
///
/// Unix read ends use nonblocking I/O; Windows read ends use owned overlapped I/O.
/// Cancellation wakes the private reader; no user callbacks run
/// on it. [`cancel`](Self::cancel), [`finish_within`](Self::finish_within) and `Drop` all join
/// the reader and close its pipe, even when a descendant keeps the write end open. Idle
/// readers check the pipe every 20 ms; cancellation also wakes that wait immediately.
///
/// This guarantee cannot be provided for arbitrary `Read`; use [`crate::OutputCapture`]
/// only when the reader's lifetime is controlled separately. Do not change the native
/// read end's blocking mode through another handle while this capture owns it.
/// Windows requires a byte pipe opened for overlapped I/O (including `OwnedCommand` pipes);
/// disconnected overlapped pipes are accepted as EOF without issuing a read.
/// Construction rejects unsupported handles before starting a reader.
pub struct NativeOutputCapture {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl NativeOutputCapture {
    /// Own & drain a child's stdout. Errors before spawning a reader if the pipe cannot
    /// support cancellable native I/O or the operating system cannot start the reader thread.
    pub fn stdout(pipe: ChildStdout, max_bytes: usize) -> io::Result<Self> {
        Self::start(platform::stdout(pipe), max_bytes, None, Stream::Stdout)
    }

    /// Own & drain a child's stderr, with the same guarantees as [`stdout`](Self::stdout).
    pub fn stderr(pipe: ChildStderr, max_bytes: usize) -> io::Result<Self> {
        Self::start(platform::stderr(pipe), max_bytes, None, Stream::Stderr)
    }

    /// Like [`stdout`](Self::stdout), but every chunk is also handed to `sink` on the reader
    /// thread and nothing is retained.
    pub(crate) fn stdout_streaming(pipe: ChildStdout, sink: ChunkSink) -> io::Result<Self> {
        Self::start(platform::stdout(pipe), 0, Some(sink), Stream::Stdout)
    }

    /// The stderr counterpart of [`stdout_streaming`](Self::stdout_streaming).
    pub(crate) fn stderr_streaming(pipe: ChildStderr, sink: ChunkSink) -> io::Result<Self> {
        Self::start(platform::stderr(pipe), 0, Some(sink), Stream::Stderr)
    }

    fn start(
        pipe: File,
        max_bytes: usize,
        mut sink: Option<ChunkSink>,
        stream: Stream,
    ) -> io::Result<Self> {
        let prepared = platform::prepare(pipe)?;
        // Test seam: taken here, on the thread that starts the capture, so a fault armed by one
        // test can only reach a capture that same test starts (see `inject_read_fault`).
        #[cfg(test)]
        let mut fault = test_fault::take(stream);
        #[cfg(not(test))]
        let _ = stream;
        let shared: Arc<Shared> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::clone(&shared);
        let cancelled = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("rightkit-native-output-capture".into())
            .spawn(move || {
                let _done = Done(Arc::clone(&state));
                let mut pipe = platform::reader(prepared);
                let mut buffer = [0; CHUNK];
                while !cancelled.load(Ordering::Acquire) {
                    #[cfg(test)]
                    let read = match fault.take() {
                        Some(kind) => Err(io::Error::new(kind, "injected pipe read failure")),
                        None => platform::read(&mut pipe, &mut buffer),
                    };
                    #[cfg(not(test))]
                    let read = platform::read(&mut pipe, &mut buffer);
                    match read {
                        Ok(0) => {
                            state.0.lock().unwrap_or_else(|e| e.into_inner()).eof = true;
                            break;
                        }
                        Ok(count) => {
                            if let Some(sink) = sink.as_mut() {
                                sink(&buffer[..count], &cancelled);
                            }
                            state
                                .0
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .output
                                .push(&buffer[..count], max_bytes);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::park_timeout(IDLE_WAIT);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            state.0.lock().unwrap_or_else(|e| e.into_inner()).error = Some(error);
                            break;
                        }
                    }
                }
                if let Err(error) = platform::shutdown(&mut pipe) {
                    let mut state = state.0.lock().unwrap_or_else(|e| e.into_inner());
                    if state.error.is_none() {
                        state.error = Some(error);
                    }
                }
                // Close the read end before Done signals completion to finish_within.
                drop(pipe);
            })?;
        Ok(Self {
            shared,
            stop,
            handle: Some(handle),
        })
    }

    /// What has been captured so far. This does not stop the reader.
    pub fn snapshot(&self) -> CapturedOutput {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output
            .clone()
    }

    /// True only if the pipe reached EOF. A deadline/cancellation can return partial bytes
    /// without exceeding the retention cap; `CapturedOutput::truncated` describes cap loss.
    pub fn is_complete(&self) -> bool {
        self.shared.0.lock().unwrap_or_else(|e| e.into_inner()).eof
    }

    /// Take the native read error, if any. Cancellation is not an error.
    pub fn take_error(&self) -> Option<io::Error> {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .take()
    }

    /// Wait up to `timeout` for EOF/error, then cancel & join the reader. Always closes the
    /// read end before returning; a writer retained by a descendant cannot extend the wait.
    /// Returned bytes may be partial: consult [`is_complete`](Self::is_complete).
    pub fn finish_within(&mut self, timeout: Duration) -> CapturedOutput {
        {
            let state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
            drop(
                self.shared
                    .1
                    .wait_timeout_while(state, timeout, |state| !state.finished)
                    .unwrap_or_else(|e| e.into_inner()),
            );
        }
        self.stop_and_join();
        self.snapshot()
    }

    /// Cancel immediately, join the reader & return retained bytes. Idempotent.
    pub fn cancel(&mut self) -> CapturedOutput {
        self.stop_and_join();
        self.snapshot()
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            if handle.join().is_err() {
                self.shared
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .error = Some(io::Error::other("native pipe reader panicked"));
            }
        }
    }
}

impl Drop for NativeOutputCapture {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[cfg(test)]
pub(crate) mod test_fault {
    use super::Stream;
    use std::{cell::Cell, io};

    thread_local! {
        // Thread-local, not global: `start` runs on the thread that called `run`, so parallel
        // tests cannot take each other's fault.
        static ARMED: Cell<[Option<io::ErrorKind>; 2]> = const { Cell::new([None, None]) };
    }

    pub(crate) fn arm(stream: Stream, kind: io::ErrorKind) {
        ARMED.with(|armed| {
            let mut slots = armed.get();
            slots[stream as usize] = Some(kind);
            armed.set(slots);
        });
    }

    pub(super) fn take(stream: Stream) -> Option<io::ErrorKind> {
        ARMED.with(|armed| {
            let mut slots = armed.get();
            let kind = slots[stream as usize].take();
            armed.set(slots);
            kind
        })
    }
}
