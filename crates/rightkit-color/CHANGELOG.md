# Changelog

## 0.1.2

- Publish the dependency requirements main already declares: `rightkit-fs` 0.5.0, where the published 0.1.1 requires 0.4.9. The 0.1.1 manifest on crates.io stays as published; this version carries the new requirements.

## 0.1.1 (unpublished)

- Calls `rightkit_ffmpeg::verify_ffmpeg_handle` / `verify_ffprobe_handle` (renamed from `verify_*_or_development`); requires `rightkit-ffmpeg` 0.2.16.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cutright-color` (CutRight).
