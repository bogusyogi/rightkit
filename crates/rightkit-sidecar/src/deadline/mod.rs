//! Deadline-bounded length-prefixed framing for sidecars whose peers must not
//! hold a call open past a bound.
//!
//! The wire format is the one [`length_prefixed`](crate::length_prefixed)
//! uses: a 4-byte big-endian length, then that many bytes of body. The body is
//! opaque here; callers encode it (JSON, a [`wire::Frame`](crate::wire::Frame)
//! or anything else) and a peer using the blocking codec reads it unchanged.
//!
//! Every call takes one absolute [`Instant`]. Partial transfers are resumed
//! until that instant, and a transfer that is still incomplete at the deadline
//! returns [`FrameError::TimedOut`] with the bytes already moved.
//!
//! # Descriptor requirements
//!
//! - **Unix**: the descriptor must be nonblocking (`O_NONBLOCK`). The
//!   functions check the flag and fail with `io::ErrorKind::InvalidInput` on a
//!   blocking descriptor, because a blocking `write` can hold the caller past
//!   the deadline. They never change the flag; [`DeadlineFramed::new`] sets it.
//! - **Windows**: the handle must be opened with `FILE_FLAG_OVERLAPPED`, as
//!   named-pipe ends created that way. A synchronous handle blocks in the
//!   kernel and the deadline does not apply. The crate cannot check this, so
//!   the caller owns it.
//!
//! # Stream state after an error
//!
//! - `TimedOut { partial_bytes: 0 }`: nothing of the frame moved. The stream is
//!   still on a frame boundary, and a later call may proceed.
//! - `TimedOut { partial_bytes: n }` with `n > 0`: `n` bytes of the frame moved.
//!   On a read those bytes are consumed and not replayed; on a write the peer
//!   received them. The stream is mid-frame and must be closed.
//! - `TooLarge` from a read: the declared length exceeds `max_len`. The body is
//!   not consumed, so the stream must be closed (as with `length_prefixed`).
//! - `TooLarge` from a write, or an empty body: nothing was written.
//! - `Io` of kind `UnexpectedEof`: the peer closed. An end exactly on a frame
//!   boundary is reported the same way, because these functions return a frame
//!   or an error and never `Ok(None)`.
//! - `Io` of kind `TimedOut` (Windows only): cancellation of an overlapped
//!   operation was not confirmed within a bounded drain. The handle must be
//!   discarded, and the state of its transfer is unknown.
//!
//! # Platform notes
//!
//! Unix uses `poll(2)` with millisecond slices rounded up. Windows waits with
//! `WaitForSingleObject` on a per-transfer event, cancels with `CancelIoEx`
//! when the deadline passes, and waits a bounded second for the kernel to
//! release the buffer. A context the kernel does not release is leaked, not
//! freed.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

use std::io;
use std::time::Instant;

use crate::FrameError;

/// Staging size for one overlapped transfer and for body growth on read.
const CHUNK: usize = 64 * 1024;

/// Outcome of one transfer attempt on a descriptor or handle.
enum Transfer {
    /// `n` bytes moved. `Bytes(0)` on a read is end of stream.
    Bytes(usize),
    /// The deadline passed before any byte moved in this attempt.
    Timeout,
}

/// Reads one length-prefixed frame from `source` and returns its body.
///
/// `max_len` bounds the declared body length. `deadline` is absolute: a peer
/// that trickles bytes cannot extend it. The descriptor requirements are in the
/// module documentation. A frame is never returned partially.
///
/// Takes a shared borrow, not a raw descriptor: the owner stays alive for the
/// whole call, so the descriptor cannot be closed underneath it.
#[cfg(unix)]
pub fn read_frame_deadline<T: AsRawFd + ?Sized>(
    source: &T,
    deadline: Instant,
    max_len: usize,
) -> Result<Vec<u8>, FrameError> {
    read_frame_raw(source.as_raw_fd(), deadline, max_len)
}

/// Reads one length-prefixed frame from `source` and returns its body.
///
/// See the Unix variant. On Windows the handle must be overlapped.
#[cfg(windows)]
pub fn read_frame_deadline<T: AsRawHandle + ?Sized>(
    source: &T,
    deadline: Instant,
    max_len: usize,
) -> Result<Vec<u8>, FrameError> {
    read_frame_raw(source.as_raw_handle(), deadline, max_len)
}

/// Writes `body` to `sink` as one length-prefixed frame.
///
/// An empty body is rejected, and a body longer than `max_len` returns
/// [`FrameError::TooLarge`] before any byte is written. `deadline` is absolute.
/// A timeout reports how many bytes of the frame reached the peer.
#[cfg(unix)]
pub fn write_frame_deadline<T: AsRawFd + ?Sized>(
    sink: &T,
    body: &[u8],
    deadline: Instant,
    max_len: usize,
) -> Result<(), FrameError> {
    write_frame_raw(sink.as_raw_fd(), body, deadline, max_len)
}

/// Writes `body` to `sink` as one length-prefixed frame.
///
/// See the Unix variant. On Windows the handle must be overlapped.
#[cfg(windows)]
pub fn write_frame_deadline<T: AsRawHandle + ?Sized>(
    sink: &T,
    body: &[u8],
    deadline: Instant,
    max_len: usize,
) -> Result<(), FrameError> {
    write_frame_raw(sink.as_raw_handle(), body, deadline, max_len)
}

/// A stream that exchanges deadline-bounded frames.
///
/// The wrapper owns the stream. [`DeadlineFramed::new`] puts a Unix descriptor
/// in nonblocking mode, which is the one state change made on the caller's
/// behalf; a stream wrapped here should not also be used for blocking I/O.
#[derive(Debug)]
pub struct DeadlineFramed<T> {
    inner: T,
}

impl<T> DeadlineFramed<T> {
    /// Borrows the wrapped stream.
    pub fn get_ref(&self) -> &T {
        &self.inner
    }

    /// Mutably borrows the wrapped stream.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Returns the wrapped stream.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

#[cfg(unix)]
impl<T: AsRawFd> DeadlineFramed<T> {
    /// Wraps `inner`, setting `O_NONBLOCK` on its descriptor.
    pub fn new(inner: T) -> io::Result<Self> {
        sys::set_nonblocking(inner.as_raw_fd())?;
        Ok(Self { inner })
    }

    /// Reads one frame. See [`read_frame_deadline`].
    pub fn read_frame(&mut self, deadline: Instant, max_len: usize) -> Result<Vec<u8>, FrameError> {
        read_frame_deadline(&self.inner, deadline, max_len)
    }

    /// Writes one frame. See [`write_frame_deadline`].
    pub fn write_frame(
        &mut self,
        body: &[u8],
        deadline: Instant,
        max_len: usize,
    ) -> Result<(), FrameError> {
        write_frame_deadline(&self.inner, body, deadline, max_len)
    }
}

#[cfg(windows)]
impl<T: AsRawHandle> DeadlineFramed<T> {
    /// Wraps `inner`. The handle must already be overlapped; nothing is changed.
    pub fn new(inner: T) -> io::Result<Self> {
        Ok(Self { inner })
    }

    /// Reads one frame. See [`read_frame_deadline`].
    pub fn read_frame(&mut self, deadline: Instant, max_len: usize) -> Result<Vec<u8>, FrameError> {
        read_frame_deadline(&self.inner, deadline, max_len)
    }

    /// Writes one frame. See [`write_frame_deadline`].
    pub fn write_frame(
        &mut self,
        body: &[u8],
        deadline: Instant,
        max_len: usize,
    ) -> Result<(), FrameError> {
        write_frame_deadline(&self.inner, body, deadline, max_len)
    }
}

fn eof(message: &'static str) -> FrameError {
    FrameError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, message))
}

fn read_frame_raw(raw: sys::Raw, deadline: Instant, max_len: usize) -> Result<Vec<u8>, FrameError> {
    sys::check_mode(raw)?;
    let mut header = [0u8; 4];
    let mut got = 0usize;
    while got < header.len() {
        match sys::read_some(raw, &mut header[got..], deadline)? {
            Transfer::Timeout => return Err(FrameError::TimedOut { partial_bytes: got }),
            Transfer::Bytes(0) => return Err(eof("peer closed before a frame header")),
            Transfer::Bytes(n) => got += n,
        }
    }

    let length = u32::from_be_bytes(header) as usize;
    if length == 0 {
        return Err(FrameError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero-length frame",
        )));
    }
    if length > max_len {
        return Err(FrameError::TooLarge { limit: max_len });
    }

    // Grow with the bytes that arrive, so a peer that declares `max_len` and
    // sends nothing cannot make us allocate it up front.
    let mut body: Vec<u8> = Vec::new();
    while body.len() < length {
        let start = body.len();
        let want = (length - start).min(CHUNK);
        body.resize(start + want, 0);
        match sys::read_some(raw, &mut body[start..], deadline)? {
            Transfer::Timeout => {
                return Err(FrameError::TimedOut {
                    partial_bytes: header.len() + start,
                })
            }
            Transfer::Bytes(0) => return Err(eof("peer closed mid-frame")),
            Transfer::Bytes(n) => body.truncate(start + n),
        }
    }
    Ok(body)
}

fn write_frame_raw(
    raw: sys::Raw,
    body: &[u8],
    deadline: Instant,
    max_len: usize,
) -> Result<(), FrameError> {
    if body.is_empty() {
        return Err(FrameError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty frame body",
        )));
    }
    let length = match u32::try_from(body.len()) {
        Ok(length) if body.len() <= max_len => length,
        _ => return Err(FrameError::TooLarge { limit: max_len }),
    };
    sys::check_mode(raw)?;

    let header = length.to_be_bytes();
    let total = header.len() + body.len();
    let mut sent = 0usize;
    while sent < total {
        let chunk: &[u8] = if sent < header.len() {
            &header[sent..]
        } else {
            &body[sent - header.len()..]
        };
        match sys::write_some(raw, chunk, deadline)? {
            Transfer::Timeout => {
                return Err(FrameError::TimedOut {
                    partial_bytes: sent,
                })
            }
            Transfer::Bytes(0) => {
                return Err(FrameError::Io(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "stream accepted zero bytes of a frame",
                )))
            }
            Transfer::Bytes(n) => sent += n,
        }
    }
    Ok(())
}
