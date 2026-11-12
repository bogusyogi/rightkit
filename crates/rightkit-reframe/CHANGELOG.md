# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-models` 0.5 -> 0.7, `rightkit-onnx` 0.2 -> 0.3. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## 0.1.1 (unpublished)

- Calls `rightkit_ffmpeg::verify_ffmpeg_handle` / `verify_ffprobe_handle` (renamed from `verify_*_or_development`); requires `rightkit-ffmpeg` 0.2.16.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cutright-reframe` (CutRight).
