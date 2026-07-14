# rightkit-ocr

Internal, unpublished shared OCR runtime extracted from the byte-identical
CodeRight and ScrapeRight implementations characterized on 2026-07-14.

The crate owns preprocessing, tensor construction, detector/recognizer
sessions, DirectML-to-CPU fallback diagnostics, boxes, reading order,
de-duplication, CTC decoding, typed errors, safe canonical artifact paths, and
SHA-256 verification. Apps retain runtime/model locations, environment
compatibility, jobs, telemetry, downloads, and UI.

## Fail-closed provenance decision

The deployed detector, recognizer, and dictionary are byte-identical across
both apps, but existing labels conflict between `PP-OCRv5` and `PP-OCRv6`.
`OcrEngine::load` therefore returns `UnresolvedProvenance` before reading a
model or constructing ORT. `load_with_manifest` becomes usable only when the
signed artifact catalog supplies a non-empty authoritative label and source;
the three characterized digests are still verified before session creation.
No version label is guessed by this crate.

The committed PNG fixture is test data, not a model. No ONNX model or ONNX
Runtime binary is bundled.

## Publication blockers

- Authoritative model label/source and license record.
- Signed Right Release model and per-OS ORT artifact catalog.
- macOS bundled-dylib parity evidence.
- Adapter parity after both apps migrate.

Licensed under MIT OR Apache-2.0. Models and ONNX Runtime retain independent
licenses and attribution obligations.
