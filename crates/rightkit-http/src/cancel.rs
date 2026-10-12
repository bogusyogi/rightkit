//! Cooperative cancellation shared by buffered calls and streams.
//!
//! A [`CancelToken`] is checked between attempts, while sleeping between retries,
//! and before every body read. It cannot interrupt a blocking socket call that is
//! already waiting for response headers; that wait is bounded by the response timeout.

use std::fmt;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Shared cancel flag. Clones observe the same flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    /// Request cancellation. Calls in flight stop at their next check point.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    /// The underlying flag, for APIs that take `&AtomicBool`.
    pub fn as_atomic(&self) -> &AtomicBool {
        &self.flag
    }
}

/// Marker carried inside the `io::Error` produced by a cancelled read.
#[derive(Debug)]
pub(crate) struct CancelledIo;

impl fmt::Display for CancelledIo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("request cancelled")
    }
}

impl std::error::Error for CancelledIo {}

pub(crate) fn cancelled_io() -> io::Error {
    io::Error::other(CancelledIo)
}

/// True when `err` came from a cancelled read on a stream returned by this crate.
pub fn is_cancelled_io(err: &io::Error) -> bool {
    err.get_ref().is_some_and(|inner| inner.is::<CancelledIo>())
}

pub(crate) fn is_set(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::SeqCst))
}

/// Sleep for `total`, waking every 50 ms to observe cancellation.
/// Returns `false` when cancelled before the delay elapsed.
pub(crate) fn sleep_cancellable(total: Duration, cancel: Option<&AtomicBool>) -> bool {
    let until = Instant::now() + total;
    loop {
        if is_set(cancel) {
            return false;
        }
        let now = Instant::now();
        if now >= until {
            return true;
        }
        std::thread::sleep((until - now).min(Duration::from_millis(50)));
    }
}

/// Reader that refuses to read once the flag is set.
pub(crate) struct CheckedReader<'a> {
    pub(crate) inner: &'a mut dyn Read,
    pub(crate) cancel: Option<&'a AtomicBool>,
}

impl Read for CheckedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if is_set(self.cancel) {
            return Err(cancelled_io());
        }
        self.inner.read(buf)
    }
}
