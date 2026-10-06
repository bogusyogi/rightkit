# rightkit-ort

Internal, unpublished ONNX Runtime dynamic-library boundary for Right Suite.
Applications supply ordered absolute candidates; the crate validates the
platform filename, requires a regular file, canonicalizes the selected path,
records diagnostics, and refuses to replace a different configured runtime.

The crate owns no product environment variable, data root, bundled runtime,
model, provider policy, download, or UI behavior. Application adapters convert
legacy environment variables and canonical app-data locations into explicit
`RuntimeCandidate` values.

## Version policy

The suite pins the Rust `ort` crate to `2.0.0-rc.12` with `load-dynamic` and
API 24 enabled. The approved macOS arm64 ONNX Runtime shared library is
`1.24.4`, recorded in `rightkit-media`'s native catalog. Runtime upgrades need
one catalog row, real native-host verification, and coordinated app migration;
this crate does not download or silently select a different runtime.

`publish = false` remains mandatory until the macOS runtime/artifact catalog
gate passes.

Licensed under MIT OR Apache-2.0.
