# Changelog


## 0.2.2

- Republished on the renamed dependencies: `rightkit-onnx` (was `rightkit-ort`). The 0.2.1 on crates.io was packaged before the renames and still named the old crates.
All notable changes to `rightkit-ocr`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.2.1

Additive. No existing signature, variant, message or behavior changes; the
`WorkerCommand` struct gains one public field (see below).

- Worker lifecycle events: `WorkerCommand::run_observed(bytes, &events)` reports
  `WorkerEvent` values (`Spawned { pid }`, `Waiting`, `Responded { bytes, elapsed }`,
  `Exited { code, success }`, `TimedOut`) to any `WorkerEvents` implementation,
  including a plain closure. It returns `WorkerRun { lines, record }` on success and
  `WorkerFailure { kind, message, record }` on failure, where `record` is a
  `RunRecord` with the same facts. `WorkerCommand::run` is unchanged and delegates
  to it.
- Typed error kinds: `OcrErrorKind` (`#[non_exhaustive]`) with `OcrError::kind()`,
  `OcrErrorKind::from_message`, `is_backend_unavailable`, and `as_str`. Kinds include
  `MacosVisionUnavailable`, `WindowsOcrUnavailable`, `UnsupportedPlatform`,
  `NoFallbackConfigured`, `WorkerTimeout`, `WorkerOutputExceeded`, `WorkerProtocol`,
  `WorkerFailed`, `RequestTooLarge`, `Provenance`, `Artifact`, and `Runtime`. The
  `BlankPage` kind is reserved: no path returns it yet, because a verified blank page
  is `Ok` with no lines.
- Deprecated for keying: the `...-unavailable:` and `ocr-unsupported-platform:` prefix
  constants. They remain in every error's `Display` text for compatibility. Match
  `OcrErrorKind` instead.
- Canary deadline: `WorkerCommand` gains a public `canary_deadline` field
  (default `WORKER_DEADLINE`, so behavior is unchanged), `with_canary_deadline`, and
  `for_canary()`. The router runs canaries under that deadline instead of the request's
  deadline. Struct-literal construction of `WorkerCommand` must now set the field; use
  `WorkerCommand::new`.
- Healthy-verdict hook: `NativeHealthHook` (`cached_verdict`, `record_verdict`) with
  `Ocr::with_health_hook`. The hosts own the cache; `probe_native` consults it first and
  reports only definite verdicts. The crate keeps no cache.
- Known limit: the child's own watchdog stays at `WORKER_DEADLINE` (90 s), so a
  parent deadline above 90 s is still cut short by the child.

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
