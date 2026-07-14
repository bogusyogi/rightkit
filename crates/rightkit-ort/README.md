# rightkit-ort

Internal, unpublished ONNX Runtime dynamic-library boundary for Right Suite.
Applications supply ordered absolute candidates; the crate validates the
platform filename, requires a regular file, canonicalizes the selected path,
records diagnostics, and refuses to replace a different configured runtime.

The crate owns no product environment variable, data root, bundled runtime,
model, provider policy, download, or UI behavior. Application adapters convert
legacy environment variables and canonical app-data locations into explicit
`RuntimeCandidate` values.

`publish = false` remains mandatory until the macOS runtime/artifact catalog
gate passes.

Licensed under MIT OR Apache-2.0.
