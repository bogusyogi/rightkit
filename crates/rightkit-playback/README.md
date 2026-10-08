# rightkit-playback

Source-preserving FFmpeg compatibility playback preparation for Right Suite media apps. It remuxes a source file to a playback-friendly derivative (video stream copy, AAC audio, `+faststart`) behind a content-fingerprinted cache, with cooperative cancellation, bounded concurrency, and explicit coded errors. Extracted from ScrapeRight's `playback.rs` and decoupled from its queue.

**Use it** when an app must give a user a playable copy of a media file that the source's codecs or container make unplayable in the host webview, without re-encoding video.

**Do not use it** to transcode to arbitrary formats, to find or verify FFmpeg (the app supplies a `PlaybackSource` and resolves FFmpeg itself, typically with `rightkit-media`), or to manage the process tree beyond what `rightkit-process` provides.

## Surface

- `compatibility_copy(ffmpeg, source, cache_root, cancel)`: returns the derivative path.
- `PlaybackRegistry`: request-id claims, early cancellation, and release on `Drop`.
- `compatibility_args`, `MediaKind`, `PlaybackSource`: pure helpers.
- `FfmpegCandidates`: explicit override, bundled, or development path resolution (no `PATH` lookup).
- `feature = "tauri"`: `PlaybackHost` trait, `PlaybackState`, and the `prepare_media_playback` and `cancel_media_playback` command handlers.

`PLAYBACK_*` error codes are stable for the frontend. `@rightkit/media-ui` maps `PLAYBACK_UNSUPPORTED` to its codec error state.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `tauri` | off | `tauri` | Tauri command handlers and the `PlaybackHost` trait. |

## Platform support

The crate has no `cfg(target_os)` gates in `src/`. Process handling is delegated to `rightkit-process`, which has platform-specific code. Playback behavior depends on the FFmpeg build the app supplies, and that build's codec set is defined by `rightkit-media`'s catalog.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/remux_e2e.rs` and `tests/cancel_race.rs`.

```rust,ignore
// Resolve an FFmpeg binary with rightkit-media, then call compatibility_copy
// with the resolved path, the source, a cache root, and a cancellation token.
```

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-playback`.
