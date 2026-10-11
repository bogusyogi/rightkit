# rightkit-audio

Shared audio for Right Suite apps. The crate provides typed FFmpeg filter graphs, loudness measurement and gates, room tone, ducking, WAV I/O, and optional microphone capture with resampling, downmixing and recovery. Presets are bundled from the `presets` directory.

Catalog capability: `audio`.

## What it owns

- Filter graphs (`filter`, `chain`, `ducking`, `room_tone`, `bus`, `preset`). Graph builders are pure: they never spawn processes.
- Measurement and offline renders (`measure`, `profile`). These run FFmpeg through `rightkit-media`, so cancellation, timeout and process-tree ownership are uniform.
- WAV I/O (`wav`): `read_wav_mono_f32`, `read_wav_16k_mono`, `write_wav_mono_i16`.
- Live capture (`capture`, feature `capture`) and the Windows endpoint and power lifecycle (`lifecycle`).

## When to use it

- Use it to build or run FFmpeg audio chains for narration, dialogue cleanup, ducking or room tone.
- Use it for WAV read and write in Right Suite apps.
- Use it for microphone capture when a product needs resampling and downmix in one place.

Do not use it for playback or for transcription (see `rightkit-asr`).

## Cargo features

| Feature | Default | Pulls in | Adds |
|---|---|---|---|
| `capture` | yes | `tracing` 0.1, `cpal` 0.18.2, `rubato` 5.0.0, `ringbuf` 0.5; macOS `objc2-core-audio`, `objc2-core-foundation`; Windows `windows`, `windows-core` | Live microphone capture (`capture` module) and the Windows endpoint and power lifecycle (`lifecycle`). Without it, the crate is filter graphs, measurement, WAV I/O and room tone only. |

## Usage

Illustrative. Function names and signatures match `src/wav.rs` and the imports in `tests/audio_e2e.rs`:

```rust
use std::path::Path;
use rightkit_audio::wav::{read_wav_16k_mono, write_wav_mono_i16};

let samples: Vec<f32> = read_wav_16k_mono(Path::new("/abs/in.wav"))?;
// ... process samples ...
// write_wav_mono_i16(...) writes mono 16-bit output; see src/wav.rs for its arguments.
```

## Platform support

- Capture is the default `capture` feature: the cpal capture module has no OS gate (`src/lib.rs:16-17`). Two parts are OS-specific: the CoreAudio transport probe is `cfg(target_os = "macos")` (`src/capture/device.rs:65`), and the endpoint and power lifecycle is `cfg(target_os = "windows")` (`src/lifecycle.rs:142-153`); the lifecycle registration returns `None` on every other OS.
- FFmpeg is required for measurement and rendering. It is resolved through `rightkit-media`.
- `tests/audio_e2e.rs` uses real files and FFmpeg, and a named loopback input device when one is available. No test routes audio to speakers.

## Presets

`presets/noisy-room.json`, `presets/podcast.json` and `presets/talking-head.json`.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Publication goes through the RightKit release workflow after platform gates pass. Changes are listed in [CHANGELOG.md](CHANGELOG.md).

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead.

- `tests/audio_e2e.rs`, `filter_graph_render_is_measurable_and_wav_round_trips` and `ducking_and_room_tone_render_have_content_changes`: a development FFmpeg, enabled by `RIGHTKIT_FFMPEG=<path>` or one of the candidate paths in `rightkit_media::dev_resolve`.
- `tests/audio_e2e.rs`, `resampler_and_capture_gate_are_explicit` (the capture half): a loopback input device (BlackHole, Loopback or Soundflower), enabled by installing one. The resampler assertions run on every host.
