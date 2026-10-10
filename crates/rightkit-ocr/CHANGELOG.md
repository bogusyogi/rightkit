# Changelog

All notable changes to `rightkit-ocr`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.2.0

- Resolve the ONNX model provenance: `OcrArtifactManifest::pp_ocr_v6()` records
  PP-OCRv6, the PaddlePaddle det/rec repos and Apache-2.0, tied to the upstream
  LFS digests. `OcrEngine::load` now uses it instead of failing closed.
- Breaking: `OnnxConfig` gains a `lease_hook` field. New `ModelLeaseHook` trait,
  `OnnxConfig::with_lease_hook`, `OcrEngine::load_with_lease`.
- Additive: `Ocr::is_available`, `Ocr::recognize_with_provenance` (`OcrOutcome`),
  `Ocr::validate_fallback`, `OcrEngine::validate_sessions`.

## 0.1.2

First crates.io release line. Ships the `native` feature (Apple Vision on macOS,
`Windows.Media.Ocr` on Windows, with per-line geometry), the `onnx` feature
(verified two-stage PP-OCR detector and recognizer through `rightkit-ort`, with
SHA-256 artifact checks), the isolated worker, and the `rightkit-ocr` binary.

Earlier history is in the git log of this repository; this file starts at the
publication point.
