//! Tokio readers and writers for the bounded JSON-lines frames of the crate root (feature
//! `tokio`). They run the same line machine as the blocking [`read_json_line`](crate::read_json_line)
//! family, so the semantics are identical: blank lines are skipped under [`ReadOptions`], at
//! most `limit + 1` bytes of a frame are ever buffered, an oversized line is drained through
//! its newline (never stored) so the next call resumes on a clean boundary, a peer that never
//! sends a newline is cut off after [`ReadLimits::max_discard`](crate::ReadLimits) bytes, a
//! malformed line is consumed and its raw bytes stay in `buffer`, and a write measures the
//! encoded frame first and writes nothing when it is over `limit`.
//!
//! Differences that follow from async:
//!
//! - [`ReadLimits::deadline`](crate::ReadLimits) is checked between chunks, as in the
//!   blocking reader; a single pending read is not interrupted. Wrap the call in
//!   `tokio::time::timeout` to bound it.
//! - A read future dropped mid-line (for example by that timeout) has consumed part of the
//!   line and keeps its bytes in `buffer`; the stream is then no longer on a line boundary,
//!   so close it. Cancelling a read that is still waiting for its first byte loses nothing.

use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use std::io;
use std::time::Instant;

use crate::{
    check_encoded_line, deadline_error, decode_line, encode_json_line, AtEof, Finished, FrameError,
    Incoming, Line, LineScan, ReadOptions, ReceivedLine,
};

/// Async [`read_json_line`](crate::read_json_line): read and deserialize one JSON frame,
/// bounded by `limit` bytes (excluding the trailing newline), with default [`ReadOptions`].
///
/// - `Ok(Some(value))`: a frame was read and deserialized.
/// - `Ok(None)`: clean end of stream.
/// - `Err(FrameError::TooLarge { limit })`: the frame exceeded `limit`; it was discarded
///   through its newline without being stored. Past [`DEFAULT_MAX_DISCARD`](crate::DEFAULT_MAX_DISCARD)
///   without a newline the result is `FrameError::Io` of kind `InvalidData`.
/// - `Err(FrameError::Json(_))` / `Err(FrameError::InvalidUtf8 { .. })`: the line was consumed
///   and its raw bytes are left in `buffer`.
pub async fn read_json_line_async<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<T>, FrameError>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    read_json_line_async_opts(reader, buffer, limit, ReadOptions::default()).await
}

/// [`read_json_line_async`] with [`ReadOptions`]. An oversized frame is an error.
pub async fn read_json_line_async_opts<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
) -> Result<Option<T>, FrameError>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    match read_json_line_or_oversized_async_opts(reader, buffer, limit, opts).await? {
        None => Ok(None),
        Some(Incoming::Frame(value)) => Ok(Some(value)),
        Some(Incoming::Oversized(_)) => Err(FrameError::TooLarge { limit }),
    }
}

/// Async [`read_json_line_or_oversized_opts`](crate::read_json_line_or_oversized_opts): an
/// oversized frame is a value ([`Incoming::Oversized`], with the frame head) rather than an
/// error, so a supervisor can answer the call it belongs to and keep the child.
pub async fn read_json_line_or_oversized_async_opts<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
) -> Result<Option<Incoming<T>>, FrameError>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    match next_line_async(reader, buffer, limit, opts).await? {
        None => Ok(None),
        Some(Line::Oversized(over)) => Ok(Some(Incoming::Oversized(over))),
        Some(Line::Frame) => decode_line(buffer).map(|value| Some(Incoming::Frame(value))),
    }
}

/// Reads one line into `buffer`; on any error `buffer` is cleared first, as the blocking
/// reader does.
async fn next_line_async<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
) -> Result<Option<Line>, FrameError> {
    match next_line_async_inner(reader, buffer, limit, opts).await {
        Ok(line) => Ok(line),
        Err(err) => {
            buffer.clear();
            Err(err)
        }
    }
}

async fn next_line_async_inner<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
) -> Result<Option<Line>, FrameError> {
    let limits = opts.limits;
    let cap = limit.saturating_add(1);
    // The async readers have no observer; the shared line machine still calls one.
    let mut ignore = |_: ReceivedLine| {};
    'line: loop {
        buffer.clear();
        let mut scan = LineScan::new();
        loop {
            if limits.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(deadline_error());
            }
            let chunk = match reader.fill_buf().await {
                Ok(c) => c,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(FrameError::Io(err)),
            };
            if chunk.is_empty() {
                match scan.at_eof(buffer, &mut ignore) {
                    AtEof::Done(line) => return Ok(line),
                    AtEof::Finish => break,
                }
            }
            let (take, ended) = scan.absorb(chunk, buffer, limit, cap);
            reader.consume(take);
            if ended {
                break;
            }
            scan.check_discard(limit, &limits)?;
        }
        match scan.finish(buffer, limit, opts, &mut ignore)? {
            Finished::Skip => continue 'line,
            Finished::Done(line) => return Ok(Some(line)),
        }
    }
}

/// Async [`write_json_line`](crate::write_json_line): serialize `value` as one compact JSON
/// line and write it, without flushing. A payload over `limit` writes nothing
/// ([`FrameError::TooLarge`]); a payload with a newline is [`FrameError::EmbeddedNewline`].
pub async fn write_json_line_async<W, T>(
    writer: &mut W,
    value: &T,
    limit: usize,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let encoded = encode_json_line(value, limit)?;
    writer.write_all(&encoded).await?;
    Ok(())
}

/// [`write_json_line_async`] followed by a flush: use it when the peer waits for the frame
/// before it writes anything.
pub async fn write_json_line_flush_async<W, T>(
    writer: &mut W,
    value: &T,
    limit: usize,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    write_json_line_async(writer, value, limit).await?;
    writer.flush().await?;
    Ok(())
}

/// Async [`write_encoded_line`](crate::write_encoded_line): write bytes from
/// [`encode_json_line`], without flushing. Invalid bytes write nothing.
pub async fn write_encoded_line_async<W>(
    writer: &mut W,
    bytes: &[u8],
    limit: usize,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    check_encoded_line(bytes, limit)?;
    writer.write_all(bytes).await?;
    Ok(())
}

/// [`write_encoded_line_async`] followed by a flush.
pub async fn write_encoded_line_flush_async<W>(
    writer: &mut W,
    bytes: &[u8],
    limit: usize,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    write_encoded_line_async(writer, bytes, limit).await?;
    writer.flush().await?;
    Ok(())
}
