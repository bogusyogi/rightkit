# rightkit-media

Public candidate: FFmpeg safety primitives for Right Suite desktop apps.
The crate contains no FFmpeg binary, app policy, model, URL credential, or
shell invocation.

## Contract

- Select exactly one approved catalog record per tool (`ffmpeg`, `ffprobe`), variant
  (`decode`, `export-lame-ass`), OS, and architecture. `pending` records (recipe and
  spec recorded, digest not yet produced) are never resolvable.
- Validate catalog metadata and SHA-256 before probing or execution.
- Require the approved version banner (`ffmpeg version` / `ffprobe version`) and the
  mandatory `--disable-gpl --disable-nonfree --disable-autodetect` flags; reject
  `--enable-gpl`, `--enable-nonfree`, and GPL encoder libraries case-insensitively.
- Record every statically linked component (licence, source, digest, enable flag,
  notice); the entry's SPDX expression is FFmpeg's LGPL `AND` each component's.
- Resolve catalog binaries only from explicit override, bundled, or explicitly
  enabled development paths. Non-catalog binaries come only from
  `unverified::resolve_unverified` (configured path, named env var, opt-in `PATH`),
  are labelled `UNVERIFIED`, expose encoder/decoder/filter capability probing, and
  are refused in `BuildMode::Release`.
- Stage a verified binary as a Tauri `externalBin`
  (`binaries/<tool>-<target-triple>[.exe]`, `0755`, re-hashed, idempotent) with
  `stage_external_bin`.
- Build FFmpeg arguments as `OsString` values without shell interpolation.
- Run verified FFmpeg through `rightkit-process`, which owns the child tree and
  reaps descendants on exit, cancellation, timeout, failure, and drop.
- Normalise loudness by rendering gain + limiter and re-measuring until the kept
  render is within ±0.5 LU (configurable, never looser), bounded to 5 renders by
  default (port of GenRight `transcode_normalized`).
- Keep the shared audio contract at WAV, mono, 16 kHz, `pcm_s16le`.

The artifact catalog records the binary/source digests, official source URL,
build provenance, full LGPL notice, components, and required configure flags.
`recipes/build-ffmpeg-macos.sh` reproduces the macOS entries from the sources
pinned in `recipes/ffmpeg-sources.json`.

## Publication gate

Publication remains gated on the current Windows catalog rerun after the
approved FFmpeg 7.1.5 macOS arm64 and x86_64 gates. Publish through
`right-release publish cargo` after the gate.

## Verification

```text
cargo test -p rightkit-media --all-targets
cargo fmt --check
cargo clippy -p rightkit-media --all-targets -- -D warnings
cargo package -p rightkit-media --allow-dirty --no-verify --list
```

Licensed under either MIT or Apache-2.0, at your option. FFmpeg artifacts retain
their own LGPL-2.1-or-later license and attribution obligations.
