# Changelog

## 0.4.2

- New `echo` module (VO-5, `docs/contracts/voice.md` in the Pulse repository): `ReferenceTap` (a time-indexed 16 kHz history of what the speakers play, with a process-wide `reference_tap()`) and `EchoCanceller` (a model-free normalised-LMS filter, `process_with_tap`), so speech over a reply can be heard without the agent's own voice. Playback and capture must share the process, or the host feeds the tap from the process that plays. Not run on a microphone and a speaker.
- New `turn` module: `TurnTracker`, `TurnPolicy`, the `TurnModel` trait and `is_voiced`. With no model a turn ends after a fixed silence (700 ms); a host that has a model (smart-turn is the candidate; a download, not included) implements the trait. Additive.

## 0.4.1

- Reserve setup meter starts at command receipt, reject stale queued starts and stops, and drain an older admitted start before opening its replacement. Consent stays independent. Existing setup start and stop signatures remain supported.

## 0.4.0

- Coordinate setup visibility, stale command rejection, owned consent & off-UI resource drain with SetupAudioScope.
- Own each microphone meter worker with an app-held MicMeter. Construct it at runtime, cancel an epoch with stop, and close or drop off the UI thread to drain its capture.
- Hold a MicrophonePermissionSession for setup consent. Windows callers migrate to the session request method, which owns and drains the grant watch. The legacy free request remains available on other platforms.
- Declare the actual lifetimes of capture registration, calibration children and setup workers.

## 0.3.10

- Add calibration session completion with owned PCM cleanup, typed outcomes & host callbacks for model activation, engine execution and restart.

## 0.3.9

- Add typed errors for exact capture cuts, including a dropped stream and a release edge before its recording epoch. Existing cut callers retain their current interface.

## 0.3.8

- Seal capture at the original hotkey release using same-host monotonic timestamps. Retain delayed pre-release microphone packets and discard samples captured afterward before resampling.
- Reject missing timestamps, timeout, capture gaps, and overflow instead of returning an unverified clip.

## 0.3.7 (unpublished)

- Windows capture callback threads register once with MMCSS, preferring `Pro Audio` then `Audio`; registration failure remains advisory. Thread-local ownership retains each registration until callback-thread exit & reverts it on that same thread.
- MMCSS outcome crosses callback boundary as an atomic numeric status; consumer-side reads, metrics or stop report it once. No callback logging, per-buffer allocation or new public metrics fields; actual stream errors retain existing classification.

## 0.3.6 (unpublished)

- Added `setup::permissions`: macOS consent & Windows capability classification, device probes, Settings opening & grant polling. Explicit Windows denials take precedence over probes. Host retains UI-thread scheduling, telemetry sink & grant event.
- Added `setup::calibration_engine::{EngineRun, run_engine_calibration}`: host-selected engine, environment, execution timeout & stdout cap with owned process-tree cleanup & host-defined JSON results. Uses child-process APIs available in 0.5.12.
- Windows consent timeout cancels & drops its WinRT operation without leaving an indefinitely blocked join thread; each request supersedes previous grant watcher before classification.

## 0.3.5 (unpublished)

- Added `setup` under feature `capture`: microphone metering, bounded calibration recording & host media restoration. Device policy, renderer events, settings & file ownership stay with host. Calibration PCM stays unconditioned.
- Calibration deadline also caps elapsed time when callbacks stall; blocking reads use remaining budget, then release stream before restoring media.
- Meter reopen drops prior stream first & serializes opens against stop. Recorder stop, cancel & supersede serialize worker teardown; recorder drop stops & joins its worker. Constructors open no device; hosts start only from user action with existing OS grants.

## 0.3.4 (unpublished)

- Fixed: `prewarm_ring_storage` left an unused binding after the debug assertion it fed was removed, so `-D warnings` clippy failed the crate. Behaviour unchanged.
- Declares the permissions the default `capture` feature needs in `[[package.metadata.rightkit.permissions]]`: `NSMicrophoneUsageDescription` and `com.apple.security.device.audio-input` on macOS, so release tooling can check an app against them instead of each app rediscovering them (rightkit#1346 item 5). No source change.

## 0.3.3 (unpublished)

- #970 (audio part): `OutputSession::set_idle_release(Some(after))` pauses the device stream while the queue is empty or the session paused for `after`, so an idle session runs zero audio callbacks; a push, `resume` or queued audio restarts it (`is_idle_released`). `OutputSession::service()` (call every 100 to 250 ms from the producer thread) retries a pending device-loss reopen, follows the default output device when the session was opened with `None` (at most one check a second; the new device must offer the session's rate and channel count), and applies the idle release. Not compiled or run on a device. The allocation-counting test of #970 is declined: tests are end-to-end only, and the callback already allocates nothing (its scratch buffer is made before the stream starts).

- Added `wav::read_wav_mono_f32_prefix(path, seconds)` (#740): reads only the first `ceil(seconds * sample_rate)` frames as mono `f32` and never decodes the rest. A shorter file returns all of its samples; a non-finite or non-positive length is refused. `read_wav_mono_f32` and `read_wav_16k_mono` are unchanged in behaviour.

## 0.3.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.3.1 (2026-10-17)

- Orthic-Labs/rightkit#454: new `mic_align` (`align_mic`, `AlignParams`, `MicAlignment`): mic-to-camera lag by mean-removed normalized cross-correlation, coarse 4x-decimated full-range pass then a full-rate fine pass, with confidence and peak margin. New `room_fill` (`room_tone_fill`, `room_tone_bed`, `FillParams`, `ToneFill`, `rms`, `encode_mono_f32_wav`): looped room-tone fill with equal-power crossfaded seams, scaled to the measured RMS plus `gain_db`. Not compiled or run.
- Lag failures (short, silent or low-correlation input) return `AudioError::Invalid` with a `no mic alignment` message; `AudioError` gains no variant.

- #413 (audio part): `playback::play_once(path)` blocks until a WAV has played through the default output device. It downmixes to mono, resamples linearly, duplicates across device channels, and fails with `Playback` past the file duration plus five seconds. Not run on a device.
- #248, #326 and #449 source side is already on main (`playback::OutputSession`, `wav::read_wav_16k_mono` single open with capacity hint). The 0.3.x publish in #449 is not part of this entry and needs a version bump.

## 0.3.1

- Orthic-Labs/rightkit#326 (NOT COMPILED OR RUN): `read_wav_16k_mono` opens the file once (it used to open it twice, once for the header and once in `read_wav_mono_f32`) and sizes the sample buffer from the header's data length. `read_wav_mono_f32` shares the same decode and gets the same capacity hint. Output and errors are unchanged.

- Android capture (Orthic-Labs/rightkit#156): `capture` builds for `*-linux-android` on cpal 0.18's AAudio host with no Windows or Apple dependency pulled in (those are target-gated). New `capture::android` (`cfg(target_os = "android")`): `RECORD_AUDIO_PERMISSION`, `is_sco_route` and `sco_route_changed` for Bluetooth SCO route recovery. `CaptureErrorKind::DeviceChanged` is terminal on Android (a disconnected AAudio stream must be reopened). `capture_open_error` adds a `RECORD_AUDIO` hint on Android. Not built or run on a device.

## 0.3.0

- New feature `playback` (off by default, uses the existing optional `cpal` 0.18.2, no new dependency): `playback::{list_output_devices, OutputSession, OutputControl, OutputDeviceInfo, OutputFormat, OutputStats}`. `OutputSession::open(device)` reports the actual sample rate and channels; interleaved `f32` samples go through a bounded lock-free SPSC ring written in the crate (capacity set at open, `open_with_capacity`). Underrun plays silence; overrun returns the accepted count and never blocks. Latency-adjusted `played_samples()` and `position()` for use as an A/V master clock; `pause`, `resume`, `flush` (drops queued samples and resets the clock), `set_volume` and `set_muted` reach the callback through atomics with no allocation or locking. Underrun and overrun counters, errors as `AudioError`, reopen of the default device after a device loss, and a `Drop` that releases the stream. Tracked in Orthic-Labs/rightkit#248.
- Added `AudioError::Playback(String)`. This is a new variant on a public non-`#[non_exhaustive]` enum, so a downstream exhaustive `match` on `AudioError` needs a new arm; that is why this is a minor release.


## 0.2.3

- Republished on the renamed dependencies: `rightkit-ffmpeg` (was `rightkit-media`). The 0.2.2 on crates.io was packaged before the renames and still named the old crates.
## 0.2.2

- New `Filter::pos(value)`: a positional option rendered as the bare value (`aresample=44100`, `atrim=0:5`, `volume=0.22`), so an app can reproduce an existing FFmpeg filter string byte for byte. Named options are unchanged.
- `rightkit-media` is depended on with `default-features = false`.

## 0.2.1

- `tracing` is now an optional dependency enabled by the `capture` feature. Only `capture::session` and `lifecycle` log, and both are `capture`-gated, so `--no-default-features` (pure filter-graph consumers) no longer builds `tracing`. Default builds are unchanged.
