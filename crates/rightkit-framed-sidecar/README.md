# rightkit-framed-sidecar

Bounded transport primitives for Right Suite sidecars: newline-delimited JSON frames with a size limit, strict decode errors, and resynchronisation on a clean line boundary. The crate is product-neutral and has no app dependencies. It is a transport layer only; product schemas and behaviour stay in the owning app.

Catalog capability: `framed-sidecar`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## What it owns

- `read_json_line`, `read_json_line_with` (with `ReadLimits`): read one frame and deserialize it. Leading blank lines, including `\r\n`-only lines, are skipped. An oversized frame is discarded as it streams in, never buffered beyond `limit + 1` bytes, and `FrameError::TooLarge` is returned. The next call resumes on a clean boundary. A peer that never sends a newline is cut off after a bounded discard.
- `write_json_line`: encodes first and measures the payload. If it exceeds the limit, nothing is written and `FrameError::TooLarge` is returned.
- `read_json_line_or_oversized`: returns an over-limit frame as a value (`Incoming::Oversized`, with its head and `Oversized::id`). A supervisor can answer that one call and keep the child running.
- `length_prefixed`: binary-safe alternative framing, a 4-byte big-endian length followed by a JSON body (`write_frame`, `read_frame`, and the `_bytes` variants).
- `wire`: one typed envelope as `wire::Frame`, with the variants `Hello`, `Ready`, `Request`, `Event`, `Response` and `Bye`.
- On malformed JSON, `FrameError::Json` is returned after the bad line is consumed, so the next call resumes cleanly.

## When to use it

- Use it for a child process or sidecar that speaks JSON lines over stdio and must not allocate without bound.

Do not use it for schemas or request routing. Those belong to the app or to `rightkit-jobs`.

## Cargo features

None. The crate has no `[features]` table. Dependencies: `serde` and `serde_json`.

## Usage

Taken from `tests/json_lines.rs`. `Frame` is a test struct with `id: u64` and `value: String`:

```rust
use rightkit_framed_sidecar::{read_json_line, write_json_line, FrameError};

let mut output = Vec::new();
write_json_line(&mut output, &Frame { id: 7, value: "ok".into() }, 128)?;
// `Frame` is any Serialize type. The output is the compact line {"id":7,"value":"ok"} plus a newline.
```

`read_json_line(&mut reader, &mut buffer, limit)` takes a `BufRead` reader, a scratch `Vec<u8>` buffer, and the frame limit in bytes. It returns `Ok(None)` at end of input. The target type is inferred from the caller.

## Platform support

Pure Rust over `std::io`. No platform-specific code.

## Tests

`tests/json_lines.rs` (line framing) and `tests/wire_and_prefixed.rs` (wire envelope and length-prefixed frames).

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
