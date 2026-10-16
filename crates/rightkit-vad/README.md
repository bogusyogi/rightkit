# rightkit-vad

Product-neutral voice activity detection for Right Suite apps. It owns the Silero scorer (`Silero`, loaded from a caller-supplied ONNX file or from embedded bytes), the frame protocol (512-sample frames at 16 kHz, 32 ms each), the span policy (`scores_to_spans`, `detect`), chunking (`chunk_spans`), and streaming frame verdicts (`FrameStream`). It owns no dictation, push-to-talk, or command policy. Callers decide what a span means.

**Use it** to find speech spans in 16 kHz mono audio, or to get per-frame speech decisions in a live stream.

**Do not use it** as a transcriber, a speaker detector, or a model downloader. Supply the Silero model yourself. The crate does not bundle or select the model.

## Layers

Each layer is usable alone:

- `FrameScorer`: any per-frame speech-probability source. `Silero` is one; tests can use a double.
- `SpeechScorer` (feature `silero`): a scorer built from a path or bytes that reports its `Backend` (`SileroCpu`, `SileroCoreMl`, `SileroDirectMl`, `Energy`). With `energy_fallback`, a failed Silero load yields the energy scorer and the reason. The energy scorer is batch-only, so it reaches `scores` and `detect` but not `FrameStream`.
- `scores_to_spans`: Silero `get_speech_timestamps`-shaped hysteresis (`threshold` to start, `neg_threshold` to end, `min_speech`, `min_silence`, `pad`, merge).
- `detect`: runs a `FrameScorer` over samples and returns spans.
- `chunk_spans`: splits spans under `ChunkConfig` limits.
- `FrameStream`: pushes samples and returns `FrameVerdicts` as frames complete. When the scorer fails, `push` returns `None` (unknown, not silence) and the stream stays failed until `reset`.
- `energy_scores`: energy and zero-crossing pseudo-scores, with no model.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `default` | on | `silero` | The default set. |
| `silero` | on | `rightkit-onnx`, `rightkit-onnx/session` | The `Silero` scorer, `SileroOptions`, `SpeechScorer`, and the `Backend` report. The crate fetches, copies and links no runtime. The consumer supplies ONNX Runtime through `rightkit-onnx`. With `default-features = false` no ONNX Runtime is compiled and only the policy layers remain. |
| `ort-tracing` | off | `silero`, `rightkit-onnx/tracing` | Opt-in: forwards ONNX Runtime's log lines into `tracing`. Off by default, because feature unification would turn it on for every ONNX Runtime session in the process. |
| `download-binaries` | off | `silero`, `rightkit-onnx/download-binaries` | Opt-in: fetches prebuilt ONNX Runtime during the build. |
| `tls-native` | off | `silero`, `rightkit-onnx/tls-native` | Opt-in: native TLS backend for `download-binaries`. |
| `copy-dylibs` | off | `silero`, `rightkit-onnx/copy-dylibs` | Opt-in: copies the ONNX Runtime dylibs next to the build output. |

Unreleased change: the default no longer enables `ort/ndarray` or the build-time download (`download-binaries`, `tls-native`, `copy-dylibs`). The `ort-defaults` feature is removed. To keep the old build-time download, opt in with `features = ["download-binaries", "tls-native", "copy-dylibs"]` (with `default-features` left on), or use `default-features = false` and bind the approved runtime through `rightkit-onnx`. The empty `load-dynamic` feature is removed.

This crate has no direct `ort` dependency (`Cargo.toml:27-28`); the `ort` pin (`=2.0.0-rc.14`, `std`, API level 24) lives in `rightkit-onnx` (`crates/rightkit-onnx/Cargo.toml:59`). rc.14's own `default` raises the API floor to ORT 1.31, which the suite's 1.24.4 runtime cannot serve. Do not use `ort/default`.

## Platform support

The only OS gates in `src/` are in `src/silero.rs`: the CoreML provider needs `target_vendor = "apple"` (`:38`) and the DirectML provider needs `target_os = "windows"` (`:39`); on other targets those providers return an error (`ensure_supported`, `:201-210`; the register fallbacks, `:277-281` and `:295-299`). ONNX Runtime support follows the `rightkit-onnx` features above. The Silero model file and the ONNX Runtime library are supplied by the app.

Android (planned; nothing compiled or run): no Apple or Windows code is reached. Only `ExecutionProvider::Cpu` is hosted (`ExecutionProvider::supported_on_this_os()`); the app binds `libonnxruntime.so` through `rightkit-onnx` (`android_runtime_candidate`).

Release notes: see `CHANGELOG.md` in this crate. The crate is published to crates.io.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/e2e_silero.rs`.

```rust,ignore
use rightkit_vad::{detect, ExecutionProvider, SpeechScorer, SileroOptions, VadConfig};

let options = SileroOptions {
    execution_provider: ExecutionProvider::CoreMl, // Apple only; Cpu is the default
    energy_fallback: true,                         // a failed load scores with energy and says why
    ..SileroOptions::default()
};
let (mut scorer, report) = SpeechScorer::from_bytes(SILERO_ONNX, options)?;
if let Some(reason) = &report.fallback_reason {
    eprintln!("VAD fell back to {}: {reason}", report.backend.name());
}
let spans = scorer.detect(&samples_16k_mono, &VadConfig::default())?;
```

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead.

- `tests/e2e_silero.rs` (all tests that load a model): the Silero model `silero_vad_16k_op15.onnx`, enabled by `RIGHTKIT_VAD_MODEL=<path>`.
- `tests/e2e_silero.rs` (all tests that load a model, and `corrupt_model_bytes_fall_back_to_energy_with_reason`) on macOS: the ONNX Runtime 1.24.4 dylib, enabled by `RIGHTKIT_ORT_TEST_DYLIB=<path>` or a `genright/.cache/...` download.
