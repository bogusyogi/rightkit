# rightkit-ocr

Internal, unpublished shared OCR runtime extracted from the byte-identical
CodeRight and ScrapeRight implementations characterized on 2026-07-14.

The crate owns preprocessing, tensor construction, detector/recognizer
sessions, DirectML-to-CPU fallback diagnostics, boxes, reading order,
de-duplication, CTC decoding, typed errors, safe canonical artifact paths, and
SHA-256 verification. Apps retain runtime/model locations, environment
compatibility, jobs, telemetry, downloads, and UI.

## Fail-closed provenance decision

The models are **PP-OCRv5** (`monkt/paddleocr-onnx`: `detection/v5` + English
recognizer, 436-entry dict, Apache-2.0). CodeRight is the canonical set;
ScrapeRight mirrors its exact digests. See `PROVENANCE.toml`.

This was recorded as a `PP-OCRv5`/`PP-OCRv6` label conflict until 2026-07-14.
It was not a mislabel: the deployed artifacts were **not** byte-identical
across the two apps. CodeRight fetched v5 from a pinned script; ScrapeRight
had no fetch script and had been hand-placed with genuine PP-OCRv6. Both
labels were true, of different files. The parity check hashed shared Rust
source only and passed green the whole time, so nothing surfaced it.
`characterization/phase5-adoption` now hashes the deployed models in both
apps and fails closed on divergence.

`OcrEngine::load` still returns `UnresolvedProvenance` before reading a model
or constructing ORT. `load_with_manifest` becomes usable only when the signed
artifact catalog supplies a non-empty authoritative label and source; the
three characterized digests are still verified before session creation. No
version label is guessed by this crate — the label above comes from the
record, not from inference.

The committed PNG fixture is test data, not a model. No ONNX model or ONNX
Runtime binary is bundled.

## Publication blockers

- ~~Authoritative model label/source and license record.~~ Resolved 2026-07-14
  (PP-OCRv5, `monkt/paddleocr-onnx`, Apache-2.0 — see `PROVENANCE.toml`).
- Signed Right Release model and per-OS ORT artifact catalog.
- macOS bundled-dylib parity evidence.
- Adapter parity after both apps migrate.

Licensed under MIT OR Apache-2.0. Models and ONNX Runtime retain independent
licenses and attribution obligations.
