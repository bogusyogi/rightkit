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
//! - With the `tokio` feature, [`tokio_io`] reads and writes the same JSON lines over
//!   `tokio::io::AsyncBufRead` and `AsyncWrite` with the same bounds, resynchronisation and
//!   errors (see [`read_json_line_async`]).

#[cfg(any(unix, windows))]
pub mod deadline;
pub mod length_prefixed;
#[cfg(feature = "tokio")]
pub mod tokio_io;
pub mod wire;

#[cfg(any(unix, windows))]
pub use deadline::{
    read_frame_deadline, read_frame_deadline_opts, write_frame_deadline, write_frame_deadline_opts,
    DeadlineFramed,
};
pub use length_prefixed::FrameOptions;
#[cfg(feature = "tokio")]
pub use tokio_io::{
    read_json_line_async, read_json_line_async_opts, read_json_line_or_oversized_async_opts,
    write_encoded_line_async, write_encoded_line_flush_async, write_json_line_async,
    write_json_line_flush_async,
};

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
    /// The encoded frame contains a newline before its terminator. A JSON line
    /// must hold exactly one newline, at its end. Compact `serde_json` output
    /// never contains one, so this is raised only by a value that embeds raw
    /// JSON with whitespace (`serde_json::value::RawValue`) or by pre-encoded
    /// bytes passed to [`write_encoded_line`].
    EmbeddedNewline,
    /// Pre-encoded bytes passed to [`write_encoded_line`] do not end in `\n`.
    MissingTerminator,
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
            FrameError::EmbeddedNewline => {
                write!(f, "json frame contains a newline before its terminator")
            }
            FrameError::MissingTerminator => {
                write!(f, "json frame does not end in a newline")
            }
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FrameError::TooLarge { .. }
            | FrameError::InvalidUtf8 { .. }
            | FrameError::EmptyLine
            | FrameError::TimedOut { .. }
            | FrameError::EmbeddedNewline
            | FrameError::MissingTerminator => None,
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

impl FrameError {
    /// The `serde_json` error inside a [`FrameError::Json`]; `None` for every other variant.
    /// The variant's shape is unchanged.
    pub fn json_error(&self) -> Option<&serde_json::Error> {
        match self {
            FrameError::Json(err) => Some(err),
            _ => None,
        }
    }

    /// The bare serde message for a [`FrameError::Json`], without the
    /// "json frame failed to deserialize:" prefix that `Display` adds, for example
    /// ``missing field `id` at line 1 column 9``. `None` for every other variant. It is an owned
    /// `String` because `serde_json` formats its message on demand rather than storing it.
    pub fn json_message(&self) -> Option<String> {
        self.json_error().map(|err| err.to_string())
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
        Some(Line::Frame) => decode_line(buffer).map(|value| Some(Incoming::Frame(value))),
    }
}

/// Decode the frame line left in `buffer`. On success `buffer` is cleared; on invalid UTF-8
/// or a JSON failure the raw line stays in `buffer` for the caller.
fn decode_line<T: DeserializeOwned>(buffer: &mut Vec<u8>) -> Result<T, FrameError> {
    if std::str::from_utf8(buffer.as_slice()).is_err() {
        return Err(FrameError::InvalidUtf8 { len: buffer.len() });
    }
    match serde_json::from_slice::<T>(buffer.as_slice()) {
        Ok(value) => {
            buffer.clear();
            Ok(value)
        }
        Err(err) => Err(FrameError::Json(err)),
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
        let mut scan = LineScan::new();
        loop {
            if limits.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(deadline_error());
            }
            let chunk = match reader.fill_buf() {
                Ok(c) => c,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(FrameError::Io(err)),
            };
            if chunk.is_empty() {
                match scan.at_eof(buffer, observe) {
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
        match scan.finish(buffer, limit, opts, observe)? {
            Finished::Skip => continue 'line,
            Finished::Done(line) => return Ok(Some(line)),
        }
    }
}

/// The error for a read that passed its [`ReadLimits::deadline`] between chunks.
fn deadline_error() -> FrameError {
    FrameError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        "json frame deadline exceeded",
    ))
}

/// What a line reader does at the end of the stream.
enum AtEof {
    /// Return this result now: clean end of stream, or an oversized line cut by the end.
    Done(Option<Line>),
    /// A final unterminated line is buffered; finish it as a frame.
    Finish,
}

/// What a finished line means to the reader loop.
enum Finished {
    /// A skipped blank line: read the next one.
    Skip,
    Done(Line),
}

/// The state of one line while its chunks arrive. The blocking readers and the tokio readers
/// share it, so both have the same resynchronisation, limit, discard and observer semantics.
struct LineScan {
    overflow: Option<Oversized>,
    discarded: usize,
    /// Every byte consumed for this line, terminator included.
    raw: usize,
    terminator: Terminator,
    /// The last payload byte seen for this line, to tell CRLF from LF at the newline.
    last_content: Option<u8>,
}

impl LineScan {
    fn new() -> Self {
        Self {
            overflow: None,
            discarded: 0,
            raw: 0,
            terminator: Terminator::None,
            last_content: None,
        }
    }

    /// The reader reported an empty chunk: the stream ended.
    fn at_eof(&mut self, buffer: &[u8], observe: &mut dyn FnMut(ReceivedLine)) -> AtEof {
        // EOF: a final unterminated line is still a frame.
        if let Some(over) = self.overflow.take() {
            observe(ReceivedLine {
                raw_len: self.raw,
                terminator: self.terminator,
            });
            return AtEof::Done(Some(Line::Oversized(over)));
        }
        if buffer.is_empty() {
            return AtEof::Done(None);
        }
        AtEof::Finish
    }

    /// Takes one buffered chunk into the line: stores its content, or drains it once the line
    /// is oversized. Returns how many bytes of `chunk` the reader must consume, and whether
    /// the line's newline was among them.
    fn absorb(
        &mut self,
        chunk: &[u8],
        buffer: &mut Vec<u8>,
        limit: usize,
        cap: usize,
    ) -> (usize, bool) {
        let newline = chunk.iter().position(|b| *b == b'\n');
        let take = newline.map_or(chunk.len(), |i| i + 1);
        self.raw = self.raw.saturating_add(take);
        let content = &chunk[..newline.unwrap_or(take)];
        if let Some(byte) = content.last() {
            self.last_content = Some(*byte);
        }
        if newline.is_some() {
            self.terminator = if self.last_content == Some(b'\r') {
                Terminator::CrLf
            } else {
                Terminator::Lf
            };
        }
        if let Some(over) = self.overflow.as_mut() {
            self.discarded = self.discarded.saturating_add(take);
            over.bytes = over.bytes.saturating_add(content.len());
        } else if buffer.len() + content.len() > cap {
            let seen = buffer.len() + content.len();
            let mut head = std::mem::take(buffer);
            head.truncate(OVERSIZE_HEAD_BYTES);
            let room = OVERSIZE_HEAD_BYTES - head.len();
            head.extend_from_slice(&content[..room.min(content.len())]);
            self.overflow = Some(Oversized {
                limit,
                head,
                bytes: seen,
            });
            self.discarded = take;
        } else {
            buffer.extend_from_slice(content);
        }
        (take, newline.is_some())
    }

    /// Called after a chunk without the newline: a peer that never sends one is cut off once
    /// the discard bound is passed.
    fn check_discard(&self, limit: usize, limits: &ReadLimits) -> Result<(), FrameError> {
        if self.overflow.is_some() && self.discarded > limits.max_discard {
            return Err(FrameError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "json frame exceeded {limit}-byte limit and no newline within {} bytes",
                    limits.max_discard
                ),
            )));
        }
        Ok(())
    }

    /// The line is complete (newline, or a final unterminated line): observe it, then decide
    /// whether it is a frame, an oversized line, a blank line to skip, or an empty-line error.
    fn finish(
        self,
        buffer: &mut Vec<u8>,
        limit: usize,
        opts: ReadOptions,
        observe: &mut dyn FnMut(ReceivedLine),
    ) -> Result<Finished, FrameError> {
        // Observed before any skip or rejection decision, so keep-alive activity sees
        // blank lines too.
        observe(ReceivedLine {
            raw_len: self.raw,
            terminator: self.terminator,
        });
        if let Some(over) = self.overflow {
            buffer.clear();
            return Ok(Finished::Done(Line::Oversized(over)));
        }
        let mut end = buffer.len();
        if end > 0 && buffer[end - 1] == b'\r' {
            end -= 1;
        }
        if end == 0 {
            if opts.skip_empty_lines {
                return Ok(Finished::Skip);
            }
            buffer.clear();
            return Err(FrameError::EmptyLine);
        }
        if opts.skip_whitespace_only_lines && is_blank_line(&buffer[..end], opts.whitespace) {
            return Ok(Finished::Skip);
        }
        let counted = match opts.limit_counts {
            LimitCounts::Payload => end,
            LimitCounts::RawLineBytes => self.raw,
        };
        if counted > limit {
            let head = buffer[..end.min(OVERSIZE_HEAD_BYTES)].to_vec();
            buffer.clear();
            return Ok(Finished::Done(Line::Oversized(Oversized {
                limit,
                head,
                bytes: counted,
            })));
        }
        buffer.truncate(end);
        Ok(Finished::Done(Line::Frame))
    }
}

/// Serialize `value` as a compact single-line JSON frame and return the bytes
/// that go on the wire: the encoded payload followed by one `\n`.
///
/// This is the one encoding every writer in this crate uses. If the payload
/// (excluding the trailing newline) exceeds `limit` bytes,
/// [`FrameError::TooLarge`] is returned. If the payload contains a newline,
/// [`FrameError::EmbeddedNewline`] is returned. Either way nothing is produced,
/// so a supervisor that queues encoded frames learns of the failure at enqueue
/// time. Send the result with [`write_encoded_line`] or
/// [`write_encoded_line_flush`].
pub fn encode_json_line<T>(value: &T, limit: usize) -> Result<Vec<u8>, FrameError>
where
    T: Serialize + ?Sized,
{
    let mut encoded = serde_json::to_vec(value)?;
    if encoded.len() > limit {
        return Err(FrameError::TooLarge { limit });
    }
    if encoded.contains(&b'\n') {
        return Err(FrameError::EmbeddedNewline);
    }
    encoded.push(b'\n');
    Ok(encoded)
}

/// Write bytes produced by [`encode_json_line`] to `writer`, without flushing.
///
/// `bytes` must be exactly one frame: a payload with no newline followed by a
/// single trailing `\n`. Otherwise nothing is written and the error is
/// [`FrameError::MissingTerminator`] (no trailing `\n`) or
/// [`FrameError::EmbeddedNewline`] (a newline before the end, including a
/// doubled terminator). The payload (excluding the newline) must not exceed
/// `limit`, else [`FrameError::TooLarge`] and nothing is written.
pub fn write_encoded_line<W>(writer: &mut W, bytes: &[u8], limit: usize) -> Result<(), FrameError>
where
    W: Write,
{
    check_encoded_line(bytes, limit)?;
    writer.write_all(bytes)?;
    Ok(())
}

/// [`write_encoded_line`] followed by a flush, as [`write_json_line_flush`] is
/// to [`write_json_line`].
pub fn write_encoded_line_flush<W>(
    writer: &mut W,
    bytes: &[u8],
    limit: usize,
) -> Result<(), FrameError>
where
    W: Write,
{
    write_encoded_line(writer, bytes, limit)?;
    writer.flush()?;
    Ok(())
}

/// Validate pre-encoded frame bytes and return the payload without its newline.
fn check_encoded_line(bytes: &[u8], limit: usize) -> Result<&[u8], FrameError> {
    let Some((&last, payload)) = bytes.split_last() else {
        return Err(FrameError::MissingTerminator);
    };
    if last != b'\n' {
        return Err(FrameError::MissingTerminator);
    }
    if payload.contains(&b'\n') {
        return Err(FrameError::EmbeddedNewline);
    }
    if payload.len() > limit {
        return Err(FrameError::TooLarge { limit });
    }
    Ok(payload)
}

/// Serialize `value` as a compact single-line JSON frame terminated by `\n`
/// and write it to `writer`. If the encoded payload (excluding the trailing
/// newline) exceeds `limit` bytes, no bytes are written and
/// [`FrameError::TooLarge`] is returned. Encoding is [`encode_json_line`].
pub fn write_json_line<W, T>(writer: &mut W, value: &T, limit: usize) -> Result<(), FrameError>
where
    W: Write,
    T: Serialize,
{
    let encoded = encode_json_line(value, limit)?;
    writer.write_all(&encoded)?;
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
