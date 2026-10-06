# rightkit-playback

Public candidate: compatibility-playback primitives for Right Suite desktop
apps. Source-preserving FFmpeg remux (video stream copy, AAC audio,
`+faststart`) behind a content-fingerprinted cache, with cooperative
cancellation, bounded concurrency, and explicit coded errors.

Provenance: extracted from ScrapeRight `src-tauri/src/playback.rs` and
decoupled from its queue. The app supplies a `PlaybackSource` and resolves
FFmpeg itself (typically via `rightkit-media` verified artifacts or
`FfmpegCandidates`); this crate owns arg construction, process lifecycle via
`rightkit-process`, cache, and cancellation.

## Surface

- `compatibility_copy(ffmpeg, source, cache_root, cancel)` → derivative path.
- `PlaybackRegistry` — request-id claims, early cancellation, `Drop` release.
- `compatibility_args` / `MediaKind` / `PlaybackSource` — pure helpers.
- `FfmpegCandidates` — explicit override/bundled/dev path resolution (no PATH).
- `feature = "tauri"` — `PlaybackHost` trait + `PlaybackState` +
  `prepare_media_playback` / `cancel_media_playback` command handlers.

`PLAYBACK_*` error codes are stable for the frontend (`@rightkit/media-ui`
turns `PLAYBACK_UNSUPPORTED` into its explicit codec error state).

## Publication gate

Publication remains gated on the current Windows platform run. Publish through
`right-release publish cargo` after the media and playback gates.
