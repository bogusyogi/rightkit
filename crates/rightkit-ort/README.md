# rightkit-ort

ONNX Runtime dynamic-library boundary for Right Suite apps. It owns runtime library selection and validation (`configure_runtime`, `configure_runtime_with_options`), the process environment for ONNX Runtime (`init_environment`, `init_environment_with_options`), execution-provider selection and CPU fallback (with the `session` feature), and a re-export of the pinned `ort` crate so apps do not declare `ort` themselves.

The crate accepts ordered absolute candidates from the app. It validates the platform filename, requires a regular file, canonicalizes the selected path, records diagnostics, and refuses to replace a different configured runtime.

**Use it** in any Right Suite app that runs ONNX models, so every app binds one ONNX Runtime the same way.

**Do not use it** to download, bundle, or choose a runtime version. The crate owns no product environment variable, data root, bundled runtime, model, provider policy, download, or UI. Application adapters convert legacy environment variables and app-data locations into explicit `RuntimeCandidate` values.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `session` | on | `ort` (pinned `=2.0.0-rc.13`, `load-dynamic`, API 24) | Session and environment APIs, `ExecutionProvider`, `SessionOptions`. Disable it to keep only the path and diagnostic resolver. |
| `coreml` | off | `session`, `ort/coreml` | Names the CoreML provider explicitly. CoreML is already on for Apple targets. |
| `directml` | off | `session`, `ort/directml` | Names the DirectML provider explicitly. DirectML is already on for Windows targets. |
| `tracing`, `download-binaries`, `tls-native`, `copy-dylibs`, `preload-dylibs` | off | `session`, `ort/<same>` | Pass-through ort features, so no crate declares `ort`. |
| `cuda`, `tensorrt`, `migraphx`, `openvino`, `webgpu`, `nnapi`, `acl`, `armnn`, `azure`, `cann`, `nvrtx`, `onednn`, `qnn`, `rknpu`, `tvm`, `vitis`, `xnnpack` | off | `session`, `ort/<same>` | Pass-through execution-provider features (asr, qts). |
| `half` | off | `session`, `ort/half`, `half` | Re-exports `half` (f16 tensors), so apps use `rightkit_ort::half::f16`. |
| `ndarray` | off | `session`, `ort/ndarray`, `ndarray` | Re-exports `ndarray` for `Tensor::from_array` and `try_extract_array`. |

## Platform support

- Apple targets: CoreML provider enabled by the crate (`cfg(target_vendor = "apple")`).
- Windows: DirectML provider enabled by the crate (`cfg(target_os = "windows")`), plus `windows-sys` as a Windows-only dependency.
- The approved ONNX Runtime shared library for macOS arm64 is `1.24.4`. It is recorded in `rightkit-media`'s `catalog/native-libraries.json`, not in this crate. Runtime upgrades need a catalog row and real native-host verification.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

- Added in 0.2.0: `configure_runtime_with_options` adds forced `ORT_DYLIB_PATH`
replacement & missing-path binding with `RuntimeBindingStatus::Missing`.
- Added in 0.2.0: `CandidateSource::LegacyDefault` & `legacy_runtime_candidate`
add opt-in `<workspace>/tools/bin/<platform runtime filename>` candidates.
- Added in 0.2.0: `init_environment_with_options` adds deferred C environment
creation, ort-default telemetry, already-configured tolerance & verbatim loader errors.
- Added in 0.2.0: `clear_inherited_runtime_for_release` clears inherited
`ORT_DYLIB_PATH` in release builds & does nothing in debug builds.
- Added in 0.2.0: `developer_override_candidate(env_name)` reads a non-empty
app-named override as `RuntimeCandidate` with `CandidateSource::Explicit` in debug only.
- Added in 0.2.0: `configured_runtime` reports the last successfully bound path,
falling back to non-empty `ORT_DYLIB_PATH` for doctor readiness reporting.

## Example

The binding snippet below is illustrative (its variables are app-supplied). For compiled checks, see `tests/runtime_contract.rs` and `tests/runtime_helpers.rs`.

## Binding & environment options

Existing `configure_runtime`, `init_environment`, `EnvironmentOptions` &
`EnvironmentReport` retain their signatures, fields & defaults. New option
types use strict binding, eager C environment creation, disabled telemetry,
external-configuration refusal & contextual loader errors by default.

```rust,ignore
// Illustrative: runtime_path and workspace_root are app-supplied.
use rightkit_ort::{
    configure_runtime_with_options, legacy_runtime_candidate, CandidateSource,
    RuntimeBindingOptions, RuntimeBindingStatus, RuntimeCandidate,
};

// App supplies absolute paths; no process cwd or implicit workspace discovery.
let binding = configure_runtime_with_options(
    [
        RuntimeCandidate::new(runtime_path, CandidateSource::Bundled),
        legacy_runtime_candidate(workspace_root)?,
    ],
    &RuntimeBindingOptions { force_replace: true, allow_missing: true },
)?;
if binding.status == RuntimeBindingStatus::Missing {
    // The app decides what to do when no runtime is available.
}
```

Candidates resolve in caller order. Missing mode prefers an available regular
file; only if none resolves does it bind & record the first absent candidate.
Missing paths retain their supplied absolute spelling; regular files are
canonicalized. Relative paths, wrong filenames, directories & System32 runtimes
remain invalid. Legacy candidates use `onnxruntime.dll`, `libonnxruntime.dylib`
or `libonnxruntime.so`; callers choose where to place them in candidate order.

With feature `session`, pass `EnvironmentInitOptions` to
`init_environment_with_options(&binding.selection, &options)`.
`defer_initialization: true` matches `ort::init_from(...).commit()`: dylib loading
still happens now, while C environment creation waits for first ORT use.
Deferred reports have status `Deferred`, empty `runtime_info` & no confirmed
active shared pool. `shared_pool_active()` includes a committed deferred pool
configuration so sessions can use it once ORT creates their environment.
`telemetry: None` preserves ort's default; `Some(false)` disables telemetry &
`Some(true)` enables it. `verbatim_loader_errors: true` preserves the loader's
original Display message, including newlines, without any added prefix.

`tolerate_already_initialized: true` accepts external configuration & reports
`AlreadyInitialized` on subsequent successful calls. Existing options stay in
place; ignored shared-pool requests never count as an active pool. First outcome
is cached, including failures; tolerance does not turn failures into success.
Binding & initialization belong before worker startup or any ORT use. Forced
environment-variable replacement cannot replace an already-loaded dylib.

Tests use temporary fixture files, an injected environment writer & a fake ORT
backend; these option tests require no real runtime binary. Existing macOS E2E
coverage remains separate.

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

Publication is gated on the macOS runtime and artifact catalog; see `INDEX.md`.

Licensed under MIT OR Apache-2.0.

## Inherited `ORT_DYLIB_PATH`

`configure_runtime()` never trusts a different inherited value. If `ORT_DYLIB_PATH` is already set and does not canonicalise to the candidate `resolve_runtime` selects, it returns `RuntimeError::AlreadyConfigured` and ORT is not initialised. A planted path can therefore stop ORT from starting, but can never load a foreign library. Call `clear_inherited_runtime_for_release()` at process startup, before any thread spawns, runtime binding or ORT use: process environment mutation requires exclusive access. This release-only cleanup is consistent with "inherited ORT_DYLIB_PATH fails closed"; debug builds preserve the variable. Apps can put `developer_override_candidate("APP_ORT_RUNTIME")` first in their candidate list; unset or empty values return `None`, as do all release calls. `RuntimeSelection::source` reports which candidate won.

`configured_runtime()` is read-only & prefers the last successful crate binding
(including missing-mode paths) over later environment changes. Matching inherited
bindings are recorded too; failed bindings preserve the previous record. Without
a recorded binding it reports non-empty `ORT_DYLIB_PATH`, or `None`. Resolution
alone records nothing; this diagnostic does not validate or load a library.
- Added in 0.2.2: `discovery_candidates`, `discover_runtime` & `export_discovered_runtime` own the worker-process runtime search (`ORT_DYLIB_PATH`, `<exe>/runtime`, `<exe>`, caller dirs; System32 copy skipped); pass-through ort features (`tracing`, download/copy, execution providers).
