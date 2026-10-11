# Changelog

## Unreleased

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
  files; id, digests, licence expression and components are unchanged (the components follow
  separately). Apps that ship this binary must ship the three notices with it.
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
