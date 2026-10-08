# rightkit-vad

Product-neutral voice activity detection for Right Suite apps. It owns the Silero scorer (`Silero`, loaded from a caller-supplied ONNX model path), the frame protocol (512-sample frames at 16 kHz, 32 ms each), the span policy (`scores_to_spans`, `detect`), chunking (`chunk_spans`), and streaming frame verdicts (`FrameStream`). It owns no dictation, push-to-talk, or command policy. Callers decide what a span means.

**Use it** to find speech spans in 16 kHz mono audio, or to get per-frame speech decisions in a live stream.

**Do not use it** as a transcriber, a speaker detector, or a model downloader. Supply the Silero model yourself. The crate does not bundle or select the model.

## Layers

Each layer is usable alone:

- `FrameScorer`: any per-frame speech-probability source. `Silero` is one; tests can use a double.
- `scores_to_spans`: Silero `get_speech_timestamps`-shaped hysteresis (`threshold` to start, `neg_threshold` to end, `min_speech`, `min_silence`, `pad`, merge).
- `detect`: runs a `FrameScorer` over samples and returns spans.
- `chunk_spans`: splits spans under `ChunkConfig` limits.
- `FrameStream`: pushes samples and returns `FrameVerdicts` as frames complete.
- `energy_scores`: a simple energy-based scorer, with no model.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `ort-defaults` | on (through `default`) | `ort` with `std`, `ndarray`, `tracing`, `download-binaries`, `tls-native`, `copy-dylibs`, `api-24` | The default ONNX Runtime setup. `download-binaries` fetches prebuilt ONNX Runtime during the build. |
| `load-dynamic` | off | `ort/load-dynamic` | Loads ONNX Runtime at run time from a library path. Use it with `default-features = false` to avoid the build-time download and to bind the approved runtime through `rightkit-ort`. |

The `ort` dependency is pinned to `=2.0.0-rc.13` in `Cargo.toml`. The comment in `Cargo.toml` says the defaults keep rc.12's set. Check that comment before relying on it.

## Platform support

The crate has no `cfg(target_os)` gates in `src/`. ONNX Runtime support follows the `ort` features above. The Silero model file and the ONNX Runtime library are supplied by the app.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`. The crate's manifest sets `publish = false`.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/e2e_silero.rs`.

```rust,ignore
use rightkit_vad::{detect, Silero, SileroOptions, VadConfig};

let mut scorer = Silero::load_with("/app/models/silero_vad.onnx", SileroOptions::default())?;
let spans = detect(&mut scorer, &samples_16k_mono, &VadConfig::default())?;
```
