//! Byte-exact, bounded capture of a child's output (stdout of a CLI that speaks JSON or
//! another binary-safe protocol).
//!
//! [`StderrTail`](crate::StderrTail) is a diagnostic tail: it keeps the most recent *lines* as
//! text, joins them with `\n` and drops the trailing newline, so it cannot carry a protocol.
//! [`OutputCapture`] keeps the *first* `max_bytes` bytes exactly as the child wrote them, and
//! keeps draining past the cap (discarding) so a chatty child never blocks on a full pipe.
//! [`CapturedOutput::truncated`] says whether anything was discarded; a caller that parses the
//! bytes must treat a truncated capture as incomplete.

use std::io::{self, Read};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const CHUNK: usize = 8 * 1024;

/// What a capture holds.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapturedOutput {
    /// The first `max_bytes` bytes of the stream, unmodified.
    pub bytes: Vec<u8>,
    /// True when the stream held more than `max_bytes` bytes and the rest was discarded.
    pub truncated: bool,
    /// Every byte the stream produced so far, kept or not.
    pub total_bytes: u64,
}

impl CapturedOutput {
    fn push(&mut self, chunk: &[u8], max_bytes: usize) {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len() as u64);
        let room = max_bytes.saturating_sub(self.bytes.len());
        let keep = room.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..keep]);
        if keep < chunk.len() {
            self.truncated = true;
        }
    }
}

/// Reads `reader` to its end on the calling thread, keeping the first `max_bytes` bytes. The
/// rest is read and discarded. Use this when the caller can block until the stream closes.
pub fn read_bounded<R: Read>(mut reader: R, max_bytes: usize) -> io::Result<CapturedOutput> {
    let mut captured = CapturedOutput::default();
    let mut buffer = [0u8; CHUNK];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(captured),
            Ok(count) => captured.push(&buffer[..count], max_bytes),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

type Done = (Mutex<bool>, Condvar);

/// Marks the drain over when the thread's body ends, however it ends.
struct DoneGuard(Arc<Done>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.0 .1.notify_all();
    }
}

/// A background drain of one output stream into a bounded byte buffer. See the module
/// documentation.
pub struct OutputCapture {
    data: Arc<Mutex<CapturedOutput>>,
    /// The error that ended the drain, if the stream did not end cleanly.
    error: Arc<Mutex<Option<io::Error>>>,
    handle: Option<JoinHandle<()>>,
    done: Arc<Done>,
}

impl OutputCapture {
    /// Drains `reader` on a thread, keeping the first `max_bytes` bytes. Best effort: if the
    /// thread cannot be spawned the capture holds nothing (use
    /// [`try_capture`](Self::try_capture) to learn that).
    pub fn capture<R: Read + Send + 'static>(reader: R, max_bytes: usize) -> Self {
        Self::try_capture(reader, max_bytes).unwrap_or_else(|_| Self {
            data: Arc::default(),
            error: Arc::default(),
            handle: None,
            done: Arc::new((Mutex::new(true), Condvar::new())),
        })
    }

    /// Like [`capture`](Self::capture), but returns the operating-system error when the drain
    /// thread cannot be spawned.
    pub fn try_capture<R: Read + Send + 'static>(reader: R, max_bytes: usize) -> io::Result<Self> {
        let data: Arc<Mutex<CapturedOutput>> = Arc::default();
        let error: Arc<Mutex<Option<io::Error>>> = Arc::default();
        let done: Arc<Done> = Arc::new((Mutex::new(false), Condvar::new()));
        let guard = DoneGuard(Arc::clone(&done));
        let shared = Arc::clone(&data);
        let failure = Arc::clone(&error);
        let handle = thread::Builder::new()
            .name("rightkit-output-capture".into())
            .spawn(move || {
                let _guard = guard;
                let mut reader = reader;
                let mut buffer = [0u8; CHUNK];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => return,
                        Ok(count) => shared
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(&buffer[..count], max_bytes),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                            return;
                        }
                    }
                }
            })?;
        Ok(Self {
            data,
            error,
            handle: Some(handle),
            done,
        })
    }

    /// What has been captured so far.
    pub fn snapshot(&self) -> CapturedOutput {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The error that stopped the drain before the end of the stream, taken once. `None` when
    /// the stream ended cleanly or is still open.
    pub fn take_error(&self) -> Option<io::Error> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Waits for the stream to end (the child exited and no grandchild holds the pipe), then
    /// returns the complete capture.
    pub fn finish(&mut self) -> CapturedOutput {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.snapshot()
    }

    /// Like [`finish`](Self::finish) but gives up after `timeout` (a grandchild that inherited
    /// the pipe keeps it open after the child is gone) and returns what was captured so far.
    /// Never joins the thread; [`finish`](Self::finish) can still do that later.
    pub fn finish_within(&mut self, timeout: Duration) -> CapturedOutput {
        {
            let guard = self.done.0.lock().unwrap_or_else(|e| e.into_inner());
            let _ = self
                .done
                .1
                .wait_timeout_while(guard, timeout, |over| !*over)
                .unwrap_or_else(|e| e.into_inner());
        }
        self.snapshot()
    }

    /// True once the stream has ended (or failed).
    pub fn is_finished(&self) -> bool {
        *self.done.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn keeps_bytes_exactly_including_trailing_newline_and_non_utf8() {
        let input: Vec<u8> = vec![b'{', b'}', b'\n', 0xff, 0x00, b'\r', b'\n'];
        let out = read_bounded(Cursor::new(input.clone()), 1024).unwrap();
        assert_eq!(out.bytes, input);
        assert!(!out.truncated);
        assert_eq!(out.total_bytes, input.len() as u64);
    }

    #[test]
    fn keeps_the_first_max_bytes_and_counts_the_rest() {
        let input = vec![7u8; CHUNK * 3 + 5];
        let out = read_bounded(Cursor::new(input), 10).unwrap();
        assert_eq!(out.bytes, vec![7u8; 10]);
        assert!(out.truncated);
        assert_eq!(out.total_bytes, (CHUNK * 3 + 5) as u64);
    }

    #[test]
    fn exactly_the_cap_is_not_truncated() {
        let out = read_bounded(Cursor::new(vec![1u8; 64]), 64).unwrap();
        assert_eq!(out.bytes.len(), 64);
        assert!(!out.truncated);
        let zero = read_bounded(Cursor::new(vec![1u8; 3]), 0).unwrap();
        assert!(zero.bytes.is_empty() && zero.truncated);
    }

    #[test]
    fn a_read_error_is_returned_by_the_blocking_reader() {
        struct Bad;
        impl Read for Bad {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
        }
        assert!(read_bounded(Bad, 8).is_err());
    }

    #[test]
    fn the_background_capture_matches_the_blocking_reader() {
        let input: Vec<u8> = (0..=255u8).cycle().take(CHUNK * 2 + 17).collect();
        let mut capture = OutputCapture::capture(Cursor::new(input.clone()), 1000);
        let out = capture.finish();
        assert_eq!(out.bytes, input[..1000].to_vec());
        assert!(out.truncated);
        assert_eq!(out.total_bytes, input.len() as u64);
        assert!(capture.is_finished());
        assert!(capture.take_error().is_none());
    }

    #[test]
    fn the_background_capture_records_a_read_error() {
        struct Bad(bool);
        impl Read for Bad {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("boom"));
                }
                self.0 = true;
                buf[..2].copy_from_slice(b"ok");
                Ok(2)
            }
        }
        let mut capture = OutputCapture::capture(Bad(false), 8);
        let out = capture.finish();
        assert_eq!(out.bytes, b"ok");
        assert!(capture.take_error().is_some());
    }

    #[test]
    fn finish_within_returns_early_data_while_the_stream_stays_open() {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        use std::io::Write;
        writer.write_all(b"partial").unwrap();
        let mut capture = OutputCapture::capture(reader, 64);
        let out = capture.finish_within(Duration::from_millis(300));
        assert!(!capture.is_finished());
        assert!(out.bytes.is_empty() || out.bytes == b"partial");
        drop(writer);
        let out = capture.finish();
        assert_eq!(out.bytes, b"partial");
        assert!(!out.truncated);
    }
}
