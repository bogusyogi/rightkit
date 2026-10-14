# rightkit-sidecar

Bounded transport primitives for Right Suite sidecars: newline-delimited JSON frames with a size limit, strict decode errors, and resynchronisation on a clean line boundary. Length-prefixed frames can also be bounded by an absolute deadline. The crate is product-neutral and has no app dependencies. It is a transport layer only; product schemas and behaviour stay in the owning app.

Catalog capability: `framed-sidecar`. crates.io has 0.1.0; the source is 0.2.0, so the `deadline` functions below are not yet published (see [INDEX.md](../../INDEX.md)).

## What it owns

- `read_json_line`, `read_json_line_with` (with `ReadLimits`): read one frame and deserialize it. Leading blank lines, including `\r\n`-only lines, are skipped. An oversized frame is discarded as it streams in, never buffered beyond `limit + 1` bytes, and `FrameError::TooLarge` is returned. The next call resumes on a clean boundary. A peer that never sends a newline is cut off after a bounded discard.
- `write_json_line`: encodes first and measures the payload. If it exceeds the limit, nothing is written and `FrameError::TooLarge` is returned.
- `encode_json_line`: the same encoding as a `Vec<u8>` (the wire bytes, newline included), refused at encode time if over the limit or if it contains a newline before its terminator. `write_encoded_line` and `write_encoded_line_flush` write those bytes with the same checks; they refuse input that does not end in exactly one newline.
- `read_json_line_or_oversized`: returns an over-limit frame as a value (`Incoming::Oversized`, with its head and `Oversized::id`). A supervisor can answer that one call and keep the child running.
- `length_prefixed`: binary-safe alternative framing, a 4-byte big-endian length followed by a JSON body (`write_frame`, `read_frame`, and the `_bytes` variants).
- `wire`: one typed envelope as `wire::Frame`, with the variants `Hello`, `Ready`, `Request`, `Event`, `Response` and `Bye`.
- `deadline` (new in 0.2.0): `read_frame_deadline` and `write_frame_deadline` move one length-prefixed frame, the same wire format as `length_prefixed`, before an absolute `Instant`. Partial transfers resume until the deadline. On expiry they return `FrameError::TimedOut { partial_bytes }`. `DeadlineFramed<T>` wraps a stream that owns its descriptor or handle.
- On malformed JSON, `FrameError::Json` is returned after the bad line is consumed, so the next call resumes cleanly. Non-UTF-8 lines return `FrameError::InvalidUtf8`.
- `read_json_line_opts` and `read_json_line_or_oversized_opts` take `ReadOptions`, including `skip_whitespace_only_lines`. `read_line_bytes` returns raw line bytes. `write_json_line_flush` flushes after writing. `into_inner_with_remainder` returns the stream and bytes buffered past the last frame. `UNCAPPED` is an explicit no-bound limit for trusted large frames.

## When to use it

- Use it for a child process or sidecar that speaks JSON lines over stdio and must not allocate without bound.

Do not use it for schemas or request routing. Those belong to the app or to `rightkit-jobs`.

## Cargo features

None. The crate has no `[features]` table. Dependencies: `serde` and `serde_json`, plus `libc` on Unix and `windows-sys` on Windows.

## Usage

Taken from `tests/json_lines.rs`. `Frame` is a test struct with `id: u64` and `value: String`:

```rust
use rightkit_sidecar::{read_json_line, write_json_line, FrameError};

let mut output = Vec::new();
write_json_line(&mut output, &Frame { id: 7, value: "ok".into() }, 128)?;
// `Frame` is any Serialize type. The output is the compact line {"id":7,"value":"ok"} plus a newline.
```

`read_json_line(&mut reader, &mut buffer, limit)` takes a `BufRead` reader, a scratch `Vec<u8>` buffer, and the frame limit in bytes. It returns `Ok(None)` at end of input. The target type is inferred from the caller.

## Platform support

The blocking JSON-lines and `length_prefixed` APIs are pure Rust over `std::io`. The `deadline` functions are platform-specific:

- **Unix**: `poll(2)` over a raw descriptor. The descriptor must be nonblocking (`O_NONBLOCK`); the functions check it and do not change it. `DeadlineFramed::new` sets it.
- **Windows**: overlapped I/O with `WaitForSingleObject` and `CancelIoEx` over a raw handle. The handle must be opened with `FILE_FLAG_OVERLAPPED`; the crate cannot verify this.

## Tests

`tests/json_lines.rs` (line framing), `tests/encoded_lines.rs` (`encode_json_line` and the encoded writers: limit, embedded newline, terminator, flush) and `tests/wire_and_prefixed.rs` (wire envelope and length-prefixed frames). `tests/deadline.rs` covers the deadline functions over socketpairs and pipes on Unix, and `tests/deadline_windows.rs` covers them over a named pipe on Windows.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Changes are recorded in [CHANGELOG.md](CHANGELOG.md).
