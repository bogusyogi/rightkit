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
//!   newline) is discarded as it streams in (never buffered beyond `limit + 1`
//!   bytes) before returning [`FrameError::TooLarge`], so the next call resumes
//!   on a clean boundary. A peer that never sends a newline is cut off after a
//!   bounded discard (see [`ReadLimits`]).
//! - `write_json_line` measures the encoded length first; if the encoded
//!   payload (without the trailing `\n`) exceeds `limit`, nothing is written
//!   to the destination and [`FrameError::TooLarge`] is returned.
//! - [`read_json_line_or_oversized`] returns an over-limit frame as a value
//!   ([`Incoming::Oversized`], with the frame head and [`Oversized::id`]) so a
//!   supervisor can answer the one call it belongs to and keep the child.
//! - [`length_prefixed`] is the binary-safe alternative framing (4-byte
//!   big-endian length + JSON body, Cockpit's IPC) and [`wire`] the one typed
//!   envelope (`hello`/`request`/`response`/`event`/`ready`/`bye`).
//! - On malformed JSON, [`FrameError::Json`] is returned after the offending
//!   line has been consumed (up to and including the newline), so the next
//!   call resumes on a clean boundary.

pub mod length_prefixed;
pub mod wire;

use std::io::{self, BufRead, Write};
use std::time::Instant;

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

/// Default bound on bytes thrown away while resynchronising after an
/// oversized frame (see [`ReadLimits`]).
pub const DEFAULT_MAX_DISCARD: usize = 64 * 1024 * 1024;

/// Extra bounds for [`read_json_line_with`].
#[derive(Debug, Clone, Copy)]
pub struct ReadLimits {
    /// Most bytes that may be discarded while skipping the rest of an oversized
    /// frame. A peer that never sends a newline gets
    /// `FrameError::Io(InvalidData)` once this is exceeded; the stream must then
    /// be considered broken.
    pub max_discard: usize,
    /// Checked between chunks; once passed, the read fails with
    /// `FrameError::Io(TimedOut)`. (A single blocked `read` cannot be
    /// interrupted by a generic `BufRead`; pair with a transport read timeout.)
    pub deadline: Option<Instant>,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_discard: DEFAULT_MAX_DISCARD,
            deadline: None,
        }
    }
}

/// Read and deserialize exactly one JSON frame from `reader`, bounded by
/// `limit` bytes (excluding the trailing newline). Blank lines (including
/// `\r\n`-only lines) are skipped.
///
/// The limit is enforced while consuming chunks: at most `limit + 1` bytes are
/// ever buffered, so a peer that streams bytes without a newline cannot
/// exhaust memory.
///
/// Returns:
/// - `Ok(Some(value))` when a frame was read and deserialized.
/// - `Ok(None)` on clean EOF (no partial frame buffered).
/// - `Err(FrameError::TooLarge { limit })` when the frame exceeded the limit;
///   the oversized bytes (up to and including the newline) are discarded
///   without being stored, so the next call resumes on a clean boundary.
///   Discarding is bounded by [`DEFAULT_MAX_DISCARD`]; past that the result is
///   `FrameError::Io` of kind `InvalidData`.
/// - `Err(FrameError::Json(_))` when the bytes parsed as JSON but failed to
///   deserialize as `T`; the malformed line is consumed before the error
///   is returned.
pub fn read_json_line<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<T>, FrameError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    read_json_line_with(reader, buffer, limit, ReadLimits::default())
}

/// [`read_json_line`] with explicit discard and deadline bounds.
pub fn read_json_line_with<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    limits: ReadLimits,
) -> Result<Option<T>, FrameError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    match read_json_line_or_oversized(reader, buffer, limit, limits)? {
        None => Ok(None),
        Some(Incoming::Frame(value)) => Ok(Some(value)),
        Some(Incoming::Oversized(_)) => Err(FrameError::TooLarge { limit }),
    }
}

/// Bytes of an oversized frame kept so the caller can still identify it
/// (for example the `"id"` of an over-limit response); see [`Oversized::head`].
pub const OVERSIZE_HEAD_BYTES: usize = 4096;

/// An over-limit frame that was drained without being stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Oversized {
    /// The byte limit that was exceeded.
    pub limit: usize,
    /// The first [`OVERSIZE_HEAD_BYTES`] bytes of the frame.
    pub head: Vec<u8>,
    /// Bytes of the frame seen (excluding the newline) before the drain ended.
    pub bytes: usize,
}

impl Oversized {
    /// The `"id"` of an oversized `response`/`event` frame, read from its head
    /// without parsing the whole frame (ScrapeRight answers the waiting call
    /// with `frame_too_large` instead of killing a healthy engine).
    pub fn id(&self) -> Option<u64> {
        peek_id(&self.head)
    }
}

/// One incoming line: a decoded frame, or an over-limit frame that was drained.
#[derive(Debug)]
pub enum Incoming<T> {
    Frame(T),
    Oversized(Oversized),
}

/// Pull `"id":<digits>` out of the leading bytes of a frame. Cheap and
/// forgiving by design: it exists only for frames too large to parse.
pub fn peek_id(head: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(&head[..head.len().min(OVERSIZE_HEAD_BYTES)])
        .or_else(|e| std::str::from_utf8(&head[..e.valid_up_to()]))
        .ok()?;
    let key = "\"id\":";
    let start = text.find(key)? + key.len();
    let digits: String = text[start..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// As [`read_json_line_with`], but an oversized frame is a value
/// ([`Incoming::Oversized`], carrying the frame head) rather than an error, so
/// a supervisor can answer the specific call it belongs to and keep going.
pub fn read_json_line_or_oversized<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    limits: ReadLimits,
) -> Result<Option<Incoming<T>>, FrameError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    // One CR may precede the newline, so `limit + 1` content bytes is the most
    // a valid frame can occupy before its terminator.
    let cap = limit.saturating_add(1);
    'line: loop {
        buffer.clear();
        let mut overflow: Option<Oversized> = None;
        let mut discarded = 0usize;
        loop {
            if limits.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(FrameError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "json frame deadline exceeded",
                )));
            }
            let chunk = match reader.fill_buf() {
                Ok(c) => c,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(FrameError::Io(err)),
            };
            if chunk.is_empty() {
                // EOF: a final unterminated line is still a frame.
                if let Some(over) = overflow {
                    return Ok(Some(Incoming::Oversized(over)));
                }
                if buffer.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let newline = chunk.iter().position(|b| *b == b'\n');
            let take = newline.map_or(chunk.len(), |i| i + 1);
            let content = &chunk[..newline.unwrap_or(take)];
            if let Some(over) = overflow.as_mut() {
                discarded = discarded.saturating_add(take);
                over.bytes = over.bytes.saturating_add(content.len());
            } else if buffer.len() + content.len() > cap {
                let seen = buffer.len() + content.len();
                let mut head = std::mem::take(buffer);
                head.truncate(OVERSIZE_HEAD_BYTES);
                let room = OVERSIZE_HEAD_BYTES - head.len();
                head.extend_from_slice(&content[..room.min(content.len())]);
                overflow = Some(Oversized {
                    limit,
                    head,
                    bytes: seen,
                });
                discarded = take;
            } else {
                buffer.extend_from_slice(content);
            }
            reader.consume(take);
            if newline.is_some() {
                break;
            }
            if overflow.is_some() && discarded > limits.max_discard {
                return Err(FrameError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "json frame exceeded {limit}-byte limit and no newline within {} bytes",
                        limits.max_discard
                    ),
                )));
            }
        }
        if let Some(over) = overflow {
            buffer.clear();
            return Ok(Some(Incoming::Oversized(over)));
        }
        let mut end = buffer.len();
        if end > 0 && buffer[end - 1] == b'\r' {
            end -= 1;
        }
        if end == 0 {
            continue 'line;
        }
        if end > limit {
            let head = buffer[..end.min(OVERSIZE_HEAD_BYTES)].to_vec();
            buffer.clear();
            return Ok(Some(Incoming::Oversized(Oversized {
                limit,
                head,
                bytes: end,
            })));
        }
        let value: T = serde_json::from_slice(&buffer[..end])?;
        buffer.clear();
        return Ok(Some(Incoming::Frame(value)));
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
