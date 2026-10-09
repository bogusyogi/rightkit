# Changelog

All notable changes to `rightkit-ocr`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.1.2

First crates.io release line. Ships the `native` feature (Apple Vision on macOS,
`Windows.Media.Ocr` on Windows, with per-line geometry), the `onnx` feature
(verified two-stage PP-OCR detector and recognizer through `rightkit-ort`, with
SHA-256 artifact checks), the isolated worker, and the `rightkit-ocr` binary.

Earlier history is in the git log of this repository; this file starts at the
publication point.
