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

use serde::{de::DeserializeOwned, Serialize};

use crate::FrameError;

/// Serialize `value` and write it as one length-prefixed frame. Nothing is
/// written when the encoding is empty or exceeds `limit`.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T, limit: usize) -> Result<(), FrameError> {
    let body = serde_json::to_vec(value)?;
    write_frame_bytes(writer, &body, limit)
}

/// As [`write_frame`] for an already-encoded body.
pub fn write_frame_bytes<W: Write>(writer: &mut W, body: &[u8], limit: usize) -> Result<(), FrameError> {
    if body.is_empty() {
        return Err(FrameError::Io(io::Error::new(io::ErrorKind::InvalidData, "empty frame body")));
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
pub fn read_frame_bytes<R: Read>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>, FrameError> {
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
        return Err(FrameError::Io(io::Error::new(io::ErrorKind::InvalidData, "zero-length frame")));
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

/// Read and deserialize one frame.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R, limit: usize) -> Result<Option<T>, FrameError> {
    match read_frame_bytes(reader, limit)? {
        None => Ok(None),
        Some(body) => Ok(Some(serde_json::from_slice(&body)?)),
    }
}
