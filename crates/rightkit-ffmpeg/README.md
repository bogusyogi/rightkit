# rightkit-ffmpeg

FFmpeg safety primitives for Right Suite desktop apps. The crate owns the catalog of approved FFmpeg and ffprobe builds, the checks that decide whether a binary may run, the argument builders, the process runner, and the loudness normalization loop. It contains no FFmpeg binary, no app policy, no model, and no shell invocation.

**Use it** when an app must run FFmpeg or ffprobe on user media and needs the binary to be a verified, catalog-approved build, or needs a documented development fallback.

**Do not use it** to decode or encode media in-process, to choose app-level media policy, or to call FFmpeg through a shell. Arguments are built as `OsString` values and run without shell interpolation.

## Capability

- **Catalog selection.** Select exactly one approved record per tool (`ffmpeg`, `ffprobe`), variant (`decode`, `decode-av1`, `decode-apple`, `export-ass`, `export-ass-zimg`), OS, and architecture (`aarch64` or `x86_64`). Records in `pending` state (recipe and spec recorded, digest not yet produced) are never resolvable.
- **ffprobe JSON.** Typed streams carry exact `r_frame_rate`, `avg_frame_rate` and `time_base` rationals; `PacketSample::pts_seconds(time_base)` converts a packet timestamp to seconds and returns `Option<f64>` (`src/probe.rs:824`).
- **Working directory.** `RunOptions::with_current_dir(dir)` (and `spawn_streaming_in`) runs the child in `dir` so relative filter files resolve; the directory is checked before spawn and the ffmpeg path is pinned absolute first.
- **Verification.** Validate catalog metadata and SHA-256 before probing or execution. Require the approved version banner (`ffmpeg version` / `ffprobe version`) and the mandatory `--disable-gpl --disable-nonfree --disable-autodetect` flags. Reject `--enable-gpl`, `--enable-nonfree`, and GPL encoder libraries, case-insensitively.
- **Licence record.** Each statically linked component is recorded with licence, source, digest, enable flag, and notice. The entry's SPDX expression is FFmpeg's LGPL `AND` each component's.
- **Resolution.** Resolve catalog binaries only from an explicit override, a bundled path, or an explicitly enabled development path. Non-catalog binaries come only from `unverified::resolve_unverified` (configured path, named environment variable, or opt-in `PATH`). These are labelled `UNVERIFIED`, expose encoder, decoder, and filter probing, and are refused in `BuildMode::Release`.
- **One handle.** `ToolHandle<V>` (`FfmpegHandle`, `FfprobeHandle`) is `Verified` or `Development`; `Development` exists only in a development build, for a not-in-catalog error on a file that exists (`handle::verified_or_development`). `resolve_ffmpeg_handle` and `resolve_ffprobe_handle` are the one-call resolver: the first bundled candidate beside the executable or in `src-tauri/binaries/<tool>-<triple>` is verified, and a development build alone falls back to its `UnverifiedRequest`. A release build never runs a bundled binary that has no catalog row. To let an app's own development override win over the bundled binary, pass `ResolveOptions::new().override_first(path)` to `resolve_ffmpeg_handle_with` (or `resolve_ffprobe_handle_with`); the app reads its own variable (for example `GENRIGHT_FFMPEG`) and supplies the path, which gets the same verification as a bundled file. An override that is missing or fails verification is listed in `HandleResolution::skipped` with its reason, and the normal order follows.
- **Encoders.** `encoders::EncoderInventory::probe` runs `-encoders` once and keeps the video encoders in listing order. `encoders::pick_h264` picks the H.264 encoder: the first listed hardware encoder for the platform that the caller's `usable` callback accepts, then `libopenh264` only when listed. `libx264` is never picked.
- **Arguments.** `command::extract_args` builds a clip, audio-only or stream-copy argument list (`-y [-ss] -i in [-t] [-vn | -c copy] extra out`); `decode::DecodeOptions` takes `max_duration` (`-t`) and `audio_stream_index` (`-map 0:a:N`).
- **Bundled handle.** `verify_bundled_ffmpeg(path, variant, timeout)` and `verify_bundled_ffprobe(path, variant, timeout)` take the path of a binary shipped in the app, the catalog variant the app expects, and a probe timeout. They select the one approved row for that tool, variant and this host, check the content digest, run the LGPL build probe, and return a `VerifiedFfmpeg` / `VerifiedFfprobe`. The handle is `Clone`, implements `FfmpegBinary`, and is what every run, probe, decode, loudness and staging call takes, so an app verifies once at start-up and passes the handle everywhere. Repeat calls for the same unchanged file hit the verify cache (no re-hash, no second `-version`). On Windows the cache has no file identity, so a file replaced with the same size and modification time is not detected; call `verify_cache_clear` after such a replacement (`src/verify_cache.rs:1-12`). The `_in` forms take an explicit catalog, OS and architecture.
- **Staging.** Stage a verified binary as a Tauri `externalBin` (`binaries/<tool>-<target-triple>[.exe]`, mode `0755`, re-hashed, idempotent) with `stage_external_bin`.
- **Process runner.** Run verified FFmpeg through `rightkit-child-process`, which owns the child tree and reaps descendants on exit, cancellation, timeout, failure, and drop.
- **Loudness.** Normalise loudness by rendering gain and a limiter, then re-measuring until the kept render is within ±0.5 LU (configurable, never looser). The loop is bounded to 5 renders by default (port of GenRight `transcode_normalized`).
- **Audio contract.** The shared audio contract is WAV, mono, 16 kHz, `pcm_s16le`.

The artifact catalog (`catalog/artifacts.json`) records binary and source digests, size where recorded, the official source URL, build provenance, the full LGPL notice, components, and the required configure flags. `recipes/build-ffmpeg-macos.sh` reproduces the macOS entries from the sources pinned in `recipes/ffmpeg-sources.json`. That file also records each source's upstream evidence.

## Variants

- `decode`: native codecs only (Windows x86_64; macOS aarch64 and x86_64; ffprobe on macOS).
- `decode-av1`: `decode` plus libdav1d (AV1 decode, BSD-2-Clause). ffmpeg on macOS aarch64 and x86_64, both approved (`src/lib.rs:74-76`).
- `decode-apple`: VideoToolbox and AudioToolbox only, no third-party code. ffprobe on macOS aarch64 and x86_64, both approved (`src/lib.rs:77-78`).
- `export-ass`: `decode` plus libass (`subtitles`/`ass` burn-in), FreeType and HarfBuzz (`drawtext` captions), zlib, and VideoToolbox/AudioToolbox (`h264_videotoolbox`). SPDX `LGPL-2.1-or-later AND FTL AND MIT-Modern-Variant AND ISC`. macOS aarch64 is approved. macOS x86_64 is built and reproducible but stays `pending` until its probes run on an x86_64-capable host. See `third-party/BUILD-NOTES-ffmpeg-export-ass-macos.txt`. `tests/export_ass_e2e.rs` burns an `.ass` subtitle and a `drawtext` caption into a VideoToolbox MP4 and checks the decoded pixels, when the binary is present in `vendor/ffmpeg-lgpl/`.
- `export-ass-zimg`: `export-ass` plus libzimg, so FFmpeg's `zscale` filter exists next to `tonemap` (PQ/HLG linearise, bt2020 to bt709, dither: the HDR and Apple Log to Rec.709 path). It is a separate variant so the `export-ass` rows, digests and licence set keep their meaning; select it explicitly with `EXPORT_ASS_ZIMG_VARIANT`. SPDX `LGPL-2.1-or-later AND FTL AND MIT-Modern-Variant AND ISC AND WTFPL` (zimg 3.0.5, statically linked). `--enable-libzimg` needs no GPL, nonfree or version3 switch in FFmpeg 7.1.5. The Windows x86_64 row is approved; the two macOS rows are `pending`. macOS aarch64 is built and was executed (its content digest awaits confirmation by the crate's `content_identity_file`, after which it is approved). macOS x86_64 is built, but its `-buildconf`, `-filters`, `-encoders` and `-L` checks were not run (no x86_64-capable host). Windows x86_64 is built by CI, and its contentSha256 is not yet confirmed by `content_identity_file` (`third-party/BUILD-NOTES-ffmpeg-export-ass-zimg-windows.txt`). The macOS x86_64 zimg has no AVX-512 kernels because Apple clang 21 rejects the flags zimg's `configure.ac` probes for (`third-party/BUILD-NOTES-ffmpeg-export-ass-zimg-macos.txt`). `tests/bundled_handle_e2e.rs` verifies the binary through `verify_bundled_ffmpeg` and renders an HDR-tagged clip to bt709 through `zscale` and `tonemap`, when the binary is present in `vendor/ffmpeg-lgpl/` and its row is approved (it reports SKIP while the row is pending).

## Example

Illustrative; not compiled by the crate. The types and functions named here exist in `src/lib.rs`. For compiled usage, see `tests/media_e2e.rs` and `tests/catalog_tools_e2e.rs`.

```rust,ignore
use rightkit_ffmpeg::{
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

`native` (default on, empty feature list): the `native` module, the suite-wide native-library catalog (`Cargo.toml:15-18`, `src/lib.rs:26-27`). FFmpeg-only consumers opt out with `default-features = false`.

`dev-overrides` (off by default, no dependencies): development and QA builds only. `BuildMode::from_build_features()` returns `Development` and `DevFfmpeg::try_unverified` accepts UNVERIFIED (non-catalog) binaries only with it. Never implied by a build profile (installed dev builds with debug assertions behave like release) and never a default feature; an app enables it only from its QA/test configuration. `BuildMode::from_debug_assertions()` is a deprecated alias of `from_build_features()`.

## Platform support

- macOS aarch64: `decode` (ffmpeg, ffprobe), `decode-av1` (ffmpeg), `decode-apple` (ffprobe), `export-ass` (ffmpeg) and `export-ass-zimg` (ffmpeg) approved.
- macOS x86_64: `decode` (ffmpeg, ffprobe), `decode-av1` (ffmpeg) and `decode-apple` (ffprobe) approved; `export-ass` and `export-ass-zimg` pending.
- Windows x86_64: `decode` (ffmpeg, ffprobe) and `export-ass-zimg` (ffmpeg) approved. No `export-ass` row: Windows exports use `export-ass-zimg`. The Windows builds are LGPL and carry no libx264: H.264 encoding there is Media Foundation (`h264_mf`), or NVENC when the build lists it and the caller's check accepts it. Always state a rate or a quality for it: with none it encodes 1080p at about 88 kbit/s (measured by CutRight on the approved 7.1.5 binary, 2026-10-12; not a test in this crate). `encoders::pick_h264` chooses the encoder from the listing; the rate or quality is the consumer's.
- Production `cfg(unix)` and `cfg(not(unix))` branches are in `src/staging.rs` (lines 117, 144, 154) and `src/verify_cache.rs` (lines 92, 98). `cfg(windows)` and `cfg(unix)` also appear in test modules (`src/lib.rs:2186`, `src/lib.rs:2211`, `src/probe.rs:1196`, `src/unverified.rs:540`).

Availability is defined by `catalog/artifacts.json`, not by this README. Check the `status` field of each record.

## Verification

Run through the RightKit wrapper, per the workspace's `docs/rules/rightkit.md`:

```text
rightkit cargo test -p rightkit-ffmpeg --all-targets
rightkit cargo fmt --check
rightkit cargo clippy -p rightkit-ffmpeg --all-targets -- -D warnings
rightkit cargo package -p rightkit-ffmpeg --allow-dirty --no-verify --list
```

## Publication

Published version: see `INDEX.md` at the repository root. This README does not state release status. Publish through `right-release publish cargo` only after the release gate is satisfied (see the workspace's `docs/rules/release-signing.md`).

## Licence

Licensed under either MIT or Apache-2.0, at your option. FFmpeg artifacts retain their own LGPL-2.1-or-later licence and attribution obligations (see `third-party/`). Statically linked components may carry only the licences in `ALLOWED_COMPONENT_LICENSES` (`src/lib.rs`): LGPL-2.0-or-later, LGPL-2.1-or-later, ISC, FTL, MIT, MIT-Modern-Variant, Zlib, BSD-2-Clause, BSD-3-Clause, and WTFPL. WTFPL was added for zimg (the `export-ass-zimg` variant): it is permissive, has no copyleft and no notice duty, and its notice is shipped anyway (`third-party/zimg-COPYING.txt`). Two further entries exist only for the toolchain runtime a static MinGW-w64 link carries into Windows binaries (`TOOLCHAIN_RUNTIME_COMPONENTS`): `LicenseRef-MinGW-w64-runtime` for the mingw-w64 C runtime and `GPL-3.0-or-later WITH GCC-exception-3.1` for libgcc and libstdc++ (the GCC Runtime Library Exception permits linking them without the GPL reaching the binary); winpthreads is `MIT AND BSD-3-Clause`. No other component may carry those two, and their notices ship in `third-party/`. Anything not on the list, notably GPL or AGPL, is refused.

## Changes

Release notes: [CHANGELOG.md](CHANGELOG.md). Version and status history is also in `INDEX.md`.

## Encoder selection policies (0.2.10)

`encoders::EncoderInventory::probe(&binary, timeout)` is the simple uncached listing. For
apps that need cancellation and process-lifetime reuse, `probe_cached(&binary, timeout,
&cancel)` lists video encoders through the owned child runner and caches successful listings
per immutable binary path; use `probe_uncached` for mutable paths or `encoder_cache_clear`
after replacing tools. `parse` and `from_names` support deterministic selection tests.

- `select_delivery(&inventory, os, &mut hardware_ok)` preserves CutRight policy: macOS
  VideoToolbox; Windows usable NVENC, then listed Media Foundation; otherwise
  `EncoderError::Unavailable`. Only NVENC invokes qualification here.
- `select_intermediate(&inventory, os, format, &mut hardware_ok)` keeps existing
  quality/profile arguments: H.264 for 8-bit, HEVC Main 10 for 10-bit 4:2:0, ProRes HQ
  fallback. 10-bit 4:2:2 always requires software `prores_ks`. `IntermediateFormat::pix_fmt`
  supplies the separate pixel-format argument. `intermediate_for` probes for the current host.
- `qualify_encoder(&binary, name, timeout, &cancel)` runs a short black-frame encode;
  `hardware_usable` supplies a boolean callback adapter. Use the typed qualification result
  when cancellation, timeout or failure details must propagate.
- `select_qualified_delivery(&inventory, os, allow_software, &mut encoder_ok)` tests every
  listed platform candidate, then optional `libopenh264` software fallback. It never
  implicitly selects GPL `libx264`; catalog variants need not ship OpenH264.

## Falling back to a development ffmpeg

Apps must not guess which error means "this binary is not in the catalog". Ask the error:
`MediaError::is_not_in_catalog()` is `true` for exactly `DigestMismatch` (the file's content
digest is not the approved row's) and `NoApprovedArtifact` (no approved row for this platform).
Only those two permit a development fallback (`DevFfmpeg`, `resolve_ffmpeg_or_unverified`,
`handle::verified_or_development`), and only in a development build. Everything else is `false` and must be shown, not worked around:
`ArtifactNotFound` and `OverrideMissing` mean the file is missing, not unapproved;
`ArtifactPending`, `AmbiguousArtifact`, `MalformedBinary`, probe failures and catalog errors are
defects a fallback would hide. A release build must not fall back silently; it reports the error
(`UnverifiedRefusedInRelease` is the crate's own refusal).

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead.

- `tests/media_e2e.rs` (probe, transcode, cancel and loudness tests) and `tests/stdin_probe_e2e.rs`: a development FFmpeg, enabled by `RIGHTKIT_FFMPEG=<path>` or one of the candidate paths listed by `rightkit_ffmpeg::dev_resolve::development_candidates`. `stdin_probe_e2e.rs` also needs an `ffprobe` beside that binary.
- `tests/media_e2e.rs`, `aac_m4a_export_normalizes_through_the_built_in_encoder`: an FFmpeg on `PATH` with the built-in `aac` encoder.
- `tests/catalog_tools_e2e.rs` and `tests/export_ass_e2e.rs`: approved FFmpeg and ffprobe artifacts in `vendor/ffmpeg-lgpl` at the workspace root (found by walking up from the crate). Enabled by placing the artifacts there. `catalog_tools_e2e.rs` also needs an FFmpeg on `PATH`.
- `tests/bundled_handle_e2e.rs`, `the_export_ass_zimg_binary_verifies_in_one_call_and_converts_hdr_to_sdr` (macOS aarch64): `ffmpeg-export-ass-zimg-macos-arm64` and `ffprobe-decode-macos-arm64` in `vendor/ffmpeg-lgpl`.
- `tests/content_identity_e2e.rs`: retained build outputs under `/Volumes/D/rightkit-publish/artifacts` and the vendor artifacts above.
- `tests/native_catalog_e2e.rs`, `whisper_and_ggml_dylibs_are_catalogued_by_file_and_verify_by_content`: retained whisper.cpp and ggml dylibs under `/Volumes/D/rightkit-publish/artifacts/whisper-cpp-1.9.3/macos-arm64`.
- `tests/native_catalog_e2e.rs`, `real_local_artifacts_and_locks_match_their_catalog_records` (ignored; run with `--ignored`): binaries and lock files in sibling app checkouts under the workspace root.
- `src/decode.rs`, `s16_decode_round_trips_pcm_when_an_ffmpeg_is_on_the_path` (unit test): an FFmpeg on `PATH`.
- `src/lib.rs`, `approved_windows_binary_resolves_and_passes_the_real_build_probe` (unit test, Windows): `vendor/ffmpeg-lgpl/ffmpeg-win-x64.exe` in the workspace.
