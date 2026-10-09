# Changelog

All notable changes to `rightkit-transcript`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.1.0

First crates.io release line. Ships the word-timed `Transcript` contract, cue
segmentation (`build_cues`, `CaptionConfig`), alignment checks, and SRT, WebVTT,
timing JSON, and plain-text import and export. The engine-backed round trip
(`asr-roundtrip` feature) is test-only and off by default.

Earlier history is in the git log of this repository; this file starts at the
publication point.
