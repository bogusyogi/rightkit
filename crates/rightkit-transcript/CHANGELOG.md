# Changelog

All notable changes to `rightkit-transcript`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.2.0

- `TimedWord` is now the single word-timing owner for the suite: added
  `TimedWord::from_seconds`, `start_secs`, `end_secs` and
  `From<(text, start_ms, end_ms)>`. `words_from_seconds` is unchanged.
- Added `group_words` / `GroupRules` (the lightweight single-line cue grouper
  `rightkit-asr` used to hand-roll) and `cues_from_spans`, so engines export
  through `to_srt` / `to_vtt` / `to_timing_json` instead of private copies.
- Removed the test-only `asr-roundtrip` feature and its optional
  `rightkit-asr` dependency (it would cycle now that `rightkit-asr` depends on
  this crate). The engine-backed round trip moved to `rightkit-asr`
  (`tests/transcript_roundtrip.rs`).

## 0.1.0

First crates.io release line. Ships the word-timed `Transcript` contract, cue
segmentation (`build_cues`, `CaptionConfig`), alignment checks, and SRT, WebVTT,
timing JSON, and plain-text import and export. The engine-backed round trip
(`asr-roundtrip` feature) is test-only and off by default.

Earlier history is in the git log of this repository; this file starts at the
publication point.
