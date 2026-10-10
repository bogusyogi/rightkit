# Changelog

All notable changes to `rightkit-vad`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.0.3

(0.0.2 was already on crates.io; the entries below ship as 0.0.3.)

- New default-on `silero` feature gates the `Silero` scorer, `SileroOptions`, and the
  `rightkit-ort` dependency. `default-features = false` now removes ONNX Runtime from the
  build and keeps `scores_to_spans`, `chunk_spans`, `detect`, `FrameStream`, `FrameScorer`.
- Removed the empty `load-dynamic = []` feature; it enabled nothing. Bind a runtime through
  `rightkit-ort` directly. `download-binaries`, `tls-native`, and `copy-dylibs` now imply `silero`.

First crates.io release line. Ships the Silero voice-activity scorer, the
hysteresis, padding, and chunking policy, and streaming frame carry. ONNX Runtime
is supplied through `rightkit-ort`; the crate fetches and copies no runtime by
default (`download-binaries`, `tls-native`, and `copy-dylibs` are opt-in).

Earlier history is in the git log of this repository; this file starts at the
publication point.
