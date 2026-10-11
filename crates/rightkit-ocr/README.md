# rightkit-ocr

Product-neutral OCR for Right Suite apps. It owns preprocessing, tensor construction, detector and recognizer sessions, DirectML-to-CPU fallback diagnostics, boxes, reading order, de-duplication, CTC decoding, typed errors, safe canonical artifact paths, and SHA-256 verification of model files. It also has native backends that return per-line geometry: Apple Vision on macOS and `Windows.Media.Ocr` on Windows.

**Use it** to read text from an image in a Right Suite app, through the native backend or the verified two-stage ONNX pipeline (`OcrEngine`, `ocr_image_with_manifest`).

**Do not use it** to download models, pick model locations, manage ONNX Runtime installs, run jobs, or send telemetry. Apps keep those. Use `rightkit-ort` for ONNX Runtime library resolution and sessions.

The public API is in `src/lib.rs`: `OcrEngine::load` and `OcrEngine::load_with_manifest` create an engine, `run` and `run_lines` read text, and `OcrArtifactManifest` carries the resolved provenance.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `native` | on | macOS `objc2`, `objc2-foundation`, `objc2-core-graphics`, `objc2-vision`, `objc2-core-foundation`; Windows `windows` | Apple Vision (macOS) or Windows.Media.Ocr (Windows). Without it, the native backend reports unavailable. |
| `onnx` | on | `rightkit-ort` (which pins `ort` with `load-dynamic`; DirectML on Windows, CoreML on Apple), `sha2` | Verified two-stage ONNX fallback (`OcrEngine`, `OcrArtifactManifest`). Without it, `Onnx` and fallback requests return `BackendUnavailable`. |

`--no-default-features` keeps the types, the worker protocol and the `rightkit-ocr` binary, with no OCR backend. Platform backends are still selected by `cfg` (see Platform support).

The crate also ships a `rightkit-ocr` binary (`src/bin/rightkit-ocr.rs`).

## Platform support

- macOS: Apple Vision native backend (`src/native/vision.rs`, `objc2-vision` and related crates), plus the ONNX path.
- Windows: `Windows.Media.Ocr` native backend (`src/native/win_native.rs`), plus the ONNX path with DirectML and CPU fallback.
- Other targets: the ONNX path is the only code path; the crate has no native backend there. This README does not claim support for other targets.

Release notes: see `CHANGELOG.md` in this crate.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/native_e2e.rs` and `tests/real_model_e2e.rs`.

```rust,ignore
use rightkit_ocr::OcrEngine;
use std::path::Path;

// `manifest` is an OcrArtifactManifest carrying the resolved provenance label
// and source from the signed artifact catalog (see Fail-closed provenance below).
let mut engine = OcrEngine::load_with_manifest(Path::new("/app/models/ocr"), &manifest)?;
let text = engine.run(&image)?; // image: &image::RgbImage
```

## Fail-closed provenance decision

The models are **PP-OCRv6** (official PaddlePaddle: `PP-OCRv6_medium_det` +
`PP-OCRv6_small_rec`, 18,708-entry unified multilingual dict, Apache-2.0).
ScrapeRight is the canonical set; CodeRight mirrors its exact digests. See
`PROVENANCE.toml`.

This was recorded as a `PP-OCRv5`/`PP-OCRv6` label conflict until 2026-07-14.
It was not a mislabel: the deployed artifacts were **not** byte-identical
across the two apps. CodeRight fetched PP-OCRv5 from a pinned script;
ScrapeRight had no fetch script, was hand-placed, and had been upgraded to
PP-OCRv6 on 2026-07-05. Both labels were true, of different files. The parity
check hashed shared Rust source only and passed green the whole time, so
nothing surfaced it. CodeRight is now migrated onto v6, and
`characterization/phase5-adoption` hashes the deployed models in both apps and
fails closed on divergence.

`OcrArtifactManifest::pp_ocr_v6()` carries the resolved provenance (PP-OCRv6,
the two PaddlePaddle Hugging Face repos above, Apache-2.0), and `OcrEngine::load`
uses it. The label is tied to bytes: the Hugging Face LFS SHA-256 of
`PP-OCRv6_medium_det_onnx/inference.onnx` and
`PP-OCRv6_small_rec_onnx/inference.onnx` equals the det and rec digests in `PROVENANCE.toml` (`[model.detector]`, `[model.recognizer]`)
(checked 2026-10-09), and both model cards declare Apache-2.0.
`characterized_windows_contract()` keeps the old conflict for callers that want
the fail-closed manifest.

Apps that run a model-store GC pass a `ModelLeaseHook` through
`OnnxConfig::with_lease_hook` (`src/router.rs:110`); it is called before verification and its guard is
held until the engine drops. `Ocr::is_available`,
`Ocr::recognize_with_provenance` and `Ocr::validate_fallback` are the other entry points on `Ocr` (`src/router.rs`).

The committed PNGs and base64 fixture (`tests/fixtures/`, and the canary `src/native/assets/vision_canary.png`) are test data, not models. No ONNX model or ONNX
Runtime binary is bundled.

## Isolated worker: events and typed kinds

Apps that run native recognition in a child process (`WorkerCommand`) can observe
the child's lifecycle and key on typed failures instead of message text:

- `WorkerCommand::run_observed(bytes, &events)` reports `WorkerEvent`s
  (`Spawned { pid }`, `Waiting`, `Responded`, `Exited`, `TimedOut`) as they happen,
  and returns a `RunRecord` with the same facts. A plain closure works as `events`.
- `WorkerFailure::kind` and `OcrError::kind()` return an `OcrErrorKind`. Match on the
  kind. The message text still carries the legacy prefix (for example
  `macos-vision-unavailable:`), but the prefix constants are deprecated for keying.
- `WorkerCommand::with_canary_deadline` bounds the canary run separately from the
  request deadline. `Ocr::with_health_hook` takes a host-owned `NativeHealthHook`
  cache of the availability verdict. The crate keeps no cache.

## Publication status

Recorded blockers as of 2026-10-06. Check `INDEX.md` for the current state.

- Signed Right Release model and per-OS ORT artifact catalog: open.
- Adapter parity after both apps migrate: open.
- macOS bundled-dylib parity evidence: passed 2026-10-06 (`tests/real_model_e2e.rs`).

Licensed under MIT OR Apache-2.0. Models and ONNX Runtime retain independent
licenses and attribution obligations.

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead.

- `tests/native_e2e.rs` and `tests/worker_events.rs` (`health_hook_records_a_fresh_canary_verdict`), and the native-backend tests in `tests/native_e2e.rs`: a native OS text recognizer that reads the embedded canary (Apple Vision on macOS, `Windows.Media.Ocr` on Windows). On a hosted runner the OS text-recognition service can fail even on the canary (`tests/common/mod.rs:58-59`), so these tests skip when it does. The check is the isolated canary, not the platform name.
- `tests/native_e2e.rs` (the ONNX tests and the agreement check) and `tests/real_model_e2e.rs`: the PP-OCRv6 ONNX models and ONNX Runtime, enabled by `RIGHTKIT_OCR_MODEL_DIR=<dir with det.onnx, rec.onnx, en_dict.txt>` and `RIGHTKIT_OCR_ORT_RUNTIME=<path to the runtime library>`. Without the variables the suites look for `scraperight/models/ocr`, and for `bin/libonnxruntime.dylib` (macOS) or `bin/onnxruntime.dll` (Windows), by walking up from the crate (`tests/native_e2e.rs:49-64`).
