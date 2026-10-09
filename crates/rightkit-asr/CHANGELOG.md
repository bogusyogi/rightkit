# Changelog

All notable changes to `rightkit-asr`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.0.2

First crates.io release line. Ships the Parakeet ONNX decoder, the Whisper
decoder (whisper.cpp, and CoreML on macOS), and the decoding primitives, built
on `rightkit-ort` and `rightkit-vad`. The crate is a derivative of `parakeet-rs`
(MIT); see `PATCHES.md`, `UPSTREAM.toml`, and `THIRD_PARTY_NOTICES.md`. Model
artifacts are not packaged.

Earlier history is in the git log of this repository; this file starts at the
publication point.
