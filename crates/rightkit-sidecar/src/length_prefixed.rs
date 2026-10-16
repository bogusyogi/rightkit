//! Length-prefixed JSON framing: a 4-byte big-endian length, then that many
//! bytes of UTF-8 JSON. This is Cockpit's IPC framing, kept for peers that
//! must carry newlines or arbitrary bytes inside a frame and for strict
//! one-request/one-response exchanges.
//!
//! Rules (all enforced before any body byte is read or written):
//! - A zero length is malformed ([`FrameError::Io`] of kind `InvalidData`).
//! - A length above `limit` is [`FrameError::TooLarge`]. The body is **not**
//!   consumed, so the stream is no longer on a frame boundary: the caller must
//!   close the connection (unlike JSON lines, there is no resynchronisation).
//! - The body buffer grows with the bytes that actually arrive, so a peer that
//!   announces `limit` bytes and sends none cannot make the reader allocate
//!   `limit` up front.
//! - A stream that ends inside a length or a body is `UnexpectedEof`; an end
//!   exactly on a frame boundary is a clean `Ok(None)`.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use serde::{de::DeserializeOwned, Serialize};

use crate::FrameError;

/// Serialize `value` and write it as one length-prefixed frame. Nothing is
/// written when the encoding is empty or exceeds `limit`.
pub fn write_frame<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
    limit: usize,
) -> Result<(), FrameError> {
    let body = serde_json::to_vec(value)?;
    write_frame_bytes(writer, &body, limit)
}

/// As [`write_frame`] for an already-encoded body.
pub fn write_frame_bytes<W: Write>(
    writer: &mut W,
    body: &[u8],
    limit: usize,
) -> Result<(), FrameError> {
    if body.is_empty() {
        return Err(FrameError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty frame body",
        )));
    }
    if body.len() > limit || u32::try_from(body.len()).is_err() {
        return Err(FrameError::TooLarge { limit });
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Read one frame body. `Ok(None)` on a clean end of stream between frames.
pub fn read_frame_bytes<R: Read>(
    reader: &mut R,
    limit: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut len = [0u8; 4];
    let mut filled = 0;
    while filled < 4 {
        match reader.read(&mut len[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Io(io::ErrorKind::UnexpectedEof.into())),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 {
        return Err(FrameError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero-length frame",
        )));
    }
    if len > limit {
        return Err(FrameError::TooLarge { limit });
    }
    let mut body = Vec::new();
    let got = reader.take(len as u64).read_to_end(&mut body)?;
    if got < len {
        return Err(FrameError::Io(io::ErrorKind::UnexpectedEof.into()));
    }
    Ok(Some(body))
}

/// Readiness callback for the `_deadline` functions below.
///
/// Called with the time left until the deadline (never zero). It waits up to that long for
/// the stream to become readable (for a read) or writable (for a write) and returns
/// `Ok(true)` when it is, `Ok(false)` when the wait ended first (the caller loop checks the
/// deadline again), and `Err` to fail the call. A transport that is already nonblocking
/// passes a `poll`/`WaitForSingleObject` wrapper; a transport whose reads and writes block
/// with their own timeout can sleep zero and return `Ok(true)`.
pub type Ready<'a> = &'a mut dyn FnMut(Duration) -> io::Result<bool>;

fn timed_out(partial_bytes: usize) -> FrameError {
    FrameError::TimedOut { partial_bytes }
}

fn eof(message: &'static str) -> FrameError {
    FrameError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, message))
}

/// Read exactly `buf.len()` bytes before `deadline`, waiting through `ready`. A stream that
/// ends first is `Err(ReadStop::End(bytes_read))`.
fn fill_deadline<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    deadline: Instant,
    ready: Ready<'_>,
) -> Result<usize, ReadStop> {
    let mut filled = 0;
    while filled < buf.len() {
        let now = Instant::now();
        if now >= deadline {
            return Err(ReadStop::Deadline(filled));
        }
        if !ready(deadline - now).map_err(|e| ReadStop::Failed(e))? {
            continue;
        }
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Err(ReadStop::End(filled)),
            Ok(n) => filled += n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => return Err(ReadStop::Failed(e)),
        }
    }
    Ok(filled)
}

enum ReadStop {
    /// The deadline passed after this many bytes of the unit.
    Deadline(usize),
    /// The stream ended after this many bytes of the unit.
    End(usize),
    Failed(io::Error),
}

/// As [`read_frame_bytes`], bounded by the absolute `deadline`, over any `Read` that has
/// a readiness wait (`ready`, see [`Ready`]).
///
/// Error mapping (the same as the descriptor-based `deadline::read_frame_deadline`):
///
/// - deadline reached: [`FrameError::TimedOut`] with the bytes of this frame (length
///   prefix included) already consumed. `partial_bytes: 0` leaves the stream on a frame
///   boundary; more than zero leaves it mid-frame and it must be closed;
/// - end of stream, even exactly between frames: [`FrameError::Io`] of kind
///   `UnexpectedEof` (this function returns a frame or an error, never `Ok(None)`; a caller
///   that treats a clean close as normal checks the kind and that no bytes were consumed
///   by seeing the message `peer closed before a frame header`);
/// - declared length zero: `Io` of kind `InvalidData`;
/// - declared length above `limit`: [`FrameError::TooLarge`], body not consumed, close the
///   stream;
/// - any `Io` error from `ready` or the read, other than `Interrupted`/`WouldBlock`
///   (which retry until the deadline).
pub fn read_frame_bytes_deadline<R: Read>(
    reader: &mut R,
    limit: usize,
    deadline: Instant,
    ready: Ready<'_>,
) -> Result<Vec<u8>, FrameError> {
    let mut header = [0u8; 4];
    match fill_deadline(reader, &mut header, deadline, &mut *ready) {
        Ok(_) => {}
        Err(ReadStop::Deadline(n)) => return Err(timed_out(n)),
        Err(ReadStop::End(0)) => return Err(eof("peer closed before a frame header")),
        Err(ReadStop::End(_)) => return Err(eof("peer closed inside a frame header")),
        Err(ReadStop::Failed(e)) => return Err(FrameError::Io(e)),
    }
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 {
        return Err(FrameError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero-length frame",
        )));
    }
    if length > limit {
        return Err(FrameError::TooLarge { limit });
    }
    // Grow with the bytes that arrive, as `read_frame_bytes` does.
    const CHUNK: usize = 64 * 1024;
    let mut body: Vec<u8> = Vec::new();
    while body.len() < length {
        let start = body.len();
        let want = (length - start).min(CHUNK);
        body.resize(start + want, 0);
        match fill_deadline(reader, &mut body[start..], deadline, &mut *ready) {
            Ok(_) => {}
            Err(ReadStop::Deadline(n)) => return Err(timed_out(header.len() + start + n)),
            Err(ReadStop::End(_)) => return Err(eof("peer closed mid-frame")),
            Err(ReadStop::Failed(e)) => return Err(FrameError::Io(e)),
        }
    }
    Ok(body)
}

/// As [`write_frame_bytes`], bounded by the absolute `deadline`, over any `Write` that
/// has a readiness wait (`ready`, see [`Ready`]; it waits for the stream to be writable).
///
/// Nothing is written for an empty body (`Io` of kind `InvalidData`) or one above `limit`
/// ([`FrameError::TooLarge`]). A deadline reached mid-frame is [`FrameError::TimedOut`]
/// with the bytes of the frame (prefix included) the peer already received; more than
/// zero leaves the peer mid-frame and the stream must be closed. A write that accepts zero
/// bytes is `Io` of kind `WriteZero`. `Interrupted` and `WouldBlock` retry until the
/// deadline.
pub fn write_frame_bytes_deadline<W: Write>(
    writer: &mut W,
    body: &[u8],
    limit: usize,
    deadline: Instant,
    ready: Ready<'_>,
) -> Result<(), FrameError> {
    if body.is_empty() {
        return Err(FrameError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty frame body",
        )));
    }
    if body.len() > limit || u32::try_from(body.len()).is_err() {
        return Err(FrameError::TooLarge { limit });
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    let mut sent = 0;
    while sent < frame.len() {
        let now = Instant::now();
        if now >= deadline {
            return Err(timed_out(sent));
        }
        if !ready(deadline - now)? {
            continue;
        }
        match writer.write(&frame[sent..]) {
            Ok(0) => {
                return Err(FrameError::Io(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "stream accepted zero bytes of a frame",
                )))
            }
            Ok(n) => sent += n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    // Flushing a nonblocking stream may report WouldBlock; the bytes are already accepted.
    match writer.flush() {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(e) => Err(FrameError::Io(e)),
    }
}

/// Read and deserialize one frame.
pub fn read_frame<R: Read, T: DeserializeOwned>(
    reader: &mut R,
    limit: usize,
) -> Result<Option<T>, FrameError> {
    match read_frame_bytes(reader, limit)? {
        None => Ok(None),
        Some(body) => Ok(Some(serde_json::from_slice(&body)?)),
    }
}
