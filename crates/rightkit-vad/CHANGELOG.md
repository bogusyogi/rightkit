# Changelog

All notable changes to `rightkit-vad`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.1.1

- Republished on the renamed dependencies: `rightkit-ffmpeg` (was `rightkit-media`, dev), `rightkit-onnx` (was `rightkit-ort`). The 0.1.0 on crates.io was packaged before the renames and still named the old crates.
- The `silero` feature no longer enables `rightkit-ort/tracing`. Forwarding ONNX Runtime's log lines into `tracing` is now the opt-in `ort-tracing` feature: through feature unification it applied to every ONNX Runtime session in the process and produced about 4,400 INFO lines per launch in HeardRight.
- `SileroOptions::intra_op_spinning` and `inter_op_spinning` are `Spinning` (`Off` default, as earlier builds forced; `On`; `RuntimeDefault` makes no call, leaving the ONNX Runtime setting untouched). `own_thread_pool: Option<bool>`: `Some(false)` makes no thread-pool call. HeardRight runs macOS and Linux with all three left to the runtime. Full struct literals of `SileroOptions` need the new fields.
- Calibration `rms` now accumulates in f64. A one-second single-precision sum drifted by about 1e-5, so a calibrated threshold can change in its fifth decimal compared with earlier builds; this is a behaviour change for anyone comparing thresholds across versions.
- `ExecutionProvider::CoreMl` is documented as ONNX Runtime's CoreML provider running the ONNX model.
  It is not a native `.mlmodelc` or `.mlpackage` stage, which stays in the application.
- `SpanOptions::cap_silence_at_calibrated: bool`, default `true` (the cap as before). `false` passes the
  silence threshold through. A silence threshold above the calibrated speech threshold then returns
  `Error::Input` from `resolve`, rather than being capped. Struct-literal construction needs
  `..SpanOptions::default()`.
- Behaviour change for generic `FrameScorer` implementations. A non-finite frame score is `Error::Model` on
  the span path (`detect`), and on `FrameStream::push` it is a scorer failure: `None`, stream failed until
  `reset`. Before, a NaN score counted as silence and an infinite one as speech, with no error.

## 0.1.0

- Breaking for struct-literal construction: `SileroOptions` gains `execution_provider`,
  `device_id`, and `energy_fallback`. Migrate with `..SileroOptions::default()`. Its defaults
  (one thread each, CPU, no fallback) are unchanged.
- `Silero::from_bytes(&[u8], SileroOptions)` loads an embedded model next to `Silero::load` and
  `Silero::load_with`. Apps that embed the model need no resource path.
- `ExecutionProvider::{Cpu, CoreMl, DirectMl}` selects the session's provider. CoreML runs the ONNX
  Runtime CoreML EP with `MLComputeUnits = CPUOnly`, the setting CutRight uses for its per-frame VAD.
  DirectML uses `device_id`. A provider this OS cannot host returns an error, never a silent CPU run.
- `Silero::backend()` and the `Backend` enum (`SileroCpu`, `SileroCoreMl`, `SileroDirectMl`,
  `Energy`) name the scorer that produced the scores.
- `SpeechScorer::from_path` and `SpeechScorer::from_bytes` return `(SpeechScorer, ScorerBackend)`.
  With `energy_fallback: true`, a failed Silero load yields the energy scorer and the reason in
  `ScorerBackend::fallback_reason`. The default is off, so a failed load is an error.
- `SpeechScorer::scores` and `SpeechScorer::detect` work for both backends. The energy scorer is
  batch-only (its noise floor spans the whole signal), so it is not a `FrameScorer` and does not
  feed `FrameStream`.
- Silero sessions run on their own thread pool with inter- and intra-op spinning off, the
  configuration CutRight's VAD session uses. The `intra_threads` and `inter_threads` options now take
  effect even when the process has a global ONNX Runtime pool.
- `SileroOptions::warmup: Warmup` (`ResetAfter` default, or `KeepState`). Loading always runs one
  zero frame. `ResetAfter` clears the state it leaves, so the first real frame sees a fresh model.
  `KeepState` keeps that state, as HeardRight does; its first frames then score differently and
  converge within the first seconds of audio.
- `SpanOptions { speech_threshold: 0.5, silence_threshold: 0.35, rms_calibration }` and
  `SpanOptions::resolve(base, ambient)` return a `ResolvedSpans` with the `VadConfig` to detect with.
  `RmsCalibration` (defaults: 8x RMS, 0.005 noise floor, 0.20 minimum, HeardRight's values) sets the
  speech threshold from ambient audio, clamped to 1.0, and caps the silence threshold at it.
  Calibration without non-empty, finite ambient samples is an error. HeardRight treats an empty
  calibration as a no-op; this crate refuses it.
- Unchanged: the frame protocol, `scores_to_spans`, `chunk_spans`, `FrameStream`, and `detect`
  (`detect` now shares one frame-scoring helper, with the same reset, padding and finiteness checks).

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
