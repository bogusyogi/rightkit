# rightkit-playback

Source-preserving FFmpeg compatibility playback preparation for Right Suite media apps. It remuxes a source file to a playback-friendly derivative (video stream copy, AAC audio, `+faststart`) behind a content-fingerprinted cache, with cooperative cancellation, bounded concurrency, and explicit coded errors. Extracted from ScrapeRight's `playback.rs` and decoupled from its queue.

**Use it** when an app must give a user a playable copy of a media file that the source's codecs or container make unplayable in the host webview, without re-encoding video.

**Do not use it** to transcode to arbitrary formats, to find or verify FFmpeg (the app supplies a `PlaybackSource` and resolves FFmpeg itself, typically with `rightkit-media`), or to manage the process tree beyond what `rightkit-process` provides.

## Surface

- `compatibility_copy(ffmpeg, source, cache_root, cancel)`: returns the derivative path.
- `PlaybackRegistry`: request-id claims, early cancellation, and release on `Drop`.
- `SourceRegistry` / `SourceId`: fingerprint-backed, revocable media sources. The UI refers to a registered file by id and never supplies a raw path.
- `compatibility_args`, `MediaKind`, `PlaybackSource`: pure helpers.
- `FfmpegCandidates`: explicit override, bundled, or development path resolution (no `PATH` lookup).
- `feature = "tauri"`: `PlaybackHost` trait, `PlaybackState`, and the `prepare_media_playback`, `prepare_registered_media_playback`, `revoke_media_source` and `cancel_media_playback` command handlers.

`PLAYBACK_*` error codes are stable for the frontend. `@rightkit/media-ui` maps `PLAYBACK_UNSUPPORTED` to its codec error state.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `tauri` | off | `tauri` | Tauri command handlers and the `PlaybackHost` trait. |

## Platform support

The crate has no `cfg(target_os)` gates in `src/`. Process handling is delegated to `rightkit-process`, which has platform-specific code. Playback behavior depends on the FFmpeg build the app supplies, and that build's codec set is defined by `rightkit-media`'s catalog.

Published version: see `INDEX.md` at the repository root. Changes are in `CHANGELOG.md`.

## Registered sources and the Tauri command shape

Paths stay in Rust. Backend code registers a file and passes the UI a `SourceId`:

```rust
// Rust only: a listing command that already holds the path.
#[tauri::command]
fn list_sources(state: tauri::State<'_, PlaybackState>) -> Vec<SourceRow> {
    rows().into_iter().map(|row| SourceRow {
        source_id: state.register_source(&row.path).map(|id| id.to_string()).ok(),
        name: row.name,
    }).collect()
}

fn main() {
    tauri::Builder::default()
        .manage(PlaybackState::new(Arc::new(MyHost)))
        .invoke_handler(tauri::generate_handler![
            list_sources,
            rightkit_playback::tauri_commands::prepare_registered_media_playback,
            rightkit_playback::tauri_commands::revoke_media_source,
            rightkit_playback::tauri_commands::cancel_media_playback,
        ]);
}
```

Frontend calls (Tauri maps camelCase arguments to snake_case parameters):

```ts
// source: a SourceId from list_sources, or an already registered path. Returns the derivative path.
const derivative = await invoke<string>("prepare_registered_media_playback", { source: sourceId, requestId });
await invoke<boolean>("revoke_media_source", { source: sourceId });  // later prepares fail with PLAYBACK_REQUEST
```

`prepare_registered_media_playback` returns the derivative path that the host has already allowed as an asset. Unregistered paths and revoked or changed sources fail closed. The existing `prepare_media_playback(id, request_id)` keeps its media-id contract.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/remux_e2e.rs` and `tests/cancel_race.rs`.

```rust,ignore
// Resolve an FFmpeg binary with rightkit-media, then call compatibility_copy
// with the resolved path, the source, a cache root, and a cancellation token.
```

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-playback`.
