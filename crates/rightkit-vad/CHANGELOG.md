# Changelog

All notable changes to `rightkit-vad`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.0.2

First crates.io release line. Ships the Silero voice-activity scorer, the
hysteresis, padding, and chunking policy, and streaming frame carry. ONNX Runtime
is supplied through `rightkit-ort`; the crate fetches and copies no runtime by
default (`download-binaries`, `tls-native`, and `copy-dylibs` are opt-in).

Earlier history is in the git log of this repository; this file starts at the
publication point.
