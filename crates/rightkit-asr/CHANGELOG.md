# Changelog

All notable changes to `rightkit-asr`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.1.0

- Breaking: `TimedWord` (`longform::TimedWord`, re-exported at the crate root) is now
  `rightkit_transcript::TimedWord`: millisecond `start_ms` / `end_ms` (`u64`) in place
  of `start` / `end` (`f32` seconds). Use `start_secs()` / `end_secs()` for seconds.
- `longform` SRT / VTT / timing-JSON rendering and cue grouping now call
  `rightkit-transcript` (`group_words`, `to_srt`, `to_vtt`, `to_timing_json`).
  `group_cues`, `render_subtitles`, `render_timing_json`, `timed_exports` keep their
  signatures. SRT/VTT output is byte-identical (golden test `longform_exports`);
  timing-JSON seconds are now the exact millisecond value (`1.52`, was `1.5199999809265137`).
  VTT text now escapes `&`, `<`, `>` and cue text rewrites `-->` to `->` (transcript export rules).
- Added a `rightkit-transcript` dependency. The engine-backed round trip that lived in
  `rightkit-transcript` moved here (`tests/transcript_roundtrip.rs`).

## 0.0.2

First crates.io release line. Ships the Parakeet ONNX decoder, the Whisper
decoder (whisper.cpp, and CoreML on macOS), and the decoding primitives, built
on `rightkit-ort` and `rightkit-vad`. The crate is a derivative of `parakeet-rs`
(MIT); see `PATCHES.md`, `UPSTREAM.toml`, and `THIRD_PARTY_NOTICES.md`. Model
artifacts are not packaged.

Earlier history is in the git log of this repository; this file starts at the
publication point.
