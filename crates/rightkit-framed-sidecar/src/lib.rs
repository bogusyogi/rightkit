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
//! - [`deadline`] (added in 0.2.0) is the deadline-bounded length-prefixed
//!   framing that production sidecars use: [`read_frame_deadline`] and
//!   [`write_frame_deadline`] run over a raw descriptor (poll(2) on Unix) or
//!   handle (overlapped I/O on Windows), and report [`FrameError::TimedOut`]
//!   with the bytes already transferred. [`DeadlineFramed`] owns the stream.
//!   Available on Unix and Windows only. The blocking API above is unchanged.

#[cfg(any(unix, windows))]
pub mod deadline;
pub mod length_prefixed;
pub mod wire;

#[cfg(any(unix, windows))]
pub use deadline::{read_frame_deadline, write_frame_deadline, DeadlineFramed};

use std::io::{self, BufRead, BufReader, Read, Write};
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
    /// The bytes of a JSON line were not valid UTF-8. `len` is the line length
    /// excluding its newline. The line is consumed, so the next call resumes on
    /// a clean boundary.
    InvalidUtf8 {
        /// Length of the rejected line, excluding its newline.
        len: usize,
    },
    /// An empty line (zero bytes, or a single `\r`) was read while
    /// [`ReadOptions::skip_empty_lines`] is off. The line is consumed, so the next call
    /// resumes on a clean boundary.
    EmptyLine,
    /// A deadline-bounded transfer (see [`read_frame_deadline`] and
    /// [`write_frame_deadline`]) reached its deadline before the frame moved.
    ///
    /// `partial_bytes` counts the bytes of this frame, length prefix included,
    /// that were transferred before the deadline. Zero means the stream is
    /// still on a frame boundary; a non-zero count leaves it mid-frame, so the
    /// stream must be closed.
    TimedOut {
        /// Bytes of the current frame transferred before the deadline.
        partial_bytes: usize,
    },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge { limit } => {
                write!(f, "json frame exceeded {limit}-byte limit")
            }
            FrameError::Json(err) => write!(f, "json frame failed to deserialize: {err}"),
            FrameError::Io(err) => write!(f, "json frame i/o error: {err}"),
            FrameError::InvalidUtf8 { len } => {
                write!(f, "json frame of {len} bytes is not valid UTF-8")
            }
            FrameError::EmptyLine => write!(f, "json frame is an empty line"),
            FrameError::TimedOut { partial_bytes } => write!(
                f,
                "json frame deadline exceeded after {partial_bytes} bytes of the frame"
            ),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FrameError::TooLarge { .. }
            | FrameError::InvalidUtf8 { .. }
            | FrameError::EmptyLine
            | FrameError::TimedOut { .. } => None,
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
    /// be considered broken. `0` stops at the first chunk that crosses the cap:
    /// that chunk (at most the reader's buffer size) is consumed, and nothing
    /// further is read.
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

/// Which characters make a line blank under [`ReadOptions::skip_whitespace_only_lines`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WhitespaceSet {
    /// ASCII blank bytes only: space, tab, `\r`, vertical tab, form feed. Default.
    #[default]
    Ascii,
    /// Unicode whitespace, with the semantics of [`str::trim`]: a line is blank when
    /// `trim()` leaves nothing. That includes U+00A0 and U+2003. It excludes U+FEFF
    /// (the byte-order mark is not White_Space). A line that is not valid UTF-8 uses
    /// the ASCII rule, as it does under [`WhitespaceSet::Ascii`].
    Unicode,
}

/// How a received line ended.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    /// The line ended with `\n`, and the byte before it was not `\r`.
    Lf,
    /// The line ended with `\r\n`.
    CrLf,
    /// No terminator was read: the stream ended before the line's newline, so the line is
    /// unterminated. The reader consumes an oversized line through its newline, so an
    /// oversized line that has a newline reports `Lf` or `CrLf`; `None` never means "cut by
    /// the limit". A line whose discard budget runs out is an error, not an observation.
    None,
}

/// One received line, as reported to the observer of
/// [`read_json_line_or_oversized_observed`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceivedLine {
    /// Bytes read for the line, terminator included: `payload + 1` for `\n`,
    /// `payload + 2` for `\r\n`, and the bytes alone when `terminator` is
    /// [`Terminator::None`].
    pub raw_len: usize,
    /// How the line ended.
    pub terminator: Terminator,
}

impl ReceivedLine {
    /// A received line with the given raw byte count and terminator. Observers only read
    /// these values; this constructor lets tests and wrappers build one.
    pub fn new(raw_len: usize, terminator: Terminator) -> Self {
        Self {
            raw_len,
            terminator,
        }
    }
}

/// What the `limit` argument of the `_opts` readers is compared against.
///
/// The comparison is made after the line is read. A line whose count exceeds `limit` is
/// oversized. The count never includes bytes of the next line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LimitCounts {
    /// The payload: every byte before the line terminator. For a `\r\n` terminator the
    /// `\r` is not counted, so `payload` is the bytes before `\r\n`; for `\n` it is the
    /// bytes before `\n`. This is the behaviour before this option. Default.
    #[default]
    Payload,
    /// Every byte of the line as read, terminator included: `payload + 1` for `\n` and
    /// `payload + 2` for `\r\n`. An unterminated final line counts its bytes with no
    /// terminator. A line is accepted iff this count is `<= limit`, the same rule as
    /// reading with `read_line` over `take(limit)`.
    RawLineBytes,
}

/// Options for the `_opts` readers. The default reads like
/// [`read_json_line`]: only empty and `\r`-only lines are skipped.
#[derive(Debug, Clone, Copy)]
pub struct ReadOptions {
    /// Discard and deadline bounds, as for [`read_json_line_with`].
    pub limits: ReadLimits,
    /// Also skip blank lines, not just empty lines. Which characters count as blank is
    /// set by [`ReadOptions::whitespace`]. With the default [`WhitespaceSet::Ascii`],
    /// a line that holds any other byte, including non-ASCII whitespace, is still a frame.
    /// A whitespace-only line is not empty, so this option is independent of
    /// [`ReadOptions::skip_empty_lines`].
    pub skip_whitespace_only_lines: bool,
    /// The blank-line character set, read only when `skip_whitespace_only_lines` is
    /// set. The default, [`WhitespaceSet::Ascii`], is the behaviour before this option.
    pub whitespace: WhitespaceSet,
    /// Skip an empty line (zero bytes, or one `\r`). Default `true`, the behaviour before
    /// this option. With `false`, an empty line is [`FrameError::EmptyLine`]; the line is
    /// consumed, so a reader that requires the first frame to be a ready frame fails on a
    /// leading blank line, as HeardRight's ready reader does.
    pub skip_empty_lines: bool,
    /// What `limit` is compared against. Default [`LimitCounts::Payload`].
    pub limit_counts: LimitCounts,
}

impl Default for ReadOptions {
    /// The behaviour before the 0.2.0 options: skip empty lines, no whitespace skipping,
    /// ASCII whitespace set, payload counting.
    fn default() -> Self {
        Self {
            limits: ReadLimits::default(),
            skip_whitespace_only_lines: false,
            whitespace: WhitespaceSet::Ascii,
            skip_empty_lines: true,
            limit_counts: LimitCounts::Payload,
        }
    }
}

/// A `limit` that accepts a frame of any length. Use it only where the peer is
/// trusted and large frames are expected (for example engine stdin/stdout
/// carrying large results): memory then grows with the frame size. Other callers
/// should pass a bound.
pub const UNCAPPED: usize = usize::MAX;

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
    read_json_line_or_oversized_opts(
        reader,
        buffer,
        limit,
        ReadOptions {
            limits,
            ..ReadOptions::default()
        },
    )
}

/// [`read_json_line`] with [`ReadOptions`]. An oversized frame is an error.
pub fn read_json_line_opts<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
) -> Result<Option<T>, FrameError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    match read_json_line_or_oversized_opts(reader, buffer, limit, opts)? {
        None => Ok(None),
        Some(Incoming::Frame(value)) => Ok(Some(value)),
        Some(Incoming::Oversized(_)) => Err(FrameError::TooLarge { limit }),
    }
}

/// [`read_json_line_or_oversized`] with [`ReadOptions`].
///
/// A frame whose bytes are not valid UTF-8 returns [`FrameError::InvalidUtf8`]
/// after the line is consumed; malformed JSON returns [`FrameError::Json`].
pub fn read_json_line_or_oversized_opts<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
) -> Result<Option<Incoming<T>>, FrameError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    read_json_line_or_oversized_observed(reader, buffer, limit, opts, &mut |_| {})
}

/// [`read_json_line_or_oversized_opts`] that also reports every received line to
/// `observe` as a [`ReceivedLine`]: its raw byte count (terminator included) and how it
/// ended. `observe` is called once per line, before any skip or rejection decision: for
/// blank lines skipped under `skip_empty_lines` or `skip_whitespace_only_lines`, for
/// oversized lines, and for lines that then fail to parse. It is not called on clean EOF
/// or when a deadline fails mid-line.
///
/// `buffer` on return:
/// - a frame, or an oversized line: empty (the frame was parsed, or the line dropped);
/// - a JSON parse failure ([`FrameError::Json`]): the raw line, without its terminator and
///   without the `\r` of a `\r\n` terminator. The caller can read the request id from it
///   and reply to the malformed line. The next read clears it;
/// - invalid UTF-8 ([`FrameError::InvalidUtf8`]): the raw line, as for `Json`;
/// - clean EOF, [`FrameError::EmptyLine`], [`FrameError::Io`], or [`FrameError::TimedOut`]:
///   empty.
pub fn read_json_line_or_oversized_observed<R, T>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
    observe: &mut dyn FnMut(ReceivedLine),
) -> Result<Option<Incoming<T>>, FrameError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    match next_line(reader, buffer, limit, opts, observe)? {
        None => Ok(None),
        Some(Line::Oversized(over)) => Ok(Some(Incoming::Oversized(over))),
        Some(Line::Frame) => {
            if std::str::from_utf8(buffer.as_slice()).is_err() {
                return Err(FrameError::InvalidUtf8 { len: buffer.len() });
            }
            match serde_json::from_slice::<T>(buffer.as_slice()) {
                Ok(value) => {
                    buffer.clear();
                    Ok(Some(Incoming::Frame(value)))
                }
                // The raw line stays in `buffer` for the caller.
                Err(err) => Err(FrameError::Json(err)),
            }
        }
    }
}

/// Read one line and return its bytes without checking UTF-8 or decoding JSON.
/// The line terminator and one `\r` are removed. Blank lines are skipped under
/// the same rules as [`read_json_line_opts`]. A line over `limit` is
/// [`FrameError::TooLarge`].
pub fn read_line_bytes<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
) -> Result<Option<Vec<u8>>, FrameError> {
    read_line_bytes_observed(reader, buffer, limit, opts, &mut |_| {})
}

/// [`read_line_bytes`] that also reports every received line to `observe`, under the same
/// rules as [`read_json_line_or_oversized_observed`]: one call per line, before any skip or
/// rejection, with its [`ReceivedLine`]. Nothing is parsed, so a line that is not valid
/// JSON, or not valid UTF-8, is returned as its bytes.
///
/// Returns:
/// - `Ok(Some(bytes))` for a frame: the line without its terminator, and without the
///   `\r` of a `\r\n` terminator. `buffer` is left empty.
/// - `Ok(None)` on clean EOF. `buffer` is empty.
/// - `Err(FrameError::TooLarge { limit })` for a line over the limit, once it is consumed
///   through its newline. `buffer` is empty.
/// - `Err(FrameError::EmptyLine)` for an empty line when `skip_empty_lines` is off. The
///   line is consumed. `buffer` is empty.
/// - `Err(FrameError::Io)` or `Err(FrameError::TimedOut)`. `buffer` is empty; the stream
///   is no longer on a line boundary that the caller can rely on.
///
/// The bytes compared to `limit` are chosen by [`ReadOptions::limit_counts`]; see
/// [`LimitCounts`].
pub fn read_line_bytes_observed<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
    observe: &mut dyn FnMut(ReceivedLine),
) -> Result<Option<Vec<u8>>, FrameError> {
    match next_line(reader, buffer, limit, opts, observe)? {
        None => Ok(None),
        Some(Line::Oversized(_)) => Err(FrameError::TooLarge { limit }),
        Some(Line::Frame) => Ok(Some(std::mem::take(buffer))),
    }
}

/// Splits a `BufReader` into its inner stream and the bytes it had read past
/// the last returned frame. Use it when a stream changes protocol after a
/// JSON-line handshake, such as a ready frame, so that buffered bytes are not
/// lost. [`BufReader::into_inner`] alone discards them.
pub fn into_inner_with_remainder<R: Read>(reader: BufReader<R>) -> (R, Vec<u8>) {
    let remainder = reader.buffer().to_vec();
    (reader.into_inner(), remainder)
}

/// One line delimited by the reader. On `Frame` the line content is left in
/// the caller's buffer, without its terminator or a trailing `\r`.
enum Line {
    Frame,
    Oversized(Oversized),
}

/// Bytes that count as blank under [`ReadOptions::skip_whitespace_only_lines`].
fn is_blank(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | 0x0B | 0x0C)
}

/// Whether `line` (without its terminator) is blank under `whitespace`.
fn is_blank_line(line: &[u8], whitespace: WhitespaceSet) -> bool {
    let ascii = || line.iter().all(|b| is_blank(*b));
    match whitespace {
        WhitespaceSet::Ascii => ascii(),
        WhitespaceSet::Unicode => match std::str::from_utf8(line) {
            Ok(text) => text.trim().is_empty(),
            Err(_) => ascii(),
        },
    }
}

/// Reads one line into `buffer`. On any error, `buffer` is cleared before the error is
/// returned, so an I/O, deadline, discard, or empty-line failure never leaves partial
/// bytes behind. Callers that keep a line on a parse error do so after this returns.
fn next_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
    observe: &mut dyn FnMut(ReceivedLine),
) -> Result<Option<Line>, FrameError> {
    match next_line_inner(reader, buffer, limit, opts, observe) {
        Ok(line) => Ok(line),
        Err(err) => {
            buffer.clear();
            Err(err)
        }
    }
}

fn next_line_inner<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
    opts: ReadOptions,
    observe: &mut dyn FnMut(ReceivedLine),
) -> Result<Option<Line>, FrameError> {
    let limits = opts.limits;
    // One CR may precede the newline, so `limit + 1` content bytes is the most
    // a valid frame can occupy before its terminator.
    let cap = limit.saturating_add(1);
    'line: loop {
        buffer.clear();
        let mut overflow: Option<Oversized> = None;
        let mut discarded = 0usize;
        // Every byte consumed for this line, terminator included.
        let mut raw = 0usize;
        let mut terminator = Terminator::None;
        // The last payload byte seen for this line, to tell CRLF from LF at the newline.
        let mut last_content: Option<u8> = None;
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
                    observe(ReceivedLine {
                        raw_len: raw,
                        terminator,
                    });
                    return Ok(Some(Line::Oversized(over)));
                }
                if buffer.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let newline = chunk.iter().position(|b| *b == b'\n');
            let take = newline.map_or(chunk.len(), |i| i + 1);
            raw = raw.saturating_add(take);
            let content = &chunk[..newline.unwrap_or(take)];
            if let Some(byte) = content.last() {
                last_content = Some(*byte);
            }
            if newline.is_some() {
                terminator = if last_content == Some(b'\r') {
                    Terminator::CrLf
                } else {
                    Terminator::Lf
                };
            }
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
        // Observed before any skip or rejection decision, so keep-alive activity sees
        // blank lines too.
        observe(ReceivedLine {
            raw_len: raw,
            terminator,
        });
        if let Some(over) = overflow {
            buffer.clear();
            return Ok(Some(Line::Oversized(over)));
        }
        let mut end = buffer.len();
        if end > 0 && buffer[end - 1] == b'\r' {
            end -= 1;
        }
        if end == 0 {
            if opts.skip_empty_lines {
                continue 'line;
            }
            buffer.clear();
            return Err(FrameError::EmptyLine);
        }
        if opts.skip_whitespace_only_lines && is_blank_line(&buffer[..end], opts.whitespace) {
            continue 'line;
        }
        let counted = match opts.limit_counts {
            LimitCounts::Payload => end,
            LimitCounts::RawLineBytes => raw,
        };
        if counted > limit {
            let head = buffer[..end.min(OVERSIZE_HEAD_BYTES)].to_vec();
            buffer.clear();
            return Ok(Some(Line::Oversized(Oversized {
                limit,
                head,
                bytes: counted,
            })));
        }
        buffer.truncate(end);
        return Ok(Some(Line::Frame));
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

/// [`write_json_line`] followed by a flush. Use it when the peer waits for the
/// frame before it writes anything (a request on a pipe, for example).
pub fn write_json_line_flush<W, T>(
    writer: &mut W,
    value: &T,
    limit: usize,
) -> Result<(), FrameError>
where
    W: Write,
    T: Serialize,
{
    write_json_line(writer, value, limit)?;
    writer.flush()?;
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
