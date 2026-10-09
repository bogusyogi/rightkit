# Changelog

All notable changes to `rightkit-tts`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.1.0

First crates.io release. Features:

- `system`: OS voice backend (`say` and `afconvert` on macOS, SAPI on Windows)
  rendering to an isolated WAV with a word-timed manifest.
- `client`: QTS worker client (`QtsClient`), sentence chunking, and model
  manifest verification.
- `engine`, `metal`, `vulkan`: the Qwen3-TTS worker (`rightkit-tts-worker`
  binary) and its accelerator features.

Earlier history is in the git log of this repository; this file starts at the
publication point.
