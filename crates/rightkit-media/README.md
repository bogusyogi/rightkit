# rightkit-media

Public candidate: FFmpeg safety primitives for Right Suite desktop apps.
The crate contains no FFmpeg binary, app policy, model, URL credential, or
shell invocation.

## Contract

- Select exactly one approved OS/architecture catalog record.
- Validate catalog metadata and SHA-256 before probing or execution.
- Require the approved FFmpeg version and LGPL build flags; reject
  `--enable-gpl` and `--enable-nonfree` case-insensitively.
- Resolve only explicit override, bundled, or explicitly enabled development
  paths. Ambient `PATH` lookup is intentionally absent.
- Build FFmpeg arguments as `OsString` values without shell interpolation.
- Run verified FFmpeg through `rightkit-process`, which owns the child tree and
  reaps descendants on exit, cancellation, timeout, failure, and drop.
- Keep the shared audio contract at WAV, mono, 16 kHz, `pcm_s16le`.

The artifact catalog records the binary/source digests, official source URL,
build provenance, full LGPL notice, and required configure flags. Right
Release remains responsible for staging the binary and those notices together.

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
