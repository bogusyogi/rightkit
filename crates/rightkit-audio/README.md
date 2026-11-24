# rightkit-audio

Reusable audio primitives. The crate provides typed FFmpeg filter graphs, loudness measurement and gates, room tone, ducking, WAV I/O, and optional microphone capture with resampling, downmixing and recovery. Presets are bundled from the `presets` directory.

Catalog capability: `audio`.

## What it owns

- Filter graphs (`filter`, `chain`, `ducking`, `room_tone`, `bus`, `preset`). Graph builders are pure: they never spawn processes.
- Measurement and offline renders (`measure`, `profile`). These run FFmpeg through `rightkit-ffmpeg`, so cancellation, timeout and process-tree ownership are uniform.
- WAV I/O (`wav`): `read_wav_mono_f32`, `read_wav_16k_mono`, `write_wav_mono_i16`.
- Live capture (`capture`, feature `capture`) and the Windows endpoint and power lifecycle (`lifecycle`).
- Setup metering & calibration (`setup`, feature `capture`): epoch-owned microphone meter, bounded PCM recorder, restore-once host media guard, OS microphone consent & owned calibration engine runner.
- Audio output (`playback`, feature `playback`): `list_output_devices`, `OutputSession` (push, pause, resume, flush, volume, mute, clock, counters, device-loss recovery) and the `OutputControl` handle for other threads.
- One-shot playback (`playback::play_once`, feature `playback`): decode a WAV and play it to the default output device, blocking until it ends.
- Mic-to-camera lag (`mic_align`: `align_mic`) and equal-power looped room-tone fill (`room_fill`: `room_tone_fill`, `room_tone_bed`).

## When to use it

- Use it to build or run FFmpeg audio chains for narration, dialogue cleanup, ducking or room tone.
- Use it to read and write WAV files.
- Use it for microphone capture when a product needs resampling and downmix in one place.
- Use it to play decoded audio through an output device (feature `playback`): `OutputSession` takes interleaved `f32` samples through a bounded lock-free ring and exposes a latency-adjusted playback clock for A/V sync.

Do not use it for transcription (see `rightkit-asr`), or to decode or remux media for playback (see `rightkit-playback`, which prepares files and does not output audio).

## Cargo features

| Feature | Default | Pulls in | Adds |
|---|---|---|---|
| `capture` | yes | `tracing` 0.1, `cpal` 0.18.2, `rubato` 5.0.1, `ringbuf` 0.6, `rightkit-child-process` 0.5.12; macOS `objc2-core-audio`, `objc2-core-foundation`, `objc2`, `objc2-foundation`, `block2`; Windows `windows`, `windows-core` | Live microphone capture (`capture`), setup metering, consent & calibration (`setup`), Windows endpoint & power lifecycle (`lifecycle`). Without it, crate provides filter graphs, measurement, WAV I/O & room tone. |
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

## Setup lifecycle

Keep a `MicMeter` and `MicrophonePermissionSession` in host-owned setup state. Constructors open no device and start no worker. Call `begin` and `start` after a user action and an existing grant, and `stop` to invalidate the current epoch without waiting. The next start drains its predecessor before opening capture. Use `session.request(&host)` off the UI thread for microphone consent. On hide or suspend, stop the meter and cancel consent immediately. Drain both with `close` off the UI thread on teardown. Close is terminal, so construct fresh owners to reopen. Callbacks that refer to an owner should hold a weak reference.

In 0.4.0, `MicMeter::new` requires runtime construction and Windows callers move from the free request function to `MicrophonePermissionSession::request`. The free API on other targets is unchanged.

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
- `tests/audio_e2e.rs`, `resampler_and_capture_gate_are_explicit` (the capture half): a configured virtual loopback input device. The resampler assertions run on every host.

## Setup metering & calibration

Use `setup::scope::SetupAudioScope` to own metering & consent across setup visibility changes. Capture `meter_ticket()` (`SetupAudioMeterTicket`) when accepting each start or device change, before dispatching `start_reserved_meter` to an existing blocking executor. Call nonblocking `stop_meter_now()` immediately on that same serialized command ingress; never dispatch it for later execution. New receipts & stops invalidate queued meter starts without revoking consent. Newest valid start waits off UI only for its admitted predecessor; newer choices, stop & hide wake obsolete waiters. `MicMeter` drains prior capture before opening another device. Device/OS calls have no timing bound. Each meter token admits one attempt, including failures; an unconsumed `Draining` refusal permits same-token retry after drain.

Existing `start_meter(&SetupAudioTicket, ...)` & `stop_meter(&SetupAudioTicket)` signatures remain available. Capture a fresh `ticket()` per legacy command before dispatch. Tickets carry monotonic command receipts. Legacy starts reject older or already-used receipts before hardware access & return `Busy` for an active predecessor. Legacy stops reject receipts older than an admitted meter action, so an earlier stop cannot cancel a later legacy or reserved start. A later stop can cancel an earlier start even when both tickets were captured before either worker ran. Consent uses `ticket()` independently of receipt order. Use reserved starts & immediate stop for receipt-ordered device changes.

Scope rejects stale tickets & hidden starts. `suspend` revokes old work, `activate` records explicit reveal, & off-UI `drain` releases retired work before new operations proceed. Both stop APIs preserve consent. Invoke hide/suspend/close synchronously at lifecycle ingress; defer only resource drain, never another cancellation that could revoke later work. For ordinary exit, call terminal `close` then `drain` off UI before releasing final owner. Host owns window lifecycle, existing OS grant checks, explicit user action, device selection, event routing & copy.

`setup::mic_meter::{MicMeter, MeterConfig, MeterEvent, MeterStatus, MeterStartError, level}` provides metering with first-buffer probing, one reopen & stale-epoch protection. Use `begin` then `start`; call `stop` on setup exit, hide, suspend & close. Stop invalidates its epoch; worker drops its stream after its current bounded read. Reopen drops old stream before opening its replacement.

`setup::calibration_capture::{Recorder, CaptureSettings, CaptureDevice, StartHooks, StartError, StopOutcome, CancelOutcome}` provides session-owned calibration capture. Start supersedes prior recording. Stop returns unconditioned mono f32 samples; cancel discards them. Stop, cancel, supersede & recorder drop stop & join workers. Sample ceiling, wall-clock deadline or capture failure releases microphone. Each blocking read waits at most 500 ms & at most remaining wall-clock budget; stalled callbacks cannot extend recording indefinitely. Media restoration uses `setup::media_guard::{MediaLifecycle, MediaPauseGuard}` & runs once per guard.

Helpers: `selected_capture_device(choose)`, `f32le_bytes(samples)` & `sweep_files_with_extension(dir, extension)`. Host supplies device policy, settings, session ids, stale-file cleanup, media callbacks & renderer events. Callbacks must return normally & avoid re-entering their meter or recorder.

Constructors open no microphone. Call `start` only for an explicit user action after verifying an existing OS microphone grant. Cancel recording when setup exits or its window hides, suspends or closes.

`setup::permissions::{PermissionHost, microphone_access_status, request_microphone_access, open_macos_microphone_settings}` provides OS consent. Host supplies telemetry sink, UI-thread scheduler & grant callback; call status & request from blocking workers. macOS waits up to 120 s for consent. Windows requests capability access on host's UI thread, waits up to 5 s for dispatch plus 120 s for consent, preserves explicit denials & refines other results with device probes. Settings grant polling checks every 2 s for 60 s; each new request supersedes prior watcher. Hardware enumeration/open calls follow native driver timing. Host owns renderer event names & copy.

`setup::calibration_engine::{EngineRun, run_engine_calibration}` runs host-selected engine over host-owned PCM path & decodes stdout into host's result type. Host supplies subcommand, environment, timeout & stdout cap. Runner owns process tree, execution deadline, bounded output capture & cleanup; PCM files, wire structs, model activation & engine restart remain host responsibilities. Capture PCM stays unconditioned.

`setup::calibration_session` coordinates recorder stop, an owned PCM file, host model and engine callbacks, and resident restart after a passing report. It reports PCM cleanup separately and can sweep owned files from dead processes. Hosts supply paths, process hooks, wire formats and copy.
