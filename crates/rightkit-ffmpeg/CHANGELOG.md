# Changelog

## 0.2.10 (2026-10-18) — unpublished staging

- BREAKING (source; plan 2.8, gap C5-02): `DevFfmpeg::allow_in_release_builds` is removed in every build, and `DevFfmpeg::unverified` (no mode check) is compiled only in this crate's own test builds and with the `dev-overrides` feature. This closes those two named constructors and nothing else: it is not a proof that a release build can only run catalog-verified binaries. Still open: `dev_resolve::resolve_for_development` is a public function with no mode check that returns a `DevFfmpeg` for the first existing candidate (including `RIGHTKIT_FFMPEG`) when none verifies, `FfmpegBinary` is a public unsealed trait any type can implement, and `BuildMode::Development` is a public variant that `handle::verified_or_development_in` and `unverified::resolve_unverified` honor in any build. Gating `resolve_for_development` is a follow-up (its callers in `rightkit-audio`, `rightkit-motion` and this crate's e2e tests would silently SKIP, so it is not changed here). `DevFfmpeg::try_unverified` (refused without `dev-overrides`), `unverified::resolve_unverified`, `UnverifiedRequest` and `dev_resolve::resolve_for_development` are unchanged. The crate's own fallbacks (`verified_or_development`, the resolvers) check the mode and then build their handle through a crate-private constructor. Migration: HeardRight is the only caller of `allow_in_release_builds` in the nine apps; it moves to `resolve_ffmpeg_handle_with` with `DECODE_VARIANT` (gap C5-01), so a bundled binary outside the catalog is refused in a release build. A test in another crate that names `DevFfmpeg::unverified` enables `rightkit-ffmpeg/dev-overrides` from its `[dev-dependencies]` or implements `FfmpegBinary` on a local type. This crate's integration tests (`stdin_probe_e2e`, `encoder_selection_e2e`, `hidden_console_windows`, through `tests/common`) and `rightkit-motion`'s `render` tests do the latter, so no test needs the feature and `cargo test -p rightkit-ffmpeg` runs them all. New doctests on `DevFfmpeg` prove the absence in a plain `cargo test`: `compile_fail` snippets name `unverified` (only without the feature) and `allow_in_release_builds` (always), beside a control snippet that compiles. Stable rustdoc does not check the error code in `compile_fail,E0599`; it records the expected error.
- Version decision (release owner): removing `allow_in_release_builds` and gating `DevFfmpeg::unverified` break any app pinned to `^0.2`, because a `^0.2` requirement resolves to this version. HeardRight calls `allow_in_release_builds` today (gap C5-01), so it stops compiling on this version until it moves to `resolve_ffmpeg_handle_with`. Whether this ships as 0.2.10 or as 0.3.0 is the release owner's call; this change bumps nothing.
- Catalog data (`catalog/native-libraries.json`, `third-party/`): the PDFium Windows row goes from `planned` to `pinned` with archive and content digests; the macOS PDFium rows carry the archive sha256 and `archive.member`; new notice file `third-party/PDFium-chromium-8021-ThirdPartyNotices.txt`; new `planned` rows for Windows whisper-cpp, ggml (`base`, `cpu`, `vulkan`, `core`) and sherpa-onnx (a `planned` row never serves a library and has no digest to check); `hostProof` `never-run` marks the rows whose bytes nobody ran on a host of their OS. The two macOS PDFium rows are pinned with no `hostProof` because ScrapeRight ships these archives in production but this repository has not loaded them, and a re-signed copy cannot verify at load until a digest that survives signing exists (the content-digest parser refuses these dylibs).
- New (plan 2.8, gap C5-08): `native::verified_library(library, path)` verifies a native library file against the embedded catalog for this host immediately before the caller loads it, and returns `VerifiedLibrary` (`id`, `library`, `version`, canonical `path`, `status`, `host_proof`). `verified_library_in(catalog, os, arch, library, path)` takes a catalog and platform, and `verified_libraries(dir, entries)` / `verified_libraries_in` verify a set all or nothing. The path must be absolute without `..`, exist, not be a symbolic link and be a regular file (new `NativeCatalogError::UnsafePath { path, reason }`; the enum is non-exhaustive); the row is chosen by library, OS, architecture and file name among `approved` and `pinned` rows (new `NativeStatus::serves`, new `NativeCatalog::serving_file`; exact architecture beats `universal` or `any`; none is `NoApproved`, several `Ambiguous`; `planned` and `stale-unverified` never serve). The digest is checked at the call, not while the loader maps the file: the verify-then-load window `rightkit_onnx::configure_runtime_verified` documents applies here too. Re-exported at the crate root with `HostProof`.
- New (plan 2.8): native catalog row fields, all optional and understood together with `deny_unknown_fields`. `archive.member` (path of the shipped file inside the archive; non-empty, relative, `/`-separated, no `..`, `.` or empty component). `contentSha256ByArch` (`aarch64` and `x86_64` digests for a `universal` row; both are required, the field is for universal rows only and never sits beside `contentSha256`); `NativeRecord::verify_file` checks every listed slice against the file's slice and treats a missing slice as a mismatch, and every other row verifies as before. `companions` (ids of rows to stage beside this one; `NativeCatalog::validate` checks that each exists, has the same `os`, serves a common architecture, is not `stale-unverified` and is not the row itself). `hostProof` (`HostProof::NeverRun`, `never-run`: the bytes were never run on a host of the row's OS; legal only on a row that is not `approved`; it travels into `VerifiedLibrary::host_proof`). `ArchiveRef` and `NativeRecord` are public structs without `#[non_exhaustive]`, so a struct literal of either must name the new fields (`member`, `content_sha256_by_arch`, `companions`, `host_proof`); nothing in this repository builds one outside this crate. A consumer that decides "this row has a content digest" from `content_sha256.is_some()` alone (rightkit-onnx `provision`) must also test `content_sha256_by_arch`.
- New (plan 2.8, gap C5-07, Rust half): `ResolveOptions::bundle_dirs([...])` (ordered relative sub-directories of the executable's directory, searched after the file beside the executable and before the `src-tauri/binaries` layout; on macOS an executable in `Contents/MacOS` also tries `Contents/Resources/<dir>` right after each; empty, absolute or `..` entries are skipped), `ResolveOptions::runtime_layout()` (`runtime`, then `resources/runtime`) and `ResolveOptions::variants([...])` (ordered variant slugs; a non-empty list replaces the resolver's `variant` argument). `bundled_tool_candidates_with_dirs(tool, exe, os, arch, bundle_dirs)` lists the candidates; `bundled_tool_candidates` keeps its signature and behaviour. `verify_first_of(path, variants, timeout, verify)` verifies the one candidate against each variant in order and returns the first success; it moves to the next variant when the error `is_not_in_catalog()` or is `MediaError::ArtifactPending` (a variant whose only row for this host is pending is not usable, so `[export-ass-zimg, decode]` on an Intel Mac uses `decode` instead of failing), returns any other error (a probe failure, an ambiguous row) at once and returns the first variant's error when every variant failed that way; the development fallback still applies only when that final error is not-in-catalog, so an all-pending list gets none. `resolve_ffmpeg_handle_with` and `resolve_ffprobe_handle_with` use both; the development fallback applies only after every variant failed not-in-catalog, only in `BuildMode::Development`. Default options behave exactly as before.
- New: `NativeRecord::serves_host(os, arch)`, the public form of the catalog's platform rule (`os` is `any` or equals the host's, ASCII case-insensitive; `arch` is `any`, equals the host's, or is `universal` for `aarch64` or `x86_64`; status is not considered). `rightkit-asr` used a private copy of that rule and now calls this.
- Catalog validation (`NativeRecord::validate`, `NativeCatalog::validate`) now also refuses a `universal` row that carries `contentSha256` (`NativeRecord::verify_file` answers `NotVerifiable` for it, so it could never verify at load; `contentSha256ByArch` is the universal form) and a companion row that itself lists `companions` (the JS staging already refuses nested companions; the two now agree). No row in the embedded catalog is affected. Source-visible only to code that builds such rows.
- Docs: `verified_library` documents that `VerifiedLibrary::path` is a verbatim (`\\?\`-prefixed) path on Windows and that loading it there is a Windows host-proof item: this crate has not loaded such a path on a Windows host and does not strip the prefix.
- Tests: `native` unit tests (fixture rows in a temp dir; universal rows built from the Mach-O fixtures of `content_digest`), `handle` unit tests with injected verify closures, and `tests/native_catalog_e2e.rs` (serving rows name existing `third-party/` notices; the Windows rows nobody ran carry `hostProof` `never-run` and none is `approved`). `an_exact_arch_record_is_preferred_over_a_universal_one` drops `contentSha256ByArch` from its thin copy of the universal row. Built and run on Linux (default features and `--features dev-overrides`); the macOS and Windows hosts were not available, so the Windows-only test and real dylib and DLL loads are unrun.

- Native catalog, ONNX Runtime on macOS (bogusyogi/rightkit#889; not compiled or run locally, CI is the compiler; the JSON row was checked by loading it with Python only):
  - New approved row `onnxruntime-1.30.0-macos-universal2` (`os` `macos`, `arch` `universal`): the `csukuangfj/onnxruntime-libs` v1.30.0 asset `onnxruntime-osx-universal2-1.30.0.zip` (19,291,730 bytes, archive sha256 `826f3a5f157b5cba769249b2c7f2215bb430c4fc8d0b31e29c9c573becad10b2`, measured 2026-10-22 and equal to the digest GitHub publishes), shipped file `libonnxruntime.dylib` (66,116,720 bytes, sha256 `4b2fb26f1d4f78ef8492e2f5dadfe77d810e6cbe1364465ff33c44d189e93110`, `lipo -archs` x86_64 arm64, ad-hoc signed). 1.30.0 carries the KleidiAI SME2 Conv cache leak fix (microsoft/onnxruntime#28363, in 1.27.0). The evidence string records what is and is not verified: the leak fix was verified by HeardRight on 1.28.3 against the KWS corpus (heardright#292); a 1.30.0 corpus run is still pending in HeardRight before app adoption, and this library has not been loaded or run in this repository.
  - New notice file `third-party/ONNXRuntime-1.30.0-ThirdPartyNotices.txt` (the asset's `ThirdPartyNotices.txt`, sha256 `143764b9...c83de3`, differs from the 1.24.4 one); the asset's `LICENSE` is byte-identical to `third-party/ONNXRuntime-MIT.txt`, which stays listed.
  - The two macOS 1.24.4 rows (`onnxruntime-1.24.4-macos-arm64`, `onnxruntime-1.24.4-macos-x86_64`) are now `stale-unverified`: kept as history, never resolved, and outside the one-version rule. The Windows 1.24.4 DirectML and DirectML rows are untouched.
  - The one-version rule in `NativeCatalog::validate()` is now per (library, OS), not suite-wide: ONNX Runtime diverges by OS (macOS 1.30.0, Windows 1.24.4) until RightKit CI builds its own universal2 ORT (#889). `NativeCatalogError::MixedVersions` retains its existing fields for patch compatibility; the enum-level `#[non_exhaustive]` does not make existing variant fields extensible. New `NativeCatalog::suite_version_for(library, os)`; `suite_version(library)` keeps returning the first non-stale record, which for ONNX Runtime is now the macOS 1.30.0 one.
  - `NativeCatalog::approved()` and `approved_file()` prefer an exact-architecture approved record over a `universal` or `any` one; two exact records for the same architecture stay `Ambiguous`. With the 1.24.4 rows stale there is exactly one approved macOS ONNX Runtime record per architecture (`aarch64` and `x86_64` both resolve to the universal2 row).
  - Consumers that selected the row by exact `arch` (for example `packages/release/stage-sidecar.mjs`, which matches `row.arch === target arch`) do not see a `universal` row; rightkit-onnx `provision` does (0.2.7).

- Tests only (`runner.rs`): the straggler deadline test no longer races its own start-up (the 400 ms deadline could fire before the fake leader had written the straggler's pid file under host load; now 4 s against a 30 s straggler), and the overhead test warms up, uses medians, best of five rounds, and a bound of floor plus 3x the blocking baseline capped by a hard ceiling. No production change.

- Fix (test only): `tests/catalog_tools_e2e.rs` `vendor_file` returned the macOS x86_64 ffprobe for a Windows host, so
  the Windows catalog row `ffprobe-7.1.5-windows-x64` was verified against a Mach-O and (correctly) failed with
  `DigestMismatch`. It now returns the Windows ffprobe under `windows-x64/ffprobe-run/`. The content digest is
  unchanged and strict. New `content_digest` unit test pins a fixture Mach-O digest (computed independently)
  to prove the digest is host-independent. Written without compiling or running anything.
- New (issue #479): "override first" for the one-call resolver. `ResolveOptions::new().override_first(path)`
  makes `resolve_ffmpeg_handle_with` and `resolve_ffprobe_handle_with` verify the caller-supplied path
  before any bundled candidate, with the same checks (catalog digest and build probe, development
  fallback only in a development build). An override that is missing or fails is skipped and recorded
  in `HandleResolution::skipped` (`SkippedCandidate { path, reason }`), then the normal order applies.
  The crate reads no environment variable for this: the app reads its own (for example
  `GENRIGHT_FFMPEG`) and passes the path. `resolve_ffmpeg_handle` and `resolve_ffprobe_handle` are
  unchanged (bundled first). Written without compiling or running anything.
- New (issue #353): encoder selection policies in `encoders`. `EncoderInventory::from_names`,
  `probe_cached(binary, timeout, cancel)` (per-path process cache, failures never cached, cancelled
  token refused even on a hit) and `probe_uncached`; `encoder_cache_clear`; `EncoderError`
  (`Unavailable`, `Probe(RunError)`, stable `code()`); `Encoder`/`codec_name`; `select_delivery`
  (platform-only: VideoToolbox, NVENC then Media Foundation), `require_listed`,
  `select_qualified_delivery` (qualifies every listed candidate, optional `libopenh264`),
  `qualify_encoder` and `hardware_usable` (short black-frame encode), and
  `IntermediateFormat`/`IntermediateEncode`/`select_intermediate`/`intermediate_for`
  (H.264, HEVC Main 10 or ProRes HQ by bit depth and chroma). Additive: `EncoderInventory::probe`,
  `parse`, `pick_h264` and the 0.2.9 items are unchanged. New `tests/encoder_selection_e2e.rs`
  (unix, scripted child processes). Written without compiling or running anything.

## 0.2.9 (2026-10-17)

- `DecodeOptions` gained public `max_duration` and `audio_stream_index` (both `Option`, default `None`) plus `Default` and builder methods. Literals using `..Default::default()` keep compiling; exhaustive literals must name the two fields (source-breaking, deliberate).

- Breaking (CutRight G5, issue #457; crate version not bumped here): `decode::DecodeOptions` gains
  `max_duration: Option<Duration>` (output `-t`) and `audio_stream_index: Option<u32>` (strict
  `-map 0:a:N`, a zero-based ordinal in input 0). Both PCM decoders share them. Duration uses
  microsecond precision; a value below one microsecond fails before spawn. `timeout` stays an
  independent wall-clock deadline. Existing struct literals must add both fields as `None` to keep
  unlimited output and automatic stream selection. Written without compiling or running anything.
- New (issues #353, #479): `encoders` module. `EncoderInventory::probe(binary, timeout)` runs
  `-encoders` once and keeps the video encoders in listing order (`names()`, `has()`);
  `video_encoder_names(listing)` does the same parse on text. `pick_h264(inventory, os, usable)`
  returns the first listed hardware H.264 encoder for `os` that `usable` accepts (`h264_videotoolbox`
  on macOS; `h264_nvenc` then `h264_mf` on Windows; `h264_vaapi` on Linux), else the first listed
  `libopenh264`, else `None`. `libx264` is never a candidate (GPL; forbidden by every catalog row).
  MPEG-4 fallback stays the caller's decision. Written without compiling or running anything.
- New (issues #354, #479): `handle` module. `ToolHandle<V>` is `Verified(V)` or `Development(DevFfmpeg)`,
  implements `FfmpegBinary` and `is_verified()`; `FfmpegHandle` and `FfprobeHandle` are its aliases.
  `verify_ffmpeg_or_development` and `verify_ffprobe_or_development` take a bundled path and variant and
  return the handle. The development handle is produced only in a development build (`dev-overrides`),
  for a file on disk, and only when the error is `is_not_in_catalog()`; a release build returns the
  catalog error unchanged. `resolve_ffmpeg_handle` and `resolve_ffprobe_handle` are the one-call
  resolver: the first existing bundled candidate (`bundled_tool_candidates`: beside the executable,
  then `src-tauri/binaries/<tool>-<triple>` up the executable's ancestors) is verified; with no bundled
  file, a development build resolves its `UnverifiedRequest` (override, environment variable, PATH).
  Not done: a release build that runs a bundled binary with no catalog row by path (see README).
  Written without compiling or running anything.
- New (issue #581): `command::extract_args(input, output, &ExtractOptions)` builds
  `-y [-ss S] -i input [-t D] [-vn | -c copy] extra... output` with `ExtractOptions { start, duration,
  mode: Option<ExtractMode::{Copy, AudioOnly}>, extra }`; times are `Duration` with microsecond
  precision. It builds arguments only and runs nothing. Not done: the PATH resolution that returns an
  unverified ffmpeg in a release build (`resolve_ffmpeg_or_path`); the development form is
  `unverified::resolve_ffmpeg_or_unverified`. Written without compiling or running anything.

- New (crate version 0.2.9; Orthic-Labs/rightkit#300, PostRight RG-3): `quickprobe::Probe` now carries `size_bytes` (always set), `codec` (png, jpeg, gif, webp, WAV PCM variants, flac, mp3, and for MP4/MOV the first video track's sample entry as h264, hevc, av1, vp9, prores and so on, else the audio track's), `audio_codec` (MP4/MOV with a video track) and `frame_count` (GIF image frames). Confirmed unchanged: duration and dimensions. No "slide count": it is not a container property, so GIF `frame_count` is the nearest and deck formats (PDF, PPTX) are not probed here (rightkit-pdf has page counts). `Probe` gained public fields, so code that builds it with a struct literal needs `..Default::default()`. Written without compiling or running anything; CI builds it.

- New (crate version 0.2.8; Orthic-Labs/rightkit#271): process priority for ffmpeg. `RunOptions::with_priority(ProcessPriority)` (used by `run_with_progress`, `run_with_stdin`, `run_capture_with` and the checked variants) and `spawn_streaming_with_priority(.., priority)`; `ProcessPriority` is re-exported. `Normal` (the default) changes nothing; `BelowNormal`, `Idle` and the new `Efficient` (EcoQoS on Windows, utility QoS on macOS) come from rightkit-child-process 0.4.3, now the minimum. Written without compiling or running anything; CI builds it.

- Security (behaviour change, crate version 0.2.7; Orthic-Labs/rightkit#109): allowing UNVERIFIED binaries no longer follows `cfg(debug_assertions)` (on in the dev profile of installed dev builds). It is now behind the explicit, non-default `dev-overrides` feature. New `BuildMode::from_build_features()`; `BuildMode::from_debug_assertions()` is kept as a deprecated alias that follows the feature, so it no longer returns `Development` in a plain dev build. `DevFfmpeg::try_unverified` refuses with `UnverifiedRefusedInRelease` unless the feature is on. App migration: enable `rightkit-ffmpeg/dev-overrides` only from a QA/test feature to keep the old debug-build behaviour. Not built or run locally.

Renamed from rightkit-media; 0.2.5 remains published under the old name.

- Catalogue: `ffmpeg-7.1.5-export-ass-zimg-macos-arm64` is approved (sha256 `52f81f26277dce071cecae6c00555aac4b7d132a17a7e5016b7423942e78594d`, 16,346,792 bytes, content digest `4fa975dac6830e5b3346b2056323912fb4cdc5b5062e4577658bdb8f83e8ca92`, slice aarch64). The row's condition was that the crate's own `content_identity_file` confirm the digest; CutRight ran `sha256_file` and `content_identity_file` from this crate (snapshot c9f222a1) on the vendored binary on 2026-10-12 and both matched, with `-buildconf` showing `--disable-gpl --disable-nonfree --enable-libzimg` and `-filters` listing `zscale` and `tonemap`. Evidence is a consumer's run on the build host, not a CI run of this repository. The x86_64 row stays pending: its probes need an x86_64-capable macOS host.
- Catalog: the two Windows x86_64 rows are approved. `ffmpeg-7.1.5-export-ass-zimg-windows-x64`: sha256
  `2ecd3ab3d313f1a09ad29491979f806faf98c38d150ab37ec2978627fcb9cdb9`, contentSha256
  `34acae414d4397d37e8125fb4366e22426ad53aa06a15d6b8007efa7964afdcb`, 19,496,448 bytes.
  `ffprobe-7.1.5-windows-x64`: sha256 `4c0ac1359dfaffb28b54a5f6e78e78185271516d50e23ee73807721b14188097`,
  contentSha256 `a1cfb297e05d2ab426a2fb5d9d4fbc817834e1b2dba62f596646564ad738712a`, 16,198,656 bytes. Evidence, both
  from `tests/promote_evidence_windows.rs` on the binary the `ffmpeg-windows` workflow built on windows-2025: run
  38173732506 (ffprobe: digests and size equal to the row, content digest computed by `content_identity_file`, a WAV
  read and its stream facts asserted) and run 38173734903 (ffmpeg: the same identity check; `h264_mf`, `subtitles`,
  `drawtext` and `zscale` + `tonemap` each encoded, decoded back as H.264 and counted at 25 frames). The ffmpeg binary
  was built four times and ffprobe three times with identical digests. Limits: `h264_mf` on the hosted runner is the
  software encoder; zlib's signature was not checked (its digest matched the publisher's); the smokes show the filters
  run and change the picture, not that colours are right. Supplementary, on a Dell (RTX 4070 Laptop GPU): digests
  verified before running, then the four smokes through the hardware path ("NVIDIA H.264 Encoder MFT"), 25 frames
  each; the software path matched CI; the CI ffprobe read those MP4/H.264 files. Not covered: Intel Quick Sync,
  hevc_mf, aac_mf, long clips, real camera footage, colour accuracy. Details in the Windows build notes.
- Tests: `tests/promote_evidence_windows.rs` produces the evidence a pending Windows catalogue row needs before it
  is approved, on the binary a build just made. With `RIGHTKIT_MEDIA_PROMOTE_FFPROBE` or
  `RIGHTKIT_MEDIA_PROMOTE_FFMPEG` set to the built executable it prints the SHA-256, size and
  `content_identity_file` digest, checks them against the row, has ffprobe read a committed 1,644-byte WAV
  (`tests/fixtures/tone-8k-mono-s16.wav`), and runs the four export smokes on ffmpeg (`h264_mf`, `subtitles`,
  `drawtext`, `zscale` + `tonemap`), each decoded back and checked. Unset, the tests print why and pass. Not run
  yet; on a hosted runner `h264_mf` is the software encoder.
- Runner: a run that completes on its own now ends the process tree before it reaps and before it
  joins the stdin writer (`wait_or_kill` on the unreaped leader), not a bare `wait`. A descendant that
  redirected its output and kept running no longer outlives `run_with_progress`, `run_with_stdin`,
  `run_capture` or `run_checked`, and one that kept stdin open can no longer hold the writer join
  past the deadline. Needs the `rightkit-process` fix that makes `wait_or_kill` terminate the
  retained job on Windows when the leader exits within the grace.
- Runner: the stderr tail of `run_with_progress`/`run_with_stdin` uses `rightkit_process::StderrTail`.
  A child that writes stderr without a newline no longer grows memory (a line over four times the
  8 KiB cap is dropped unbuffered and replaced by a marker), and the retained tail never exceeds
  8 KiB, including a single long line. The 40-line limit is gone; the byte cap is the bound. The
  stdout progress reader likewise drops lines over 64 KiB instead of buffering them.
- Native catalog: `onnxruntime-1.24.4-windows-x64-directml` moves from `planned` to `pinned`:
  `onnxruntime.dll` from the NuGet package `Microsoft.ML.OnnxRuntime.DirectML` 1.24.4, with the
  package and file digests and the package's own `ThirdPartyNotices.txt`. Not resolvable as
  approved until rightkit-ort has loaded it.
- Native catalog: new library kind `directml` and the pinned record `directml-1.15.4-windows-x64`
  (`DirectML.dll` from NuGet `Microsoft.AI.DirectML` 1.15.4, with its licence and third-party
  notices verbatim). `DIRECTML_LICENSE` (`LicenseRef-Microsoft-DirectML`, Microsoft's licence
  terms, not open source) is admitted for that library on Windows only; any other library or OS
  carrying it is refused.
- Windows x86_64 catalog rows, both `pending`: `ffmpeg-7.1.5-export-ass-zimg-windows-x64` (libass
  through DirectWrite, FreeType, HarfBuzz, FriBidi, zimg, zlib, Media Foundation `h264_mf`) and
  `ffprobe-7.1.5-windows-x64` (`decode`). Recipe: `recipes/build-ffmpeg-windows.sh` (MSYS2
  MINGW64); notes: `third-party/BUILD-NOTES-ffmpeg-export-ass-zimg-windows.txt`. Built once
  locally; digests are recorded after the `ffmpeg-windows` CI workflow has built them twice.
- `TOOLCHAIN_RUNTIME_COMPONENTS` (`mingw-w64-runtime`, `winpthreads`, `gcc-runtime`): the runtime
  code a static MinGW-w64 link carries into a Windows binary. Any variant may list them, `decode`
  included, each only under its own licence. `ALLOWED_COMPONENT_LICENSES` gains
  `LicenseRef-MinGW-w64-runtime` (permissive notice licence without an SPDX identifier) and
  `GPL-3.0-or-later WITH GCC-exception-3.1` (libgcc and libstdc++; the exception permits linking
  without the GPL reaching the binary); only those components may carry them, and every other
  GPL identifier is still refused. A component licence may now be a conjunction
  (`MIT AND BSD-3-Clause` for winpthreads): every term must be allowed and named in the entry's
  expression. The two new Windows rows list all three with their notice files.
- The approved `ffmpeg-7.1.5-windows-x64` decode row was built the same way and shipped without
  the mingw-w64 runtime, winpthreads and GCC runtime notices. Its `noticeFiles` now list the three
  files and its `components` name the three toolchain runtime components, so its licence expression
  is now `LGPL-2.1-or-later AND LicenseRef-MinGW-w64-runtime AND MIT AND BSD-3-Clause AND
  GPL-3.0-or-later WITH GCC-exception-3.1`; id and digests are unchanged. The native catalog
  (`native::NativeRecord::validate`) accepts FFmpeg's licence followed by toolchain runtime terms
  only. Apps that ship this binary must ship the three notices with it; a caller comparing the
  row's licence string with `LGPL-2.1-or-later` must compare the first term instead.
- `bundled::verify_bundled_ffmpeg(path, variant, timeout)` / `verify_bundled_ffprobe(...)`: one
  call from a bundled binary path plus the expected variant to a `VerifiedFfmpeg` /
  `VerifiedFfprobe` (embedded catalog, this host's OS and architecture, content digest, then the
  LGPL build probe), through the existing verify cache. `verify_bundled_ffmpeg_in` /
  `verify_bundled_ffprobe_in` take an explicit catalog, OS and architecture. Additive.
- `EXPORT_ASS_ZIMG_VARIANT` (`export-ass-zimg`): `export-ass` plus libzimg for the `zscale`
  filter (HDR and Apple Log to Rec.709 with `tonemap`). New variant, so `export-ass` rows keep their
  digests and licence set. Both macOS rows are `pending`: aarch64 is built and executed, awaiting
  confirmation of its content digest by `content_identity_file`; x86_64 is built, never run.
  Recipe: `build-ffmpeg-macos.sh --variant export-ass-zimg`; notes:
  `third-party/BUILD-NOTES-ffmpeg-export-ass-zimg-macos.txt`.
- `ALLOWED_COMPONENT_LICENSES` gains `WTFPL`, for zimg 3.0.5 (statically linked, `--enable-libzimg`).
  Reason: permissive, no copyleft, no notice duty; the licence notice ships anyway. Any other
  unlisted licence is still refused (`UnapprovedLicense`).
- `probe::probe_media_opt_with(ffmpeg, input, timeout: Option<Duration>, cancel)`: the
  Option-returning probe (`NoStreams` becomes `Ok(None)`) with a caller deadline; `None` means no
  deadline. `probe_media_opt` is unchanged and keeps its 60 s deadline.
- `decode::decode_pcm_s16(binary, input, options) -> Vec<i16>`: the same argument vector as
  `decode_pcm_f32` except `-c:a pcm_s16le -f s16le` in place of `-f f32le`. `decode_pcm_f32` is
  unchanged. It returns samples only, as `decode_pcm_f32` does.

## 0.2.6

### Breaking for exhaustive matches and struct literals

Marked `#[non_exhaustive]` now, so future variants and fields are not breaking again:
`MediaError`, `RunError`, `LoudnessError`, `ProbeError`, `NativeCatalogError` (error enums that
keep growing), `ProcessOutcome`, `NativeLibrary` (grows with each catalogued library),
`ContentFormat`, and the struct `RunOptions`. Consequences for consumers:
- a `match` on any of these enums needs a wildcard arm (this release also adds variants to
  `MediaError` and `RunError`);
- `RunOptions { .. }` struct literals no longer compile outside this crate. Use
  `RunOptions::new(timeout, cancel)`, `RunOptions::unbounded(cancel)` or `RunOptions::default()`
  with `with_deadline`, `with_kill_grace`, `with_cancel`, `with_current_dir` and `with_progress`;
  every field is reachable that way.
Left exhaustive on purpose because their variant sets are complete by definition:
`Tool` (ffmpeg, ffprobe), `StdioMode` (null, piped, inherit), `LifecycleDecision`
(continue, cancel, timeout), `ArtifactStatus`, `Linkage`, `VerificationMethod`, `NativeStatus`,
`FfmpegArg`, and the unverified-resolution enums (`BuildMode`, `UnverifiedSource`,
`ConfigLicense`, `AnyFfmpeg`, `ResolutionKind`, `DevResolved`).

Source-level caveats: `LoudnessError` gains `TruePeakExceeded`, so exhaustive matches need a
new arm. `RunOptions::timeout` is now `Option<Duration>` (`None` = no deadline); construct it
with `RunOptions::new` or `RunOptions::unbounded`, and read it as an option. Code that
reads the field directly needs `Option` handling. `two_pass_normalize_filter` takes a
`print_format` argument (it is new in this release). Everything else is additive, with these further source-level caveats: `MediaError` gains
`MalformedBinary` and `ComponentNotAllowed`; `RunError` gains `InvalidCurrentDir`; `Artifact`
gains `content_sha256` and `Component` gains `source_commit` (struct literals need them);
`NativeRecord` gains `content_sha256`; an approved catalog row now requires `contentSha256`;
`StagedBinary::sha256` now holds the content digest; the `size` precheck is gone.

- Catalog identity is now a signature-independent content digest (`content_digest`,
  `content_identity`, `verify_content`). Mach-O: SHA-256 of the file with the code signature
  and its load command removed, `ncmds`/`sizeofcmds`/`__LINKEDIT` normalised and codesign
  padding trimmed, equal to `codesign --remove-signature` output. Universal binaries are
  digested per slice and every slice must match an approved row for its architecture. PE: the
  Authenticode rule. Anything else: plain SHA-256. Malformed Mach-O or PE is an error. Rows
  carry `contentSha256`; `sha256` and `size` remain provenance of the retained bytes. Native
  library rows may carry it too (`NativeCatalog::approved_file` selects one file of a
  multi-file library).
- New variants `decode-av1` (libdav1d 1.5.4, BSD-2-Clause, pinned by git commit through the
  new `sourceCommit`) and `decode-apple` (VideoToolbox/AudioToolbox ffprobe), each with an
  explicit component and flag policy; `decode` still forbids third-party components. Rows
  added for the retained dav1d ffmpeg (arm64, x86_64), the ffprobe `decode-apple` build
  (arm64, x86_64 slices), and whisper.cpp 1.9.3 / ggml 0.20.2 macOS arm64 dylibs; the
  `export-ass` arm64 row and all `decode` rows gained `contentSha256`. Notices:
  `third-party/dav1d-COPYING.txt`, `third-party/whisper.cpp-MIT.txt`.
- `MediaError::is_not_in_catalog()`: true for exactly `DigestMismatch` and `NoApprovedArtifact`
  (the only errors that permit a development fallback); the match is exhaustive. README section
  "Falling back to a development ffmpeg".
- Consumer evidence (HeardRight, Dell, 2026-10-11, not a RightKit test): its bundled `ffmpeg.exe`
  verified against the catalog through the real resolution path ("media tool: verified") with the
  `contentSha256` rows, so the approved Windows row resolves in practice.
- Runner latency: `run_capture`, `run_with_progress`, `run_with_stdin` and the `-version`
  probe wait on the child's exit instead of polling every 10 ms, removing up to 10 ms from each
  probe, loudness measure and decode. Cancellation is still checked every 25 ms.

- `CancellationToken::from_flag(Arc<AtomicBool>)` shares a flag the caller owns, and
  `CancellationToken::flag()` returns the shared flag.
- ffprobe JSON: `probe::parse_ffprobe_json` and the `ProbeJson` types (format, streams,
  chapters) for `ffprobe -v error -print_format json -show_format -show_streams
  -show_chapters`. Typed codec, pixel format, colour metadata, frame rates, duration, bit
  rate, sample rate, channels, `creation_time`, `timecode`, and tags. Helpers
  `ProbeStream::is_hdr` (PQ or HLG transfer), `is_variable_frame_rate` (`r_frame_rate`
  and `avg_frame_rate` differ), and `ProbeJson::first_video` / `first_audio`.
  `run_ffprobe_json` runs any FFmpeg-binary handle, and `VerifiedFfprobe::ffprobe_json`
  runs the catalog-verified ffprobe. The `ffmpeg -i` stderr probe is unchanged.
- Loudness: `LoudnessAcceptance { lufs_tolerance_lu: 0.5, true_peak_max_dbtp: None }`
  with `verify`, `measure_loudness_verified`, and `LoudnessReport::verify` (checks the
  delivered file). The existing `measure_loudness` and `ensure_within_tolerance` keep
  their signatures and behavior. `parse_loudnorm_output` reads `output_*`, and
  `two_pass_normalize_filter` builds the in-graph linear second pass for callers that
  compose their own filter graph.
- Runner: `FfmpegCommand::stdin_piped(true)` removes `-nostdin` and makes the child's stdin
  a pipe. `runner::run_with_stdin` hands the `ChildStdin` to a caller closure on a scoped
  thread, with the same cancel, timeout, and process-tree handling as `run_with_progress`.
  `FfmpegCommand::raw_args(Vec<String>)` is a migration adapter for untyped argument vectors.
- `VerifiedFfmpeg::probe_capabilities` lists encoders, decoders, and filters for a
  catalog-verified build, with the same parsing as `UnverifiedTool::probe_capabilities`.

- Verification cache: `resolve_ffmpeg` and `verify_ffmpeg` cache the whole-file digest and
  the `-version` probe for the process lifetime, keyed by canonical path, size, mtime, file
  identity (device and inode on Unix; `None` on Windows, where the file index is not in stable
  `std`), artifact id, and expected digest. A touched or rewritten file is verified again.
  `verify_cache_clear()` forgets everything. `digest_computations()` counts this thread's
  digests for tests and diagnostics. Only successes are cached.
- ffprobe JSON additions: `Rational` (exact `{ num, den }`) for `r_frame_rate` and
  `avg_frame_rate`, with `r_frame_rate_fps()` and `avg_frame_rate_fps()`. `rotation` (from
  side data), `profile`, `bits_per_raw_sample`, `duration_raw` (the text ffprobe printed), and
  `duration_ns` (exact nanoseconds parsed from it), on streams and the format.
- Re-exports of `OwnedChild`, `WaitOutcome`, `ReapOutcome`, `TerminationOptions` (from
  `rightkit-process`), `spawn_streaming`, `StdioMode`, and `StdioPolicy` at the crate root, so
  consumers need no direct `rightkit-process` dependency.
- `RunOptions::with_current_dir` (and `runner::spawn_streaming_in`) runs the child in a
  directory; a missing or non-directory path is `RunError::InvalidCurrentDir` before spawn
  (new variant: exhaustive `RunError` matches need an arm), and the program path is made
  absolute first so the child cannot pick a binary from that directory.
- `ProbeStream::time_base` (exact `Rational`, `None` for absent, `0/0` or zero denominator) and
  `PacketSample::pts_seconds` / `dts_seconds(time_base)` (documented `f64` results).
- `probe::probe_packets` lists the first N packets of one stream (`-show_packets` with
  `-read_intervals`) as `PacketSample { pts, dts, duration, size, flags }`.
- `probe::probe_media_with` takes an optional deadline (`probe_media` keeps its 60 s);
  `probe_media_opt` returns `Ok(None)` for an input with no streams.
- `decode::decode_pcm_f32` decodes to interleaved `f32` in memory, for any `FfmpegBinary`.
- `runner::spawn_streaming` returns an `OwnedChild` with caller-chosen `StdioPolicy`
  (`Null`, `Piped`, or `Inherit` per stream). Dropping it stops the process tree. It works for
  ffprobe too, since both implement `FfmpegBinary`.
- `RunOptions::unbounded`, `with_deadline`, and `with_kill_grace`. A deadline too large to
  represent never fires. Cancellation applies in every mode. `run_capture_with` and
  `run_checked_with` take the options; `run_capture` keeps its 2 s grace.
- `DevFfmpeg::try_unverified` is refused in release builds with `UnverifiedRefusedInRelease`.
  `DevFfmpeg::allow_in_release_builds` is the explicit escape. `DevFfmpeg::unverified` is
  unchanged, so existing callers still compile; release code should move to `try_unverified`.
- `two_pass_normalize_filter(measured, target, print_format)` equals
  `loudnorm_filter(target, Some(measured), fmt)` when a format is given.
- Documentation: `FfmpegCommand::base` adds `-y`; `transcode_audio_command` emits `-ar`
  before `-ac`; `pcm_s16le_16khz_mono_args` adds `-vn`.

## 0.2.5

- New default-on feature `native` gates the `native` module (native-library catalog). Default
  builds are unchanged; FFmpeg-only consumers can build with `default-features = false`.
- Internal: `stage_external_bin` writes through `rightkit_fs::atomic_write_with` (temp file,
  fsync, atomic rename, directory fsync) instead of a hand-rolled copy and rename. The mode is
  set to `0755` before the rename, and on Windows an existing destination is replaced atomically
  instead of being deleted first. Temp files are now named `.<name>.rktmp-*`.
