# Changelog

## 0.3.1 (2026-10-17)

- Orthic-Labs/rightkit#454: new `mic_align` (`align_mic`, `AlignParams`, `MicAlignment`): mic-to-camera lag by mean-removed normalized cross-correlation, coarse 4x-decimated full-range pass then a full-rate fine pass, with confidence and peak margin. New `room_fill` (`room_tone_fill`, `room_tone_bed`, `FillParams`, `ToneFill`, `rms`, `encode_mono_f32_wav`): looped room-tone fill with equal-power crossfaded seams, scaled to the measured RMS plus `gain_db`. Lifted from CutRight `cutright-audio`, with its tests. Not compiled or run.
- Lag failures (short, silent or low-correlation input) return `AudioError::Invalid` with a `no mic alignment` message; `AudioError` gains no variant.

- #413 (audio part): `playback::play_once(path)` blocks until a WAV has played through the default output device. It downmixes to mono, resamples linearly, duplicates across device channels, and fails with `Playback` past the file duration plus five seconds. Not run on a device.
- #248, #326 and #449 source side is already on main (`playback::OutputSession`, `wav::read_wav_16k_mono` single open with capacity hint). The 0.3.x publish in #449 is not part of this entry and needs a version bump.

## 0.3.1

- Orthic-Labs/rightkit#326 (NOT COMPILED OR RUN): `read_wav_16k_mono` opens the file once (it used to open it twice, once for the header and once in `read_wav_mono_f32`) and sizes the sample buffer from the header's data length. `read_wav_mono_f32` shares the same decode and gets the same capacity hint. Output and errors are unchanged.

- Android capture (Orthic-Labs/rightkit#156): `capture` builds for `*-linux-android` on cpal 0.18's AAudio host with no Windows or Apple dependency pulled in (those are target-gated). New `capture::android` (`cfg(target_os = "android")`): `RECORD_AUDIO_PERMISSION`, `is_sco_route` and `sco_route_changed` for Bluetooth SCO route recovery. `CaptureErrorKind::DeviceChanged` is terminal on Android (a disconnected AAudio stream must be reopened). `capture_open_error` adds a `RECORD_AUDIO` hint on Android. Not built or run on a device.

## 0.3.0

- New feature `playback` (off by default, uses the existing optional `cpal` 0.18.2, no new dependency): `playback::{list_output_devices, OutputSession, OutputControl, OutputDeviceInfo, OutputFormat, OutputStats}`. `OutputSession::open(device)` reports the actual sample rate and channels; interleaved `f32` samples go through a bounded lock-free SPSC ring written in the crate (capacity set at open, `open_with_capacity`). Underrun plays silence; overrun returns the accepted count and never blocks. Latency-adjusted `played_samples()` and `position()` for use as an A/V master clock; `pause`, `resume`, `flush` (drops queued samples and resets the clock), `set_volume` and `set_muted` reach the callback through atomics with no allocation or locking. Underrun and overrun counters, errors as `AudioError`, reopen of the default device after a device loss, and a `Drop` that releases the stream. Requested by Orthic-Labs/rightkit#248 for Ember.
- Added `AudioError::Playback(String)`. This is a new variant on a public non-`#[non_exhaustive]` enum, so a downstream exhaustive `match` on `AudioError` needs a new arm; that is why this is a minor release.


## 0.2.3

- Republished on the renamed dependencies: `rightkit-ffmpeg` (was `rightkit-media`). The 0.2.2 on crates.io was packaged before the renames and still named the old crates.
## 0.2.2

- New `Filter::pos(value)`: a positional option rendered as the bare value (`aresample=44100`, `atrim=0:5`, `volume=0.22`), so an app can reproduce an existing FFmpeg filter string byte for byte. Named options are unchanged.
- `rightkit-media` is depended on with `default-features = false`.

## 0.2.1

- `tracing` is now an optional dependency enabled by the `capture` feature. Only `capture::session` and `lifecycle` log, and both are `capture`-gated, so `--no-default-features` (pure filter-graph consumers) no longer builds `tracing`. Default builds are unchanged.
