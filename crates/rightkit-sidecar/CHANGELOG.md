# Changelog

## 0.2.1

- Added (rightkit#190, additive): `length_prefixed::read_frame_bytes_deadline` and `write_frame_bytes_deadline` move one length-prefixed frame over any `Read` or `Write` before an absolute `Instant`, waiting through a readiness callback (`Ready`: given the time left, returns whether the stream is ready). For transports that are not a plain descriptor or overlapped handle (the descriptor-based `read_frame_deadline` and `write_frame_deadline` from 0.2.0 are unchanged). Same wire format and error mapping as `deadline`: a deadline is `FrameError::TimedOut { partial_bytes }` (0 means still on a frame boundary), a zero length or empty body is `Io(InvalidData)`, a length above the limit is `TooLarge` (a write sends nothing; a read leaves the body unread, close the stream), and an end of stream, including a clean one between frames, is `Io(UnexpectedEof)` (message `peer closed before a frame header` for the clean case), never `Ok(None)`. `Interrupted` and `WouldBlock` retry until the deadline. Tests: `tests/deadline_generic.rs`. Not compiled when written.

## 0.2.0 — 2026-10-11

Renamed from rightkit-framed-sidecar; 0.1.0 remains published under the old name.

- Additive: `FrameError::json_error(&self) -> Option<&serde_json::Error>` and
  `FrameError::json_message(&self) -> Option<String>` return the serde error and its bare message
  (no "json frame failed to deserialize:" prefix) for `FrameError::Json`, and `None` for other variants.
  `json_message` returns an owned `String` because `serde_json` formats its message on demand. The
  variant's shape is unchanged. Tests are in `src/lib.rs`. Not compiled when written.

Added (0.2.0, unpublished; additive options, defaults unchanged):
- `WhitespaceSet` (`Ascii` default, `Unicode`) and `ReadOptions::whitespace`. `Unicode` skips lines that `str::trim()` reduces to nothing: U+00A0 and U+2003 are blank; U+FEFF is not (it is not White_Space). A line that is not valid UTF-8 keeps the ASCII rule. Construct `ReadOptions` with `..ReadOptions::default()`.
- `read_json_line_or_oversized_observed`: the `_or_oversized_opts` reader plus an `observe: &mut dyn FnMut(ReceivedLine)` argument. It is called once for every received line, before any skip or rejection decision, with a `ReceivedLine { raw_len, terminator }`. `raw_len` is the raw byte count, terminator included. `terminator` is `Terminator::Lf`, `Terminator::CrLf`, or `Terminator::None`. `None` means the stream ended before the line's newline, so the line is unterminated (MAX-1 bytes plus LF is `Lf`; MAX bytes at EOF is `None`). The reader consumes an oversized line through its newline, so it reports `Lf` or `CrLf`; `None` never means the limit cut the line. Blank lines that are skipped, oversized lines, and lines that then fail to parse are all observed. Not called on clean EOF or on a deadline failure mid-line. `ReceivedLine` and `Terminator` are `#[non_exhaustive]`. A callback is an argument rather than a `ReadOptions` field so that `ReadOptions` stays `Copy` and `Default`.
- `LimitCounts` (`Payload` default, `RawLineBytes`) and `ReadOptions::limit_counts`. `Payload` compares the bytes before the terminator, not counting a `\r` before `\n`; this is the behaviour before this option. `RawLineBytes` compares every byte as read, the `\n` or `\r\n` terminator included, so a line is accepted iff `raw <= limit`. That is the rule of a `read_line` over `take(limit)`, which HeardRight uses for `MAX_LINE`. With `Payload`, an LF line of raw `limit + 1` bytes is accepted and a CRLF line of raw `limit + 1` or `limit + 2` bytes is accepted; `RawLineBytes` refuses them.
- `ReadOptions::skip_empty_lines` (default `true`, the behaviour before this option). With `false`, an empty line (zero bytes, or one `\r`) returns the new `FrameError::EmptyLine` and is consumed. Empty and whitespace-only are separate cases: `skip_whitespace_only_lines` never skips an empty line under `skip_empty_lines = false`, and `skip_empty_lines` never skips a whitespace-only line. A whitespace-only line is skipped only by `skip_whitespace_only_lines` (under `WhitespaceSet`).
- `FrameError::EmptyLine`.
- `encode_json_line<T: Serialize>(value, limit) -> Result<Vec<u8>, FrameError>`: the one encoding every writer uses. It returns the wire bytes (compact JSON plus one `\n`) and enforces the limit and the no-interior-newline rule at encode time, so a queueing supervisor sees the error at enqueue.
- `write_encoded_line` and `write_encoded_line_flush`: write bytes from `encode_json_line` with the same limit check. They refuse bytes that do not end in exactly one newline or that contain an interior newline, writing nothing.
- `FrameError::EmbeddedNewline` and `FrameError::MissingTerminator`. Exhaustive matches on `FrameError` need two new arms. `write_json_line` now refuses a value whose compact encoding contains a newline (only reachable through embedded raw JSON such as `RawValue` with whitespace); previously it wrote such a value.

- `deadline` module, re-exported at the crate root: `read_frame_deadline` and `write_frame_deadline`. They move one 4-byte big-endian length-prefixed frame, the same wire format as `length_prefixed`, before an absolute `Instant`. Partial reads and writes resume until the deadline. A read is bounded by `max_len`; a write by `max_len` and by the body being non-empty.
- Unix: the transfers use `poll(2)` over a raw descriptor, and the descriptor must be nonblocking. The functions check `O_NONBLOCK` and fail with `InvalidInput` on a blocking descriptor; they never change it.
- Windows: the transfers use overlapped `ReadFile`/`WriteFile` over a raw handle, waited with `WaitForSingleObject` and cancelled with `CancelIoEx` at the deadline. The handle must be opened with `FILE_FLAG_OVERLAPPED`. A cancellation the kernel does not confirm within one second is reported as `Io` of kind `TimedOut`, and the handle must then be discarded.
- `DeadlineFramed<T>`, a wrapper over a stream that owns its descriptor or handle, with `new`, `get_ref`, `get_mut`, `into_inner`, `read_frame` and `write_frame`. `new` sets `O_NONBLOCK` on Unix.
- `ReadOptions` with `skip_whitespace_only_lines`, and the `_opts` readers `read_json_line_opts` and `read_json_line_or_oversized_opts`. With the option set, a line of only ASCII blank bytes (space, tab, `\r`, vertical tab, form feed) is skipped like an empty line. Non-ASCII whitespace is still a frame. The existing readers keep their behaviour.
- `FrameError::InvalidUtf8 { len }`. A JSON line that is not valid UTF-8 now reports this instead of `Json`. The line is consumed, so the next call resumes.
- `read_line_bytes`: a raw-bytes read that returns the line without UTF-8 or JSON checks, under the same blank-line and limit rules.
- `write_json_line_flush`: `write_json_line` followed by a flush.
- `UNCAPPED` (`usize::MAX`): a limit that accepts any frame length, for trusted peers with large frames (for example engine stdin/stdout). Memory grows with the frame; other callers should pass a bound.
- `into_inner_with_remainder(BufReader<R>) -> (R, Vec<u8>)`: returns the inner stream and the bytes buffered past the last returned frame, so a protocol change after a handshake loses nothing.
- Documented and tested: `ReadLimits::max_discard = 0` stops at the first chunk that crosses the cap. That chunk is consumed, and nothing further is read.
- `FrameError::TimedOut { partial_bytes }`. `partial_bytes` counts the bytes of the current frame, length prefix included, that moved before the deadline. Zero leaves the stream on a frame boundary; a non-zero count leaves it mid-frame, so it must be closed. The stream-state table is in the `deadline` module documentation.

Compatibility: `FrameError` gained `EmptyLine`, and `ReadOptions` gained fields; exhaustive matches on `FrameError` need a new arm, and struct literals of `ReadOptions` need `..ReadOptions::default()`. `ReadOptions` no longer derives `Default`; it implements it by hand so that `skip_empty_lines` defaults to `true`.

Changed:
- `Cargo.toml`: `libc` is a Unix dependency and `windows-sys` a Windows dependency, both target-specific. The `deadline` module exists only on Unix and Windows; other targets build the blocking API as before.
- `README.md` documents the deadline API and the platform requirements. The crate now ships a `CHANGELOG.md`.

Unchanged:
- The blocking API is unchanged: `read_json_line`, `read_json_line_with`, `read_json_line_or_oversized`, `write_json_line`, `length_prefixed`, and `wire`. Their tests are unchanged.

Compatibility: `FrameError` gained a variant. Exhaustive matches on `FrameError` need a new arm. The version is 0.2.0 rather than 0.1.1 for that reason, as cargo treats 0.x minor bumps as breaking.
