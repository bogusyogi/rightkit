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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Stall(Duration);
    impl Read for Stall {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(self.0);
            Ok(0)
        }
    }

    #[test]
    fn a_stalled_read_times_out_with_timed_out() {
        let mut r = IdleReader::new(
            Box::new(Stall(Duration::from_millis(800))),
            Duration::from_millis(120),
            None,
        );
        let err = r.read(&mut [0u8; 8]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_prompt_body_is_passed_through() {
        let mut r = IdleReader::new(
            Box::new(Cursor::new(b"hello world".to_vec())),
            Duration::from_secs(5),
            None,
        );
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello world");
    }

    #[test]
    fn cancellation_ends_a_waiting_read() {
        let token = CancelToken::new();
        let mut r = IdleReader::new(
            Box::new(Stall(Duration::from_millis(1500))),
            Duration::from_secs(30),
            Some(token.clone()),
        );
        token.cancel();
        let err = r.read(&mut [0u8; 8]).unwrap_err();
        assert!(crate::cancel::is_cancelled_io(&err));
    }
}
