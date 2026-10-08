# rightkit-media

FFmpeg safety primitives for Right Suite desktop apps. The crate owns the catalog of approved FFmpeg and ffprobe builds, the checks that decide whether a binary may run, the argument builders, the process runner, and the loudness normalization loop. It contains no FFmpeg binary, no app policy, no model, and no shell invocation.

**Use it** when an app must run FFmpeg or ffprobe on user media and needs the binary to be a verified, catalog-approved build, or needs a documented development fallback.

**Do not use it** to decode or encode media in-process, to choose app-level media policy, or to call FFmpeg through a shell. Arguments are built as `OsString` values and run without shell interpolation.

## Capability

- **Catalog selection.** Select exactly one approved record per tool (`ffmpeg`, `ffprobe`), variant (`decode`, `export-ass`), OS, and architecture (`aarch64` or `x86_64`). Records in `pending` state (recipe and spec recorded, digest not yet produced) are never resolvable.
- **Verification.** Validate catalog metadata and SHA-256 before probing or execution. Require the approved version banner (`ffmpeg version` / `ffprobe version`) and the mandatory `--disable-gpl --disable-nonfree --disable-autodetect` flags. Reject `--enable-gpl`, `--enable-nonfree`, and GPL encoder libraries, case-insensitively.
- **Licence record.** Each statically linked component is recorded with licence, source, digest, enable flag, and notice. The entry's SPDX expression is FFmpeg's LGPL `AND` each component's.
- **Resolution.** Resolve catalog binaries only from an explicit override, a bundled path, or an explicitly enabled development path. Non-catalog binaries come only from `unverified::resolve_unverified` (configured path, named environment variable, or opt-in `PATH`). These are labelled `UNVERIFIED`, expose encoder, decoder, and filter probing, and are refused in `BuildMode::Release`.
- **Staging.** Stage a verified binary as a Tauri `externalBin` (`binaries/<tool>-<target-triple>[.exe]`, mode `0755`, re-hashed, idempotent) with `stage_external_bin`.
- **Process runner.** Run verified FFmpeg through `rightkit-process`, which owns the child tree and reaps descendants on exit, cancellation, timeout, failure, and drop.
- **Loudness.** Normalise loudness by rendering gain and a limiter, then re-measuring until the kept render is within ±0.5 LU (configurable, never looser). The loop is bounded to 5 renders by default (port of GenRight `transcode_normalized`).
- **Audio contract.** The shared audio contract is WAV, mono, 16 kHz, `pcm_s16le`.

The artifact catalog (`catalog/artifacts.json`) records binary and source digests, size where recorded, the official source URL, build provenance, the full LGPL notice, components, and the required configure flags. `recipes/build-ffmpeg-macos.sh` reproduces the macOS entries from the sources pinned in `recipes/ffmpeg-sources.json`. That file also records each source's upstream evidence.

## Variants

- `decode`: native codecs only (Windows x86_64; macOS aarch64 and x86_64; ffprobe on macOS).
- `export-ass`: `decode` plus libass (`subtitles`/`ass` burn-in), FreeType and HarfBuzz (`drawtext` captions), zlib, and VideoToolbox/AudioToolbox (`h264_videotoolbox`). SPDX `LGPL-2.1-or-later AND FTL AND MIT-Modern-Variant AND ISC`. macOS aarch64 is approved. macOS x86_64 is built and reproducible but stays `pending` until its probes run on an x86_64-capable host. See `third-party/BUILD-NOTES-ffmpeg-export-ass-macos.txt`. `tests/export_ass_e2e.rs` burns an `.ass` subtitle and a `drawtext` caption into a VideoToolbox MP4 and checks the decoded pixels, when the binary is present in `vendor/ffmpeg-lgpl/`.

## Example

Illustrative; not compiled by the crate. The types and functions named here exist in `src/lib.rs`. For compiled usage, see `tests/media_e2e.rs` and `tests/catalog_tools_e2e.rs`.

```rust,ignore
use rightkit_media::{
    resolve_ffmpeg, verify_ffmpeg, ArtifactCatalog, ResolutionRequest, APPROVED_CATALOG_JSON,
};
use std::time::Duration;

let catalog = ArtifactCatalog::parse(APPROVED_CATALOG_JSON)?;
let artifact = catalog.approved("macos", "aarch64")?;
let request = ResolutionRequest {
    override_path: None,
    bundled_paths: vec![/* app bundle paths */],
    development_path: None,
    allow_development_path: false,
};
let resolved = resolve_ffmpeg(artifact, &request)?;
let verified = verify_ffmpeg(resolved, Duration::from_secs(10))?;
```

## Features

None. The crate has no Cargo features.

## Platform support

- macOS aarch64: `decode` (ffmpeg, ffprobe) and `export-ass` (ffmpeg) approved.
- macOS x86_64: `decode` (ffmpeg, ffprobe) approved; `export-ass` pending.
- Windows x86_64: `decode` (ffmpeg) approved.
- `cfg(windows)` and `cfg(unix)` branches exist only in `src/staging.rs` and `src/lib.rs`.

Availability is defined by `catalog/artifacts.json`, not by this README. Check the `status` field of each record.

## Verification

Run through the RightKit wrapper, per the workspace's `docs/rules/rightkit.md`:

```text
rightkit cargo test -p rightkit-media --all-targets
rightkit cargo fmt --check
rightkit cargo clippy -p rightkit-media --all-targets -- -D warnings
rightkit cargo package -p rightkit-media --allow-dirty --no-verify --list
```

## Publication

Published version: see `INDEX.md` at the repository root. This README does not state release status. Publish through `right-release publish cargo` only after the release gate is satisfied (see the workspace's `docs/rules/release-signing.md`).

## Licence

Licensed under either MIT or Apache-2.0, at your option. FFmpeg artifacts retain their own LGPL-2.1-or-later licence and attribution obligations (see `third-party/`).

## Changes

This crate has no `CHANGELOG.md`. Version and status history is in `INDEX.md`.
