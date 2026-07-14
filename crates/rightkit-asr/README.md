# rightkit-asr

Internal, unpublished reconciliation of the generic Parakeet ONNX runtime used
by Right Suite products. It is a path-dependency characterization target only:
applications have not migrated to it and it must not be published yet.

The shared API owns model-format loading, typed ONNX execution configuration,
generic RNN-T/TDT decoding, context bias, timestamps, and the retained
DirectML static-conformer correctness workaround. Product environment-variable
compatibility, model choice, model/artifact identity, private manifests,
credentials, download policy, long-audio orchestration, experiments, and UI
remain in application adapters.

## Provenance and license

This derivative is based on crates.io `parakeet-rs 0.3.5`, upstream commit
`7d680db3d62e6aeaf68bc93aaae0ab891a09fae7` (`altunenes/parakeet-rs`, tag
`v0.3.5`). See `UPSTREAM.toml`, `PATCHES.md`, `THIRD_PARTY_NOTICES.md`, and the
preserved upstream `LICENSE`. The derivative currently selects the upstream
MIT grant. Model artifacts are separate works and are not licensed by this
crate.

## Verification

```text
cargo test -p rightkit-asr --no-default-features --features load-dynamic --all-targets
cargo clippy -p rightkit-asr --no-default-features --features load-dynamic --all-targets -- -D warnings
cargo fmt --check
cargo package -p rightkit-asr --allow-dirty --no-verify --list
```

The `load-dynamic` test lane avoids linking the downloaded ONNX Runtime static
library into test binaries. Real Windows inference parity is recorded under
`tools/rightkit/characterization/asr/`; macOS parity and portable fixtures are
still publication gates.
