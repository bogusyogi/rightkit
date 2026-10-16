# rightkit-record

Per-window screen capture for any app, plus the recorder core that turns a capture into a verified
session bundle. A request names only a `window_id` and a `pid` (`src/plan.rs`), so any visible window
can be recorded, including apps this suite does not own. The recorder never sends input: it does not
click, type, scroll or move the pointer. Driving an app is `rightkit-automation`'s job.

| OS | Capture API | Where it lives |
|---|---|---|
| macOS | ScreenCaptureKit | Swift helper `helpers/macos` (`rightkit-record-macos-helper`), launched by `src/macos` |
| Windows | Windows.Graphics.Capture + Media Foundation | `src/windows`, in process or via `rightkit-record-helper serve` |

## Permissions

Capturing another app's window needs the operating system's consent. The crate never prompts and never
bypasses it; with no grant, start fails closed with `Code::PermissionDenied`, and `Backend::permission()` reports it up front.

- **macOS: Screen Recording permission is required.** It is granted per signing identity and
  responsible process, in System Settings > Privacy & Security > Screen & System Audio Recording. The
  helper reads it with `CGPreflightScreenCaptureAccess`, which never prompts. The identity that needs the
  grant is the helper (`dev.rightkit.record.macos-helper`) as launched by the host app (its responsible
  process), so a host must request the grant itself and a re-signed helper starts with none. After the user
  grants it, macOS may require the host app to restart before it takes effect.
- **Windows:** Windows.Graphics.Capture needs no permission prompt for a window capture, but a yellow
  capture border is shown unless borderless access is granted (see "Windows borderless capture").

Callers must obtain the user's informed consent before recording another app, and should keep bundles
local. See `rightkit-credentials` for the per-installation redaction key.

## Scope

The macOS helper sources ship in this crate (`helpers/macos`, built with `build.sh`; a release
must be Developer-ID signed and notarized, an ad-hoc build must never ship). Find it with
`RIGHTKIT_RECORD_MACOS_HELPER` or place it next to the running executable. The record core itself never
captures pixels: a per-OS helper implements the `Backend` trait and reports frames, geometry, losses and
failures as `HelperEvent`s.

## Modules

- `backend`: the `Backend` seam, `BackendPlan`, `StartInfo`, `HelperEvent`, `Failure`, `Permission`, `Codec`.
- `recorder`: the public take lifecycle over a `Backend`. A take ends `complete`, `quarantined` or `invalid`; `complete` needs a clean stop and an accepted readiness marker. `record_stop_with(StopReason::Killed)` (a kill hotkey) never seals `complete`: the bundle is `partial_quarantined` with reason `killed`. `Recorder::host_ns()` reads the clock frames and events carry as `hostNs` (`None` when the backend cannot), so a host stamps its own events on the frames' timeline.
- `plan`, `redact`, `verify`, `reader`: the request, per-field redaction, bundle verification, and the bundle reader (step and target to frame, scenario, events, clock map, cursor assets).
- `protocol`: the helper wire protocol. Length-prefixed JSON frames (`rightkit-sidecar`) with `request`, `response`, `event` and `bye` kinds.
- `process`: `ProcessBackend`, the core's `Backend` over a helper's stdio, and `serve_backend`, which lets a helper serve any `Backend`.
- `windows` (Windows only): Windows.Graphics.Capture and Media Foundation capture for one HWND (`src/lib.rs:47-49`).
- `macos` (macOS only): ScreenCaptureKit capture through the Swift helper in `helpers/macos`; the module launches it and speaks the helper protocol (`src/lib.rs:50-53`).

## Recoverable media (R14)

The helpers write a fragmented MP4, so a helper killed mid-take leaves `clean.mp4` playable up to its
last whole fragment (about half a second plus encoder latency is lost). macOS runs `AVAssetWriter` in segment mode
(`outputFileTypeProfile = .mpeg4AppleHLS`, 0.5 s segments) and appends and fsyncs each segment itself,
because `movieFragmentInterval` has no effect on a file-URL MP4 writer (measured). Windows sets
`MF_TRANSCODE_CONTAINERTYPE = MFTranscodeContainerType_FMPEG4` on the sink writer (`src/windows/mf.rs`,
unverified on Windows).

When a take ends `quarantined` or is recovered from a stale state, the core probes the media
structurally (`media::probe_mp4`: ftyp, moov, and every complete `moof`/`mdat` pair; a fragment whose
`mdat` is cut short does not count). Playable media is kept and its figures are appended to the manifest
reason (`helper_lost; recovered_media frames=118/149 duration_ms=4006`) and returned as
`Outcome::recovered_media` / `Recovered::media`. Media that does not play ends the take `invalid` with
`media_unplayable`. The `accepted` count is the bundle reader's frame table.

## Redaction hashes

A `hash` field stores HMAC-SHA256 keyed by a per-installation secret that never enters the bundle. The
secret lives in the OS credential store (`rightkit-credentials`, service `dev.rightkit.record`, account
`record.redaction-key.v1`, minted on first use). `StartRequest::key_source` (a `KeyHandle` wrapping a `KeySource`, `src/plan.rs:64`) supplies it;
tests inject `FixedKeySource`. A take that hashes a field and cannot read the key is refused before
anything is written. The bundle records only the key id (`scenario.json` `redaction.keyId`, plus
`hashAlgorithm`), and each hashed event carries `saltId` `hmac-sha256:<keyId>`. Equal values hash equal
within an installation. Digests are written `hmac-sha256:<64 hex>` (record-event contract 1.2), and
`bundle_verify` requires the digest prefix to match the `saltId` algorithm (`hmac-sha256:` or, for legacy
`label:` salts, `blake3:`).

Migration: bundles written before this change carry label-salted digests (`saltId` `label:<field>`, no
key id). They still read and verify; `bundle_verify` reports them in `VerifyReport::warnings` as weak
redaction (anyone can confirm a guessed value). New bundles never use label salts. Existing digests
cannot be re-keyed: the plaintext is gone.

## Reading a sealed bundle

`BundleReader` is the consumer side. Two ways to open a bundle:

- `BundleReader::open(dir)` reads any bundle without re-hashing it. A quarantined,
  partial or invalid bundle opens too, and `status()` reports it.
- `BundleReader::open_complete(dir, &VerifyOptions)` opens only a bundle sealed
  `complete` that passes `bundle_verify`. A `partial_quarantined` or `invalid` bundle
  is refused with `bundle_not_complete`, and the message names the status and the
  manifest reason. A bundle that fails verification is refused with
  `bundle_verify_failed`, and the message names the first failed checks. Consumers
  that import takes as finished use this form.

Accessors:

- `scenario()`: the scenario id, and each step's `stepId`, `label` and optional `say`,
  in declaration order (`scenario.json`).
- `events()`: `events.ndjson` as `RecordEvent`s, streamed one line at a time. A line
  that does not parse yields an error with its line number, and the stream ends there.
- `step_index()`, `frame_at(step_id, description)`: the step's first and last host time,
  and the first source frame that shows a postcondition by its description text.
- `frame_at_target(step_id, target_id)`: the same resolution, keyed by the
  postcondition event's `target.targetId`. The recorder passes the id through unchanged.
  The `postcondition` object is `additionalProperties: false` in record-event v1, so the
  target id lives on the event's existing `target` object, not on the postcondition.
  The earliest observed postcondition for the target wins. A bundle recorded without
  target ids returns `not_found` for every target.
- `clock_map()`: a `ClockMap` with `host_to_sample_pts(ns)` and
  `sample_to_encoded_pts(ns)`, both clamped at zero. The host-to-sample offset is
  anchored to the first accepted frame. The encoded offset is
  `mapping.samplePtsToEncodedPtsOffsetNs`, which the recorder writes as 0 today, so
  encoded PTS equals sample PTS. A bundle without the field reads as 0.
- `cursor_assets()` and `cursor_assets_status()`: see "Cursor assets" below.

### Decoding clean.mp4

This crate never decodes video, and it exposes no `decode_frame`. A consumer decodes
`clean.mp4` through its own typed ffmpeg boundary and selects each picture by the
`sample_pts_ns` of the `SourceFrame` it wants (from `frames()` or `frame_at*`). Convert
the frame's time with `clock_map().sample_to_encoded_pts(sample_pts_ns)` before matching
decoder timestamps. Match by PTS, never by decode order or elapsed time. The raw frame
identity is `raw_blake3`: BLAKE3 over the captured frame as packed BGRA, rows top to
bottom, row padding removed (`helpers/macos/Sources/RecordWire/FrameHash.swift`). A decoded H.264
picture is in a different pixel format, so it does not reproduce `raw_blake3`. Use the PTS to find
the picture, and the hash only to identify the source frame it came from.

### Cursor assets

The cursor manifest is `cursors/manifest.json`, relative to the bundle root. The recorder
does not write it yet, so a bundle without it reads as `CursorAssetsStatus::NotRecorded`
with an empty list. A producer, when it exists, must:

- list `cursors/manifest.json` and each image in `hashes.json` `files`, so
  `bundle_verify` covers them. Until verify learns the `cursors/` directory, a sealed
  bundle that contains one fails verification as an unlisted file.
- write this shape:

```json
{
  "schema": "rightkit.record.cursor-assets.v1",
  "cursors": [
    { "id": "arrow", "path": "cursors/arrow.png", "hotspotX": 4, "hotspotY": 2, "width": 32, "height": 32 }
  ]
}
```

`id` is a record id (letters, digits, `_.:-`). `path` is relative to the bundle root and
must sit under `cursors/` with no `..`, root or prefix components. `hotspotX` and
`hotspotY` are in the image's own pixels from its top-left corner, and each must be less
than the image's `width` or `height`. The reader rejects a manifest that breaks these
rules, or whose `schema` is not `rightkit.record.cursor-assets.v1`.

## Helpers

- `rightkit-record-macos-helper` (Swift, `helpers/macos`): the macOS capture helper that `src/macos/mod.rs` launches.
- `rightkit-record-helper capture ...`: a one-shot Windows take that prints a JSON summary.
- `rightkit-record-helper serve`: the protocol over stdin and stdout, for rightkit-child-process. Windows only; other platforms print an `unsupported` result (`src/bin/rightkit-record-helper/main.rs:212`).

Process ownership (spawn, supervise, restart) belongs to rightkit-child-process. A
helper that dies mid-take is reported as `helper_lost` and the take is
quarantined, never completed.

## Windows capture notes

- **On-screen and off-screen windows both work.** `rightkit-record-helper capture` declares the window's real
  visibility (`windows::observed_visibility`). Off-screen capture only delivers frames when the target
  renders while off-monitor: a GPU-composited renderer does, a GDI or WinForms window does not (its paint
  is clipped, so WGC delivers about one frame). Capture a visible window when in doubt.
- **`boundsPt` is window pixels divided by the window's DPI / 96.** For a DPI-unaware window Windows reports
  96, so the numbers are physical pixels on a scaled display (a 640x360 window on a 250% display reports
  1600x900). Frame size is the capture item's size, which excludes the window shadow.
- **Build the helper with the optimised profile for real use.** The per-frame work (readback, BLAKE3,
  BGRA to NV12) is per-pixel; the workspace builds this crate at opt-level 3 even in dev profiles.
- The one-shot `capture` subcommand writes only `clean.mp4`; a manifest and bundle come from the
  `Recorder` path (`serve`).

## Windows borderless capture

The helper requests borderless access and calls `SetIsBorderRequired(false)` only when that
access reports `Allowed` (`src/windows/mod.rs:219-268`). The failure below was observed on one
host (XPS, build 26300). No test or evidence file in this repository records it, so it is unverified:
a byte-identical copy of a helper that captures borderless fails at a new path: `SetIsBorderRequired(false)`
returns `0x80070490` (Element not found), even though `GraphicsCaptureAccess::RequestAccessAsync(Borderless)`
reports `Allowed`.

- Install the helper at a stable path: the app's install directory or its `current` root.
  Never run it from a broker build path or a temp copy (unverified: see above).
- Grant consent once, at install or first run, for that installed path (unverified: see above).
- A refused setter is not fatal: the take keeps the border, the helper warns on stderr, and
  the manifest records `capture.border {required, access, error}`. `bundle_verify` warns
  "capture border visible".

Unverified: the consent store key is believed to be
`HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\graphicsCaptureWithoutBorder\NonPackaged`.
It has not been read on a host yet.

## Tests

`rightkit cargo test -p rightkit-record` runs the lifecycle, bundle, redaction, reader and
protocol tests on any host. The Windows capture code compiles only on Windows.
