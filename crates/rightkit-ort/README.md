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

The suite pins the Rust `ort` crate to `=2.0.0-rc.13` with `load-dynamic` and
API 24 enabled (`ort` declares `links = "onnxruntime"`, so every crate in one
build must agree). Do not enable `ort/default`: rc.13 raises its default API
floor to 27, which the approved runtime cannot serve. The approved macOS arm64 ONNX Runtime shared library is
`1.24.4`, recorded in `rightkit-media`'s native catalog. Runtime upgrades need
one catalog row, real native-host verification, and coordinated app migration;
this crate does not download or silently select a different runtime.

## Execution providers

ort rc.13 compiles each execution provider only behind its Cargo feature.
rightkit-ort turns on `coreml` for Apple targets and `directml` for Windows, so
no consumer feature is needed (the explicit `coreml` / `directml` features forward to
`ort/coreml` / `ort/directml` for consumers that want to name the provider).
Apps that run their own inference use the `rightkit_ort::ort` re-export
(`rightkit_ort::ort::{value::Tensor, session::Session, ...}`) instead of declaring
an `ort` dependency, so the release ownership scanner never sees one. Select at runtime with `ExecutionProvider`
(`Cpu`, `CoreMl`, `DirectMl`, or `ExecutionProvider::platform_accelerator()`).
An accelerator that is unsupported, fails registration, or fails model load is
an error unless `SessionOptions::with_cpu_fallback(true)` is set; then the
`*_reported` builders return `BuiltSession { provider, fallback }` naming the
CPU fallback and its reason.

`publish = false` remains mandatory until the macOS runtime/artifact catalog
gate passes.

Licensed under MIT OR Apache-2.0.

## Inherited `ORT_DYLIB_PATH`

`configure_runtime()` never trusts a different inherited value. If `ORT_DYLIB_PATH` is already set and does not canonicalise to the candidate `resolve_runtime` selects, it returns `RuntimeError::AlreadyConfigured` and ORT is not initialised. A planted path can therefore stop ORT from starting, but can never load a foreign library. A release build that should start regardless can clear the variable at startup, before any ORT use: `unsafe { std::env::remove_var("ORT_DYLIB_PATH") }`. For tests and debug builds, put an explicit override first (`RuntimeCandidate::new(path, CandidateSource::Explicit)`); `RuntimeSelection::source` reports which candidate won.
