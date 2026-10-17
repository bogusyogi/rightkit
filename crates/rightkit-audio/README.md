# rightkit-audio

Shared audio for Right Suite apps. The crate provides typed FFmpeg filter graphs, loudness measurement and gates, room tone, ducking, WAV I/O, and optional microphone capture with resampling, downmixing and recovery. Presets are bundled from the `presets` directory.

Catalog capability: `audio`.

## What it owns

- Filter graphs (`filter`, `chain`, `ducking`, `room_tone`, `bus`, `preset`). Graph builders are pure: they never spawn processes.
- Measurement and offline renders (`measure`, `profile`). These run FFmpeg through `rightkit-ffmpeg`, so cancellation, timeout and process-tree ownership are uniform.
- WAV I/O (`wav`): `read_wav_mono_f32`, `read_wav_16k_mono`, `write_wav_mono_i16`.
- Live capture (`capture`, feature `capture`) and the Windows endpoint and power lifecycle (`lifecycle`).
- Audio output (`playback`, feature `playback`): `list_output_devices`, `OutputSession` (push, pause, resume, flush, volume, mute, clock, counters, device-loss recovery) and the `OutputControl` handle for other threads.
- One-shot playback (`playback::play_once`, feature `playback`): decode a WAV and play it to the default output device, blocking until it ends.
- Mic-to-camera lag (`mic_align`: `align_mic`) and equal-power looped room-tone fill (`room_fill`: `room_tone_fill`, `room_tone_bed`).

## When to use it

- Use it to build or run FFmpeg audio chains for narration, dialogue cleanup, ducking or room tone.
- Use it for WAV read and write in Right Suite apps.
- Use it for microphone capture when a product needs resampling and downmix in one place.
- Use it to play decoded audio through an output device (feature `playback`): `OutputSession` takes interleaved `f32` samples through a bounded lock-free ring and exposes a latency-adjusted playback clock for A/V sync.

Do not use it for transcription (see `rightkit-asr`), or to decode or remux media for playback (see `rightkit-playback`, which prepares files and does not output audio).

## Cargo features

| Feature | Default | Pulls in | Adds |
|---|---|---|---|
| `capture` | yes | `tracing` 0.1, `cpal` 0.18.2, `rubato` 5.0.0, `ringbuf` 0.5; macOS `objc2-core-audio`, `objc2-core-foundation`; Windows `windows`, `windows-core` | Live microphone capture (`capture` module) and the Windows endpoint and power lifecycle (`lifecycle`). Without it, the crate is filter graphs, measurement, WAV I/O and room tone only. |
| `playback` | no | `cpal` 0.18.2 (the same optional dependency `capture` uses; nothing new) | Audio output (`playback` module): `OutputSession` over a bounded lock-free SPSC ring, `played_samples()`/`position()` clock, pause/resume/flush, volume/mute, underrun/overrun counters, reopen of the default device after a device loss. Independent of `capture`. |

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
- Android: capture uses cpal's AAudio host (`cfg(target_os = "android")` module `capture::android` holds the permission name and Bluetooth SCO route helpers). The app must hold `RECORD_AUDIO` and have run `RightKit.init` first. Not yet built or run on a device (docs/design/android-minimum.md).
- FFmpeg is required for measurement and rendering. It is resolved through `rightkit-ffmpeg`.
- `tests/audio_e2e.rs` uses real files and FFmpeg, and a named loopback input device when one is available. No test routes audible audio to speakers; the one `playback` test that opens the default output device is `#[ignore]` and pushes only silence.
- Playback (feature `playback`) uses cpal output on WASAPI, CoreAudio and ALSA, as capture does. The pure ring, gain and clock tests run on every host; the device test runs with `--features playback -- --ignored`.

## Playback

For a short cue, `play_once` does the decode, resample and device handling in one call and returns when the device has consumed the file:

```rust
rightkit_audio::playback::play_once(std::path::Path::new("chime.wav"))?;
```

```rust
use rightkit_audio::playback::OutputSession;

let mut out = OutputSession::open(None)?;       // default device; or a name from list_output_devices()
let fmt = out.format();                         // actual sample_rate and channels: push f32 in this layout
let accepted = out.push_samples(&decoded);      // never blocks; fewer than decoded.len() means overrun
let clock = out.position();                     // latency-adjusted A/V master clock
out.pause(); out.resume();
out.flush();                                    // seek: drops queued samples, clock back to zero
out.set_volume(0.8)?; out.set_muted(false);
let stats = out.stats();                        // underruns, overruns, stream errors, recoveries
```

- Real-time safety: the output callback touches only atomics and the lock-free ring, in buffers allocated before the stream starts. Underrun plays silence and is counted. Overrun returns the accepted count and is counted.
- The clock counts samples actually played (silence substituted on underrun and time spent paused are not counted), minus the output latency taken from the host's output timestamps as of the latest callback.
- `push_samples` takes `&mut self` (single producer). `out.control()` returns a cloneable `OutputControl` for clock reads, pause, volume, flush and stats from other threads.
- Device loss: on a device-not-available or stream-invalidated error, the next `push_samples` (or an explicit `recover()`) reopens the default device, keeping queued samples, pause, volume and mute. The new device must offer the same rate and channel count, otherwise `recover()` errors and the caller opens a new session. Failures are available from `take_last_error()`.
- The session starts running, so `stats().underruns` counts callbacks until the first samples arrive.

## Presets

`presets/noisy-room.json`, `presets/podcast.json` and `presets/talking-head.json`.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Publication goes through the RightKit release workflow after platform gates pass. Changes are listed in [CHANGELOG.md](CHANGELOG.md).

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead.

- `tests/audio_e2e.rs`, `filter_graph_render_is_measurable_and_wav_round_trips` and `ducking_and_room_tone_render_have_content_changes`: a development FFmpeg, enabled by `RIGHTKIT_FFMPEG=<path>` or one of the candidate paths in `rightkit_ffmpeg::dev_resolve`.
- `tests/audio_e2e.rs`, `resampler_and_capture_gate_are_explicit` (the capture half): a loopback input device (BlackHole, Loopback or Soundflower), enabled by installing one. The resampler assertions run on every host.
