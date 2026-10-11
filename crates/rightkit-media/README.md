# rightkit-media

FFmpeg safety primitives for Right Suite desktop apps. The crate owns the catalog of approved FFmpeg and ffprobe builds, the checks that decide whether a binary may run, the argument builders, the process runner, and the loudness normalization loop. It contains no FFmpeg binary, no app policy, no model, and no shell invocation.

**Use it** when an app must run FFmpeg or ffprobe on user media and needs the binary to be a verified, catalog-approved build, or needs a documented development fallback.

**Do not use it** to decode or encode media in-process, to choose app-level media policy, or to call FFmpeg through a shell. Arguments are built as `OsString` values and run without shell interpolation.

## Capability

- **Catalog selection.** Select exactly one approved record per tool (`ffmpeg`, `ffprobe`), variant (`decode`, `export-ass`, `export-ass-zimg`), OS, and architecture (`aarch64` or `x86_64`). Records in `pending` state (recipe and spec recorded, digest not yet produced) are never resolvable.
- **ffprobe JSON.** Typed streams carry exact `r_frame_rate`, `avg_frame_rate` and `time_base` rationals; `PacketSample::pts_seconds(time_base)` converts packet timestamps (`f64`).
- **Working directory.** `RunOptions::with_current_dir(dir)` (and `spawn_streaming_in`) runs the child in `dir` so relative filter files resolve; the directory is checked before spawn and the ffmpeg path is pinned absolute first.
- **Verification.** Validate catalog metadata and SHA-256 before probing or execution. Require the approved version banner (`ffmpeg version` / `ffprobe version`) and the mandatory `--disable-gpl --disable-nonfree --disable-autodetect` flags. Reject `--enable-gpl`, `--enable-nonfree`, and GPL encoder libraries, case-insensitively.
- **Licence record.** Each statically linked component is recorded with licence, source, digest, enable flag, and notice. The entry's SPDX expression is FFmpeg's LGPL `AND` each component's.
- **Resolution.** Resolve catalog binaries only from an explicit override, a bundled path, or an explicitly enabled development path. Non-catalog binaries come only from `unverified::resolve_unverified` (configured path, named environment variable, or opt-in `PATH`). These are labelled `UNVERIFIED`, expose encoder, decoder, and filter probing, and are refused in `BuildMode::Release`.
- **Bundled handle.** `verify_bundled_ffmpeg(path, variant, timeout)` and `verify_bundled_ffprobe(path, variant, timeout)` take the path of a binary shipped in the app, the catalog variant the app expects, and a probe timeout. They select the one approved row for that tool, variant and this host, check the content digest, run the LGPL build probe, and return a `VerifiedFfmpeg` / `VerifiedFfprobe`. The handle is `Clone`, implements `FfmpegBinary`, and is what every run, probe, decode, loudness and staging call takes, so an app verifies once at start-up and passes the handle everywhere. Repeat calls for the same unchanged file hit the verify cache (no re-hash, no second `-version`). The `_in` forms take an explicit catalog, OS and architecture.
- **Staging.** Stage a verified binary as a Tauri `externalBin` (`binaries/<tool>-<target-triple>[.exe]`, mode `0755`, re-hashed, idempotent) with `stage_external_bin`.
- **Process runner.** Run verified FFmpeg through `rightkit-process`, which owns the child tree and reaps descendants on exit, cancellation, timeout, failure, and drop.
- **Loudness.** Normalise loudness by rendering gain and a limiter, then re-measuring until the kept render is within ±0.5 LU (configurable, never looser). The loop is bounded to 5 renders by default (port of GenRight `transcode_normalized`).
- **Audio contract.** The shared audio contract is WAV, mono, 16 kHz, `pcm_s16le`.

The artifact catalog (`catalog/artifacts.json`) records binary and source digests, size where recorded, the official source URL, build provenance, the full LGPL notice, components, and the required configure flags. `recipes/build-ffmpeg-macos.sh` reproduces the macOS entries from the sources pinned in `recipes/ffmpeg-sources.json`. That file also records each source's upstream evidence.

## Variants

- `decode`: native codecs only (Windows x86_64; macOS aarch64 and x86_64; ffprobe on macOS).
- `export-ass`: `decode` plus libass (`subtitles`/`ass` burn-in), FreeType and HarfBuzz (`drawtext` captions), zlib, and VideoToolbox/AudioToolbox (`h264_videotoolbox`). SPDX `LGPL-2.1-or-later AND FTL AND MIT-Modern-Variant AND ISC`. macOS aarch64 is approved. macOS x86_64 is built and reproducible but stays `pending` until its probes run on an x86_64-capable host. See `third-party/BUILD-NOTES-ffmpeg-export-ass-macos.txt`. `tests/export_ass_e2e.rs` burns an `.ass` subtitle and a `drawtext` caption into a VideoToolbox MP4 and checks the decoded pixels, when the binary is present in `vendor/ffmpeg-lgpl/`.
- `export-ass-zimg`: `export-ass` plus libzimg, so FFmpeg's `zscale` filter exists next to `tonemap` (PQ/HLG linearise, bt2020 to bt709, dither: the HDR and Apple Log to Rec.709 path). It is a separate variant so the `export-ass` rows, digests and licence set keep their meaning; select it explicitly with `EXPORT_ASS_ZIMG_VARIANT`. SPDX `LGPL-2.1-or-later AND FTL AND MIT-Modern-Variant AND ISC AND WTFPL` (zimg 3.0.5, statically linked). `--enable-libzimg` needs no GPL, nonfree or version3 switch in FFmpeg 7.1.5. Both macOS rows are `pending`: aarch64 is built and was executed (its content digest awaits confirmation by the crate's `content_identity_file`, after which it is approved); x86_64 is built but never run (no x86_64-capable host); its zimg has no AVX-512 kernels because Apple clang 21 rejects the flags zimg's `configure.ac` probes for. See `third-party/BUILD-NOTES-ffmpeg-export-ass-zimg-macos.txt`. `tests/bundled_handle_e2e.rs` verifies the binary through `verify_bundled_ffmpeg` and renders an HDR-tagged clip to bt709 through `zscale` and `tonemap`, when the binary is present in `vendor/ffmpeg-lgpl/` and its row is approved (it reports SKIP while the row is pending).

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

`native` (default on, empty feature list): the `native` module, the suite-wide native-library catalog (`Cargo.toml:15-18`, `src/lib.rs:23-24`). FFmpeg-only consumers opt out with `default-features = false`.

## Platform support

- macOS aarch64: `decode` (ffmpeg, ffprobe) and `export-ass` (ffmpeg) approved; `export-ass-zimg` pending.
- macOS x86_64: `decode` (ffmpeg, ffprobe) approved; `export-ass` and `export-ass-zimg` pending.
- Windows x86_64: `decode` (ffmpeg) approved.
- Production `cfg(unix)` and `cfg(not(unix))` branches exist only in `src/staging.rs`; `cfg(windows)` and `cfg(unix)` also appear in test modules (`src/lib.rs:1632`, `src/probe.rs:827`, `src/unverified.rs:540`).

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

Licensed under either MIT or Apache-2.0, at your option. FFmpeg artifacts retain their own LGPL-2.1-or-later licence and attribution obligations (see `third-party/`). Statically linked components may carry only the licences in `ALLOWED_COMPONENT_LICENSES` (`src/lib.rs`): LGPL-2.0-or-later, LGPL-2.1-or-later, ISC, FTL, MIT, MIT-Modern-Variant, Zlib, BSD-2-Clause, BSD-3-Clause, and WTFPL. WTFPL was added for zimg (the `export-ass-zimg` variant): it is permissive, has no copyleft and no notice duty, and its notice is shipped anyway (`third-party/zimg-COPYING.txt`). Anything not on the list, notably GPL or AGPL, is refused.

## Changes

Release notes: [CHANGELOG.md](CHANGELOG.md). Version and status history is also in `INDEX.md`.

## Falling back to a development ffmpeg

Apps must not guess which error means "this binary is not in the catalog". Ask the error:
`MediaError::is_not_in_catalog()` is `true` for exactly `DigestMismatch` (the file's content
digest is not the approved row's) and `NoApprovedArtifact` (no approved row for this platform).
Only those two permit a development fallback (`DevFfmpeg`, `resolve_ffmpeg_or_unverified`), and
only in a development build. Everything else is `false` and must be shown, not worked around:
`ArtifactNotFound` and `OverrideMissing` mean the file is missing, not unapproved;
`ArtifactPending`, `AmbiguousArtifact`, `MalformedBinary`, probe failures and catalog errors are
defects a fallback would hide. A release build must not fall back silently; it reports the error
(`UnverifiedRefusedInRelease` is the crate's own refusal).
