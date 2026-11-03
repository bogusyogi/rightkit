# Changelog

## 0.4.0 (unpublished)

Removed (breaking): `fake::FakeBackend`, the scripted backend that wrote placeholder media (Adrian, 2026-11-03: "no dummy data, no fucking made up shit. real data."). The recorder runs over the real macOS and Windows backends only.

Additive: whole-screen capture with step markers (rightkit#443; SwiftRight's flicker probe records the whole virtual screen at 60 fps and today needs ffmpeg). Not built or run: written without compiling; the next gate run on a Windows host and on a Mac is the first compile, and the acceptance is a device run (below).

- **New: `ScreenRecorder`** (`src/screen.rs`), the whole-screen sibling of the window `Recorder`: `ScreenRecorder::start(ScreenSpec::new(ScreenTarget::Virtual | Monitor(n), fps, lossless, out), backend)`, or `start_default(spec)` for the platform backend. `mark(label)` stamps a label on the frames' own clock and appends `{"seq","label","hostNs","utcMs"}` to `markers.ndjson` (flushed at once; `ScreenSpec::markers` moves the file). `pump()` reports a mid-take failure, `stop()` returns a `ScreenTake` and writes `screen.json`, `abort()` keeps what is on disk. The spelling is `ScreenRecorder::start`, not `Recorder::screen`: `Recorder<B, C>` is generic over the window `Backend`, so an associated `screen` on it could not name its type parameters.
- **New: `ScreenReader`**: `open(dir)`, then frames in order with their `host_ns` (`next_frame` or `Iterator`), `markers()`, `clock()`, `summary()`; `Marker::active_at(markers, host_ns)` is the marker in force at a frame. Nothing needs ffmpeg.
- **New: `rkv` module, the lossless container `RKV1`.** FFV1 was the old path (ffmpeg `gdigrab`), but neither Media Foundation nor AVFoundation encodes a lossless codec and an FFV1 encoder is not in the dependency set, so whole-screen capture writes its own small container: per frame the byte-wise XOR with the previous frame, run-length coded (zero runs and literals). It is exactly lossless, a static desktop costs a few bytes per frame, a killed capture keeps every whole frame, and the Rust writer, the Swift writer and the Rust reader share one documented layout. `LosslessWriter`, `LosslessReader`, `RawFrame`.
- **New: Windows capture** (`WindowsScreen`, `src/windows/screen.rs`): Windows.Graphics.Capture monitor items (`CreateForMonitor`), monitors listed through DXGI, composited on one canvas for `Virtual`, frames stamped with `SystemRelativeTime` (QPC, `clock` = `qpc_ns`), paced to the fps with the same pacer and staging-slot pattern as the window capture.
- **New: macOS capture** (`MacScreen`, `src/macos/screen.rs`; Swift `helpers/macos/.../ScreenCapture.swift`): the helper's new `capture-screen` and `screens` modes capture ScreenCaptureKit displays (one `SCStream` per display, composited for `Virtual`) and write `screen.rkv` themselves; frames carry `CLOCK_UPTIME_RAW` nanoseconds (`clock` = `uptime_ns`). The window protocol is unchanged. Mixed-DPI virtual desktops keep each display at its native pixels placed by its point origin times the display's scale; they can overlap or leave gaps.
- **New: CLI** `rightkit-record-helper capture --screen --fps <1..120> --lossless --out <dir> [--target virtual|monitor:N] [--seconds <n>] [--markers <file>]` and `rightkit-record-helper monitors`, on Windows and macOS. stdin takes `mark <label>` and `stop`; stdout is JSON lines (`started`, `marker`, then the summary). `--lossless` is required.
- **Decision: lossless only.** `ScreenSpec::lossless == false` is refused with `Code::Unsupported`; a compressed take of one window is the window recorder's job. The parameter stays so a compressed mode can be added without an API break.
- Not covered: audio, cursor pixels (never captured), a compressed whole-screen mode, per-display scale resampling.
- Acceptance (device runs, not done here): on Windows and on a Mac, record the virtual screen at 60 fps with markers, read it back with `ScreenReader`, and run SwiftRight's H19 flicker probe (SwiftRight #15) on it with no ffmpeg installed.

## 0.3.4 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.3.3 — unpublished staging

- Raises rightkit-sidecar to 0.3.0 and rightkit-credentials to 0.4.0 for dependency compatibility. rightkit-credentials 0.4.0 is not yet on crates.io (latest 0.3.0), so this crate cannot publish until it is.

## 0.3.2 (2026-10-17)

- Depend on rightkit-child-process 0.5.0 (and rightkit-sandbox 0.5.0 where used); no behaviour change.

## 0.3.1

Additive. Nothing compiled locally; verification is the CI run. Requests: rightkit#176 (CutRight).

- **New: `StopReason::{Requested, Killed}` and `Recorder::record_stop_with(reason)`.** A kill-hotkey stop used to seal the take like a clean stop, so "killed" existed only in the host's audit log. `Killed` finishes the media as usual but seals the bundle `partial_quarantined` with manifest reason `killed` (`invalid`, reason `killed: no_accepted_frames`, when no frame was accepted); it is never `complete`. `record_stop()` is `record_stop_with(StopReason::Requested)`, unchanged. A take that already ended keeps its outcome whatever reason a later stop passes. `StopReason` is `non_exhaustive`.
- **New: `Recorder::host_ns() -> Option<u64>` and `Backend::host_ns()`** (default `None`, so existing backends compile). The current time on the clock frames and events carry as `hostNs`, so a host can stamp the events it appends on the frames' timeline instead of on time since the take began. `WindowsBackend`: QPC in nanoseconds (the clock of `host_ns_at_start`). `MacBackend`: `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`, the helper's `uptimeNs`. `ProcessBackend` alone reports `None` (its clock is in another process). `FakeBackend::host_now_ns` sets it for tests.
- Tests: `tests/killed_and_host_clock.rs`.

## 0.3.0

First published release. `publish = true`; no behavior change from 0.2.0 beyond packaging.

- Captures any window chosen by `window_id` and `pid`: ScreenCaptureKit on macOS, Windows.Graphics.Capture on Windows. Never sends input.
- Dependencies are published crates: `rightkit-contracts` 0.1.5, `rightkit-sidecar` 0.2.0, `rightkit-credentials` 0.2.0, `rightkit-child-process` 0.4.0 (macOS).
- The macOS helper sources (`helpers/macos`) are included in the package.
- README documents the Screen Recording permission requirement (macOS) and scope.

## 0.2.0

Never released separately (folded into 0.3.0). Reader additions for CutRight's contract asks (C2/RK9/RK3/RK4, plan section 3).

- `BundleReader::open_complete(dir, &VerifyOptions)`: opens only a bundle sealed `complete` that passes `bundle_verify`. A quarantined, partial or invalid bundle is refused with `Code::NotComplete`, and a failed verification with `Code::VerifyFailed`. Both messages name the reason. `open()` is unchanged and still accepts every bundle.
- `Code::NotComplete` (`bundle_not_complete`) and `Code::VerifyFailed` (`bundle_verify_failed`).
- `BundleReader::scenario()`: the scenario id and each step's `stepId`, `label` and `say`, read from `scenario.json`.
- `BundleReader::events()`: streams `events.ndjson` as `RecordEvent`s, one line at a time. A bad line yields an error and ends the stream.
- `BundleReader::clock_map()` and `ClockMap`: `host_to_sample_pts(ns)` and `sample_to_encoded_pts(ns)`. The encoded-PTS offset is read from `mapping.samplePtsToEncodedPtsOffsetNs`, defaulting to 0. The recorder writes 0 today.
- `BundleReader::frame_at_target(step_id, target_id)`: resolves a postcondition by the `target.targetId` of its event. No recorder change: the event's existing target id is passed through. Bundles recorded without one return `not_found`.
- `BundleReader::cursor_assets()` and `cursor_assets_status()`: read `cursors/manifest.json` (schema `rightkit.record.cursor-assets.v1`). Without the manifest, the list is empty and the status is `CursorAssetsStatus::NotRecorded`. The recorder does not write cursor assets yet.
- README: the clean.mp4 decode rule, the cursor manifest shape, and the target-id key.
