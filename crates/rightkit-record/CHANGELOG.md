# Changelog

## 0.2.0

Unpublished. Reader additions for CutRight's contract asks (C2/RK9/RK3/RK4, plan section 3).

- `BundleReader::open_complete(dir, &VerifyOptions)`: opens only a bundle sealed `complete` that passes `bundle_verify`. A quarantined, partial or invalid bundle is refused with `Code::NotComplete`, and a failed verification with `Code::VerifyFailed`. Both messages name the reason. `open()` is unchanged and still accepts every bundle.
- `Code::NotComplete` (`bundle_not_complete`) and `Code::VerifyFailed` (`bundle_verify_failed`).
- `BundleReader::scenario()`: the scenario id and each step's `stepId`, `label` and `say`, read from `scenario.json`.
- `BundleReader::events()`: streams `events.ndjson` as `RecordEvent`s, one line at a time. A bad line yields an error and ends the stream.
- `BundleReader::clock_map()` and `ClockMap`: `host_to_sample_pts(ns)` and `sample_to_encoded_pts(ns)`. The encoded-PTS offset is read from `mapping.samplePtsToEncodedPtsOffsetNs`, defaulting to 0. The recorder writes 0 today.
- `BundleReader::frame_at_target(step_id, target_id)`: resolves a postcondition by the `target.targetId` of its event. No recorder change: the event's existing target id is passed through. Bundles recorded without one return `not_found`.
- `BundleReader::cursor_assets()` and `cursor_assets_status()`: read `cursors/manifest.json` (schema `rightkit.record.cursor-assets.v1`). Without the manifest, the list is empty and the status is `CursorAssetsStatus::NotRecorded`. The recorder does not write cursor assets yet.
- README: the clean.mp4 decode rule, the cursor manifest shape, and the target-id key.
