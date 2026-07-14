//! Bounded JSON-lines transport primitives for Right Suite sidecars.
//!
//! This crate is intentionally minimal: it is a *transport* layer. Product
//! schemas and behaviour stay in the owning application; this crate only
//! guarantees bounded-size, newline-delimited JSON frames with strict
//! resynchronisation semantics.
//!
//! Contract:
//! - `read_json_line` skips leading blank lines (including `\r\n`-only lines),
//!   reads up to one newline-terminated frame, validates the frame fits within
//!   `limit` bytes (excluding the trailing newline), and deserializes it.
//!   On an oversized frame the entire frame (up to and including the next
//!   newline) is consumed before returning [`FrameError::TooLarge`], so the
//!   next call resumes on a clean boundary.
//! - `write_json_line` measures the encoded length first; if the encoded
//!   payload (without the trailing `\n`) exceeds `limit`, nothing is written
//!   to the destination and [`FrameError::TooLarge`] is returned.
//! - On malformed JSON, [`FrameError::Json`] is returned after the offending
//!   line has been consumed (up to and including the newline), so the next
//!   call resumes on a clean boundary.

use std::io::{self, BufRead, Write};

use serde::{de::DeserializeOwned, Serialize};

/// Errors that can occur while reading or writing a bounded JSON frame.
#[derive(Debug)]
pub enum FrameError {
    /// The encoded frame exceeded the supplied byte limit.
    TooLarge {
        /// The byte limit that was exceeded.
        limit: usize,
    },
    /// The frame was syntactically valid bytes but failed to deserialize as
    /// the requested type.
    Json(serde_json::Error),
    /// The underlying I/O stream failed.
    Io(io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge { limit } => {
                write!(f, "json frame exceeded {limit}-byte limit")
            }
            FrameError::Json(err) => write!(f, "json frame failed to deserialize: {err}"),
            FrameError::Io(err) => write!(f, "json frame i/o error: {err}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FrameError::TooLarge { .. } => None,
            FrameError::Json(err) => Some(err),
            FrameError::Io(err) => Some(err),
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(err: io::Error) -> Self {
        FrameError::Io(err)
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(err: serde_json::Error) -> Self {
        FrameError::Json(err)
    }
}

/// Read and deserialize exactly one JSON frame from `reader`, bounded by
/// `limit` bytes (excluding the trailing newline). Blank lines (including
/// `\r\n`-only lines) are skipped.
///
/// Returns:
/// - `Ok(Some(value))` when a frame was read and deserialized.
/// - `Ok(None)` on clean EOF (no partial frame buffered).
/// - `Err(FrameError::TooLarge { limit })` when the frame exceeded the limit;
///   the oversized bytes (up to and including the newline) are consumed
///   before the error is returned so the next call resumes on a clean
///   boundary.
/// - `Err(FrameError::Json(_))` when the bytes parsed as JSON but failed to
///   deserialize as `T`; the malformed line is consumed before the error
///   is returned.
pub fn read_json_line<R, T>(reader: &mut R, buffer: &mut Vec<u8>, limit: usize) -> Result<Option<T>, FrameError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    buffer.clear();
    loop {
        let consumed = match reader.read_until(b'\n', buffer) {
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(FrameError::Io(err)),
        };
        if consumed == 0 {
            return Ok(None);
        }

        // Trim a single trailing newline + optional preceding CR, then check
        // whether the line is blank (e.g. an empty `\r\n` or `\n`).
        let mut end = buffer.len();
        if end > 0 && buffer[end - 1] == b'\n' {
            end -= 1;
        }
        if end > 0 && buffer[end - 1] == b'\r' {
            end -= 1;
        }
        if end == 0 {
            buffer.clear();
            continue;
        }

        let frame_len = end;

        if frame_len > limit {
            // Consume the entire line (including the newline we already
            // read), drop the buffered bytes, and report the oversize error.
            // The next call will start on the byte after this line.
            buffer.clear();
            return Err(FrameError::TooLarge { limit });
        }

        let slice = &buffer[..frame_len];
        let value: T = serde_json::from_slice(slice)?;
        buffer.clear();
        return Ok(Some(value));
    }
}

/// Serialize `value` as a compact single-line JSON frame terminated by `\n`
/// and write it to `writer`. If the encoded payload (excluding the trailing
/// newline) exceeds `limit` bytes, no bytes are written and
/// [`FrameError::TooLarge`] is returned.
pub fn write_json_line<W, T>(writer: &mut W, value: &T, limit: usize) -> Result<(), FrameError>
where
    W: Write,
    T: Serialize,
{
    let encoded = serde_json::to_vec(value)?;
    if encoded.len() > limit {
        return Err(FrameError::TooLarge { limit });
    }

    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufReader, Cursor};

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Sample {
        id: u64,
        value: String,
    }

    #[test]
    fn write_then_read_round_trips_within_limit() {
        let mut encoded = Vec::new();
        write_json_line(
            &mut encoded,
            &Sample {
                id: 1,
                value: "hello".into(),
            },
            128,
        )
        .unwrap();

        assert!(encoded.ends_with(b"\n"));

        let mut reader = BufReader::new(Cursor::new(encoded));
        let mut buffer = Vec::new();
        let decoded: Sample = read_json_line(&mut reader, &mut buffer, 128)
            .unwrap()
            .unwrap();
        assert_eq!(
            decoded,
            Sample {
                id: 1,
                value: "hello".into(),
            }
        );
    }

    #[test]
    fn blank_lines_are_skipped_until_eof() {
        let input = b"\n\n\r\n{\"id\":2,\"value\":\"ok\"}\n";
        let mut reader = BufReader::new(Cursor::new(input));
        let mut buffer = Vec::new();
        let decoded: Sample = read_json_line(&mut reader, &mut buffer, 128)
            .unwrap()
            .unwrap();
        assert_eq!(decoded.id, 2);
        let eof = read_json_line::<_, Sample>(&mut reader, &mut buffer, 128).unwrap();
        assert_eq!(eof, None);
    }
}