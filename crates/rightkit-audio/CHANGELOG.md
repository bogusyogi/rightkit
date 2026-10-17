# Changelog

## Unreleased

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
