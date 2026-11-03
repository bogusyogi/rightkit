//! Idle (stall) limit for streamed bodies: fail a read when no bytes arrive for a
//! configured time, independent of the header and total deadlines.
//!
//! The blocking read runs on one helper thread per stream; on a stall the helper is
//! abandoned and ends when its read returns, errors, or the consumer drops the reader
//! (the next send fails). Socket-level timeouts still bound the helper.

use std::io::{self, Read};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::cancel::{cancelled_io, CancelToken};

/// How often a waiting read re-checks cancellation.
const POLL: Duration = Duration::from_millis(50);

pub(crate) struct IdleReader {
    chunks: mpsc::Receiver<io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    at: usize,
    idle: Duration,
    cancel: Option<CancelToken>,
    done: bool,
}

impl IdleReader {
    pub(crate) fn new(
        mut inner: Box<dyn Read + Send>,
        idle: Duration,
        cancel: Option<CancelToken>,
    ) -> Self {
        let (sender, chunks) = mpsc::sync_channel(2);
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let item = match inner.read(&mut buffer) {
                    Ok(0) => Ok(Vec::new()),
                    Ok(n) => Ok(buffer[..n].to_vec()),
                    Err(error) => Err(error),
                };
                let last = !matches!(&item, Ok(chunk) if !chunk.is_empty());
                if sender.send(item).is_err() || last {
                    return;
                }
            }
        });
        Self {
            chunks,
            pending: Vec::new(),
            at: 0,
            idle,
            cancel,
            done: false,
        }
    }
}

impl Read for IdleReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.pending.len() {
            if self.done {
                return Ok(0);
            }
            let started = Instant::now();
            loop {
                if self.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                    self.done = true;
                    return Err(cancelled_io());
                }
                let left = self.idle.saturating_sub(started.elapsed());
                if left.is_zero() {
                    self.done = true;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("no data received for {} ms", self.idle.as_millis()),
                    ));
                }
                match self.chunks.recv_timeout(left.min(POLL)) {
                    Ok(Ok(chunk)) if chunk.is_empty() => {
                        self.done = true;
                        return Ok(0);
                    }
                    Ok(Ok(chunk)) => {
                        self.pending = chunk;
                        self.at = 0;
                        break;
                    }
                    Ok(Err(error)) => {
                        self.done = true;
                        return Err(error);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.done = true;
                        return Ok(0);
                    }
                }
            }
        }
        let n = out.len().min(self.pending.len() - self.at);
        out[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}
