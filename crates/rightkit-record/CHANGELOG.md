# Changelog

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
