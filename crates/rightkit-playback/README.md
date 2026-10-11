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

## Host-authorised sources (opt-in)

By default no path becomes a source unless Rust registers it. A host that already authorises whole folders (for example Tauri's asset scope) can hand the registry that decision instead of registering files one by one:

```rust
let state = PlaybackState::new(Arc::new(MyHost));
// Either the host's own rule (receives the canonical path) ...
let app = handle.clone();
state.set_authoriser(move |path| app.asset_protocol_scope().is_allowed(path));
// ... or a ready helper: regular files under these directories, recursively.
state.allow_roots([project_dir]);
// register `authorised_media_source_id` next to `media_source_id` in the invoke handler
```

`authorised_media_source_id(path)` (Tauri command and `PlaybackState` method; `SourceRegistry::resolve_or_register` underneath) returns the id for any file the hook allows, registering it on demand. `media_source_id` keeps its contract and never registers.

- The path is canonicalised before the hook sees it and before it is stored. Symlinks that leave an allowed root, `..` escapes, missing paths, directories and non-regular files are refused. A path that does not resolve is `PLAYBACK_NOT_ALLOWED`, not `PLAYBACK_MISSING`, so the call does not reveal whether a file outside the allowed area exists. Without a hook every call is `PLAYBACK_NOT_ALLOWED`.
- The hook runs again every time a hook-registered id is used, and the path must still canonicalise to the registered one. A hook that starts returning `false`, `clear_authoriser`, or a file swapped for a symlink out of the root refuses the id with `PLAYBACK_NOT_ALLOWED`. Explicitly registered ids ignore the hook.
- The hook runs under the registry lock; it must not call back into the registry or state.
- Residual window: the check precedes the open by FFmpeg, which reopens the path. A swap in that final instant is not detectable by path checks alone; a changed size or modification time still fails closed.
- Windows: canonical paths carry the verbatim `\\?\` prefix. `allowed_roots` canonicalises both sides, so it is consistent; a custom hook that compares strings must expect that form.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/remux_e2e.rs` and `tests/cancel_race.rs`.

```rust,ignore
// Resolve an FFmpeg binary with rightkit-media, then call compatibility_copy
// with the resolved path, the source, a cache root, and a cancellation token.
```

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-playback`.
