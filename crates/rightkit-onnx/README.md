# rightkit-onnx

ONNX Runtime dynamic-library boundary for Right Suite apps. It owns runtime library selection and validation (`configure_runtime`, `configure_runtime_with_options`), the process environment for ONNX Runtime (`init_environment`, `init_environment_with_options`), execution-provider selection and CPU fallback (with the `session` feature), and a re-export of the pinned `ort` crate so apps do not declare `ort` themselves.

The crate accepts ordered absolute candidates from the app. It validates the platform filename, requires a regular file, canonicalizes the selected path, records diagnostics, and refuses to replace a different configured runtime.

**Use it** in any Right Suite app that runs ONNX models, so every app binds one ONNX Runtime the same way.

**Do not use it** to download, bundle, or choose a runtime version at app run time. The build-time `provision` feature (below) is the one exception: a build script fetches the catalog runtime into the app bundle. The crate owns no product environment variable, data root, bundled runtime, model, provider policy, download, or UI. Application adapters convert legacy environment variables and app-data locations into explicit `RuntimeCandidate` values.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `session` | on | `ort` (pinned `=2.0.0-rc.14`, `load-dynamic`, API 24) | Session and environment APIs, `ExecutionProvider`, `SessionOptions`. Disable it to keep only the path and diagnostic resolver. |
| `coreml` | off | `session`, `ort/coreml` | Names the CoreML provider explicitly. CoreML is already on for Apple targets. |
| `directml` | off | `session`, `ort/directml` | Names the DirectML provider explicitly. DirectML is already on for Windows targets. |
| `tracing`, `download-binaries`, `tls-native`, `copy-dylibs`, `preload-dylibs` | off | `session`, `ort/<same>` | Pass-through ort features, so no crate declares `ort`. |
| `cuda`, `tensorrt`, `migraphx`, `openvino`, `webgpu`, `nnapi`, `acl`, `armnn`, `azure`, `cann`, `nvrtx`, `onednn`, `qnn`, `rknpu`, `tvm`, `vitis`, `xnnpack` | off | `session`, `ort/<same>` | Pass-through execution-provider features (asr, qts). |
| `dev-overrides` | off | none | Development and QA builds only. Honors `ORT_DYLIB_PATH` as a developer runtime override and stops `clear_inherited_runtime_for_release` clearing it. Never implied by a build profile (installed dev builds with debug assertions behave like release) and never a default feature; an app enables it only from its QA/test configuration. |
| `catalog` | off | `rightkit-ffmpeg` | `BindVerification::Catalog` for `bind_installed_runtime`: the runtime file and its companion files are checked against the native-library catalog. Without it only the manifest check exists and no catalog code is pulled. |
| `provision` | off | `rightkit-ffmpeg`, `rightkit-models` (HTTP, tar, zip) | Build-time `rightkit_onnx::provision` and the `rightkit-onnx-provision` binary. Use it as a build-dependency, never in the shipped app. |
| `half` | off | `session`, `ort/half`, `half` | Re-exports `half` (f16 tensors), so apps use `rightkit_onnx::half::f16`. |
| `ndarray` | off | `session`, `ort/ndarray`, `ndarray` | Re-exports `ndarray` for `Tensor::from_array` and `try_extract_array`. |

## Platform support

- Apple targets: CoreML provider enabled by the crate (`cfg(target_vendor = "apple")`).
- Windows: DirectML provider enabled by the crate (`cfg(target_os = "windows")`), plus `windows-sys` as a Windows-only dependency.
- The approved ONNX Runtime shared library for macOS is `1.30.0` universal2 (`onnxruntime-1.30.0-macos-universal2`, both Apple slices); for Windows it is the DirectML `1.24.4` build. Both are recorded in `rightkit-ffmpeg`'s `catalog/native-libraries.json`, not in this crate, which pins one version per OS until RightKit CI builds its own universal2 runtime (bogusyogi/rightkit#889). Runtime upgrades need a catalog row and real native-host verification.

Published version: see `INDEX.md` at the repository root. Release notes are in `CHANGELOG.md`.

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
use rightkit_onnx::{
    configure_runtime_with_options, legacy_runtime_candidate, CandidateSource,
    RuntimeBindingOptions, RuntimeBindingStatus, RuntimeCandidate,
};

// App supplies absolute paths; no process cwd or implicit workspace discovery.
let binding = configure_runtime_with_options(
    [
        RuntimeCandidate::new(runtime_path, CandidateSource::Bundled),
        legacy_runtime_candidate(workspace_root)?,
    ],
    &RuntimeBindingOptions { force_replace: true, allow_missing: true, ..Default::default() },
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
`defer_initialization: true` loads the dylib now and stores the options, while
environment creation waits for the first `ort_environment()` call (every session
build in this crate makes one).
Deferred reports have status `Deferred`, empty `runtime_info` & no confirmed
active shared pool. `shared_pool_active()` includes a committed deferred pool
configuration so sessions can use it once the environment is built.
`telemetry: None` preserves ort's default; `Some(false)` disables telemetry &
`Some(true)` enables it. `verbatim_loader_errors: true` preserves the loader's
original Display message, including newlines, without any added prefix.

`tolerate_already_initialized: true` accepts external configuration & reports
`AlreadyInitialized` on subsequent successful calls. Existing options stay in
place; ignored shared-pool requests never count as an active pool. First outcome
is cached, including failures; tolerance does not turn failures into success.
Binding & initialization belong before worker startup or any ORT use. Forced
environment-variable replacement cannot replace an already-loaded dylib.

ort rc.14 has no implicit global environment: every `Session::builder` takes an
`&Environment`, only one may be built per process, and it cannot be rebuilt after it
is dropped. This crate therefore owns the process environment handle.
`rightkit_onnx::ort_environment()` returns it (committing the default options first when
nothing was initialised), so apps that build sessions through the `rightkit_onnx::ort`
re-export write `Session::builder(rightkit_onnx::ort_environment()?)?` and share the
runtime, telemetry and pool options of `init_environment*`. An environment built
directly with `ort::init().build()` cannot be shared and makes the crate's own
initialisation fail; build none. An environment built outside this crate is therefore
never `AlreadyInitialized`; `tolerate_already_initialized` only covers repeated calls.

Tests use temporary fixture files, an injected environment writer & a fake ORT
backend; these option tests require no real runtime binary. Existing macOS E2E
coverage remains separate.

## Acquiring and binding the runtime

One acquisition story, in two halves:

- **Build time: stage the library.** `right-release stage-sidecar` copies the ONNX Runtime library out of the native-library catalog (`crates/rightkit-ffmpeg/catalog/native-libraries.json`) into the app bundle with an entry of kind `native-library` (`library: "onnxruntime"`; the `onnxruntime` kind is its alias). The pinned archive and the extracted file are digest-checked, a row's `companions` (`DirectML.dll` beside the Windows runtime) and the licence notices are staged with it, and the receipt records the catalog row. See `docs/STAGE-SIDECAR.md`. Apps keep no download or digest code of their own. (`provision`, below, is the alternative that also writes a `manifest.json`.)
- **Run time: bind it.** `bind_installed_runtime(&BindSpec)` finds, verifies, binds and reports the staged runtime. Apps keep only their declared `BindSpec`; they hand-write none of the glue (candidate search, digest check, `ORT_DYLIB_PATH`, `PATH`, the Doctor report).
- `rightkit-models` `FileSpec::shared` stays for runtimes that belong to a model (files that install and are reference-counted with the model), not for an app's own ONNX Runtime library.

```rust,no_run
use rightkit_onnx::{bind_installed_runtime, BindSpec, BindVerification};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
// Single-threaded startup, before any ORT use.
let bound = bind_installed_runtime(&BindSpec {
    developer_env: Some("MYAPP_ORT_DYLIB".into()), // honoured only with `dev-overrides`
    extra_dirs: vec![std::env::temp_dir().join("myapp").join("runtime")], // app data, searched last
    prepend_path: true,
    ..BindSpec::new(BindVerification::Manifest)
})?;
println!("runtime {} ({:?})", bound.report.path.display(), bound.report.source);
# Ok(()) }
```

`bind_installed_runtime` runs these steps in order; none of them loads the library (ort does, later):

1. `clear_inherited_runtime_for_release()`: an inherited `ORT_DYLIB_PATH` is dropped unless `dev-overrides` is on. Call `bind_installed_runtime` once, single-threaded, at startup: it removes and sets process environment variables, which needs exclusive access to the environment.
2. Candidates, first regular file wins: the developer override named by `BindSpec::developer_env` (only with `dev-overrides`; a release build ignores the variable and never reads it), then the bundled runtime beside the installed executable (`installed_runtime_candidate`, in `BindSpec::resource_subdir`, `runtime` by default, of `BindSpec::executable` or the current executable), then each of `BindSpec::extra_dirs` (app data; relative directories are skipped). A bundled runtime beats an app-data copy.
3. Verification of the resolved file, **before anything is bound and before any environment variable is written**. `BindVerification::Manifest` checks the `manifest.json` beside the library (`verify_library_in_manifest`). `BindVerification::Catalog` (feature `catalog`) checks the file against this host's `onnxruntime` row of the native-library catalog (`rightkit_ffmpeg::verified_library`; an `approved` or `pinned` row serves, `planned` and `stale-unverified` never do) and every companion file of that row that sits beside it: a companion that is present but not the catalog's (a tampered `DirectML.dll`) is an error, a companion that is absent is listed in the report, because a CPU-only app can run without it.
4. The existing binding (`configure_runtime_with_options` semantics): the known-bad SME2 refusal surfaces as an error, a different inherited `ORT_DYLIB_PATH` conflicts unless `BindSpec::force_replace` is set, `BindSpec::allow_known_bad_runtime` is the QA opt-out. Then, with `BindSpec::prepend_path`, the runtime's directory goes first on `PATH` so sibling DLLs load, and the binding is recorded for `configured_runtime()`.

A failure is a typed `BindError`: `NoRuntime { candidates }` (the diagnostics of every candidate inspected), `Rejected(RuntimeError)` (an unsafe or `System32` path, a wrong file name, a conflicting `ORT_DYLIB_PATH`, `RuntimeError::KnownBadRuntime`) or `VerificationFailed { path, reason }` (the manifest or catalog error). Success is a `BoundRuntime { binding, report }`; `RuntimeReport` carries the selected path and source, the candidates inspected, and the verification that passed: the manifest entry, or for the catalog the row (`id`, `version`, `status`, `host_proof`: `Some(HostProof::NeverRun)` for a pinned row whose bytes were never run on a host of that OS) plus `companions_verified` and `companions_missing`. A pinned, never-run row still binds; the report is how a Doctor screen shows it. The file is verified before it is bound, not while ort loads it: a local writer that replaces it in that window is outside the check.

Unit tests drive an internal function with an injected environment (reader, writer, `PATH` writer, executable and runtime check), so none of them mutates the process environment; one integration test (`tests/bind_installed_runtime.rs`) runs the real wrapper.

## Runtime bundle manifest

`verify_runtime_manifest(dir)` checks a staged ONNX Runtime bundle before anything loads it. `dir`
is the bundle directory and must be absolute; `manifest.json` sits inside it. The usual layout,
staged beside an installed executable:

```text
<exe dir>/runtime/
  manifest.json                              not listed in its own manifest
  onnxruntime.dll                            platform library (libonnxruntime.dylib on macOS)
  DirectML.dll
  models/face_detection_yunet_2023mar.onnx
```

```json
{
  "schema": 1,
  "platform": "windows-x64",
  "onnxruntime": { "version": "1.24.4", "variant": "directml", "catalogRow": "<catalog row id>" },
  "files": [
    { "path": "onnxruntime.dll", "sha256": "<64 hex>", "bytes": 12345 },
    { "path": "models/face_detection_yunet_2023mar.onnx", "sha256": "<64 hex>", "bytes": 232589 }
  ]
}
```

Rules:

- `schema` is the integer `1`. Unknown fields are rejected. `platform` is lowercase `<os>-<arch>`.
- Each `path` is relative to `dir`, uses `/`, and is normalised: no `..`, `.`, leading `/`, drive
  prefix, `\`, repeated or trailing `/`. It may not name `manifest.json`. Paths are unique.
- Every listed file must exist as a regular file with the listed byte count and SHA-256. Each file
  is hashed in full. The manifest digest may be uppercase; the result is lowercase.
- Extra files: every regular file under `dir`, at any depth, must be listed. The only exemption is
  `manifest.json` at the top level. Directories may exist. Symbolic links and special files anywhere
  under `dir` are rejected.

On success it returns `VerifiedRuntime { dir, platform, onnxruntime, files }`, with `dir`
canonicalised. `onnxruntime_library_path()` gives `<dir>/<runtime filename>` when that library is
listed, and `None` otherwise. Verification loads no library and creates no session. The catalog row
is recorded, not checked; catalog checks belong to the native-library catalog in `rightkit-ffmpeg`; the `provision` feature applies them when it writes the bundle.

### One library against an app manifest

`verify_library_in_manifest(lib)` verifies a single library against the `manifest.json` in its
directory when that manifest also lists models or other providers, has extra fields, or the
directory holds unlisted files. Everything except the library's entry (keyed by file name) is
ignored: schema, platform, unknown fields, other entries (even malformed or absent on disk) and
unlisted files. The entry is strict: a `files` array must list it exactly once with a 64-hex
`sha256` and `bytes`, and the file must be a regular file in that directory with matching size and
full SHA-256. A missing manifest or `files` array, or any mismatch, is an error. It returns the
`VerifiedFile`.

`configure_runtime_verified(candidates)` is `configure_runtime` that fails closed: the selected
library is verified this way before `ORT_DYLIB_PATH` is bound, and on any failure the environment is
left untouched. The check runs before binding, not during ort's load.

## Provisioning a runtime bundle (build time)

Enable the `provision` feature as a **build-dependency** so the app itself pulls no HTTP, tar or zip code. `provision_runtime` downloads the pinned archive from the native-library catalog, checks its SHA-256 before extraction, extracts only the runtime files, checks each against the catalog digest, and writes the `manifest.json` that `verify_runtime_manifest` reads. Only `approved` and `pinned` catalog rows are used.

| Target | Files written to the bundle directory | Catalog rows |
|---|---|---|
| `windows-x64` | `onnxruntime.dll`, `DirectML.dll` | `onnxruntime` + `directml`, windows / x86_64 |
| `macos-arm64` | `libonnxruntime.dylib` (from the catalog zip or tgz; a `universal` row such as `onnxruntime-1.30.0-macos-universal2` serves it) | `onnxruntime`, macos / aarch64 or universal |
| `android-arm64` | `libonnxruntime.so` (member `jni/arm64-v8a/libonnxruntime.so` of the `.aar`) | `onnxruntime`, android / aarch64. **No such catalog row ships yet**, so this target returns a `Row` error until one is added; UNVERIFIED against a real AAR. |

`Cargo.toml` of the app:

```toml
[build-dependencies]
rightkit-onnx = { version = "0.2.4", default-features = false, features = ["provision"] }
```

`build.rs`:

```rust
use rightkit_onnx::provision::{provision_runtime, ProvisionTarget};
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let Some(target) = ProvisionTarget::from_cargo_env() else { return };
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // `resources/onnxruntime` is owned by provisioning: other files in it are removed.
    let outcome = provision_runtime(
        target,
        &manifest_dir.join("resources").join("onnxruntime"),
        &out_dir.join("onnxruntime-archives"),
    )
    .expect("provision ONNX Runtime");
    println!("cargo:warning=onnxruntime {} ready (reused: {})", outcome.runtime.onnxruntime.version, outcome.reused);
}
```

The same from a shell or xtask: `rightkit-onnx-provision --dest resources/onnxruntime --cache target/onnxruntime-archives [--target windows-x64|macos-arm64|android-arm64] [--catalog native-libraries.json]`.

Notes:

- The destination directory is owned by provisioning. `verify_runtime_manifest` rejects unlisted files, so the bundle is staged next to it, verified, then swapped in; a failure keeps the previous bundle.
- A second call with a matching verified bundle returns `reused: true` and downloads nothing. Verified archives stay in the cache directory for later builds.
- `provision_runtime_with(&catalog, target, dest, cache, &mut fetch)` takes an explicit catalog and a fetcher (`FnMut(url, sha256, dest_file) -> Result<(), String>`), for offline builds from a pre-staged mirror and for tests.
- Apps stage the Windows bundle and its `manifest.json` with `rightkit-onnx-provision` (or `provision_runtime`) and keep no nupkg or DLL SHA-256 of their own; the catalog row is the only pin, so it cannot drift. The DirectML licence and notices are `rightkit-ffmpeg/third-party/DirectML-1.15.4-LICENSE.txt` and `DirectML-1.15.4-ThirdPartyNotices.txt`. The rows `onnxruntime-1.24.4-windows-x64-directml` and `directml-1.15.4-windows-x64` stay `pinned` until the DLLs are loaded through this crate on Windows (Orthic-Labs/rightkit#265).
- The `windows-x64` bundle ships Microsoft's DirectML redistributable under the licence in the catalog notice files; ship those notices with the app.

## Version policy

The suite pins the Rust `ort` crate to `=2.0.0-rc.14` with `load-dynamic` and
API 24 enabled in `Cargo.toml`. Upstream `ort` declares `links = "onnxruntime"`, so every crate in one
build must agree; this repository does not check that upstream metadata. Do not enable `ort/default`; this crate's
`Cargo.toml` sets `api-24` on its `ort` dependency. rc.14 targets ONNX Runtime 1.31 and its own default is
`api-31` (runtime 1.31 or newer); the suite stays on `api-24`, which accepts any runtime from 1.24 up, so the approved
runtime below is unchanged by the rc.14 move. The upstream default API floor is not checked here. The approved macOS ONNX Runtime shared library is
`1.30.0` universal2 and the Windows one is `1.24.4`, recorded in `rightkit-ffmpeg`'s native catalog. Runtime upgrades need
one catalog row, real native-host verification, and coordinated app migration;
this crate does not download or silently select a different runtime.

## Known-bad runtime guard: Apple SME2 KleidiAI leak (bogusyogi/rightkit#889)

ONNX Runtime 1.26.0 and older leak about 92 KB per Conv run through the KleidiAI SME2 Conv cache on
Apple silicon with `FEAT_SME2` (M4 and newer). Upstream fixed it in 1.27.0
(microsoft/onnxruntime#28363). Nothing here touches Windows or x86_64.

- **Bind-time refusal.** `configure_runtime_with_options` (and so `configure_runtime` and
  `configure_runtime_verified`) reads the version from the library file it is about to bind,
  through ONNX Runtime's own `OrtGetApiBase().GetVersionString()`, never from the catalog pin. On a
  macOS arm64 host where `sysctl hw.optional.arm.FEAT_SME2` is 1, a runtime older than 1.27.0 fails
  with `RuntimeError::KnownBadRuntime { path, version, host_feature, issue }` and the environment
  stays untouched. A missing-mode binding has no library to read and is not checked. An unreadable or unparseable version also fails closed on SME2; the explicit
  opt-out permits it only with KleidiAI forced off. Probe handles close on refusal or errors;
  only successful bindings retain a handle. A runtime bound without this crate (an `ORT_DYLIB_PATH` the app set itself, then
  `init_environment_default_loader`) is not guarded.
- **Documented opt-out, QA reproductions only.** `RuntimeBindingOptions { allow_known_bad_runtime: true, .. }`
  binds the old runtime anyway. Every session built through `SessionOptions` then sets
  `mlas.disable_kleidiai = 1` for the rest of the process, and `with_kleidiai(true)` cannot override
  it (`rightkit_onnx::kleidiai::forced_off()` reports it).
- **`SessionOptions::with_kleidiai(enabled)`.** Default `true`: KleidiAI stays on for ONNX Runtime
  1.27.0 and newer (its fast Conv path). `with_kleidiai(false)` sets `mlas.disable_kleidiai = 1`
  (session config key `kOrtSessionOptionsMlasDisableKleidiAi`, present since 1.26) on aarch64
  targets, on any runtime. Session-config forwarding through sherpa-onnx is the app's bundle choice.
- **The fix is the runtime.** To stop the leak, bind ONNX Runtime 1.27.0 or newer: the catalog's
  macOS pin is `onnxruntime-1.30.0-macos-universal2` (provisioning handles its `.zip` archive and
  the `universal` architecture).

## Execution providers

ort rc.14 compiles each execution provider only behind its Cargo feature.
rightkit-onnx turns on `coreml` for Apple targets and `directml` for Windows, so
no consumer feature is needed (the explicit `coreml` / `directml` features forward to
`ort/coreml` / `ort/directml` for consumers that want to name the provider).
Apps that run their own inference use the `rightkit_onnx::ort` re-export
(`rightkit_onnx::ort::{value::Tensor, session::Session, ...}`) instead of declaring
an `ort` dependency, so the release ownership scanner never sees one. Select at runtime with `ExecutionProvider`
(`Cpu`, `CoreMl`, `DirectMl`, or `ExecutionProvider::platform_accelerator()`).
An accelerator that is unsupported, fails registration, or fails model load is
an error unless `SessionOptions::with_cpu_fallback(true)` is set; then the
`*_reported` builders return `BuiltSession { provider, fallback }` naming the
CPU fallback and its reason.

## Android (design stage, nothing verified on a device)

Nothing in this section has been compiled for or run on Android; it was written without a toolchain.

- **Runtime**: the crate stays `load-dynamic`. The app ships `libonnxruntime.so` (from the ONNX Runtime Android AAR, `jniLibs/arm64-v8a`) and Kotlin passes `ApplicationInfo.nativeLibraryDir` in. `android_runtime_candidate(dir)` / `configure_android_runtime(dir)` resolve `<dir>/libonnxruntime.so`. That file exists only when the APK extracts native libraries (`android:extractNativeLibs="true"`); with the AGP default for a recent `minSdk` it stays inside the APK, resolution reports `NotFound`, and the app calls `init_environment_default_loader`, whose bare `libonnxruntime.so` lets the platform linker find it. Which one a given packaging produces is unverified. `discovery_candidates` skips the executable-directory entries on Android (`current_exe` is the zygote's `app_process`).
- **CPU and XNNPACK**: `SessionOptions::with_xnnpack(true)` (Android only, `provider` must be `Cpu`) registers ort's `XNNPACK` execution provider with `error_on_failure`, the ORT CPU provider keeps every node XNNPACK does not claim, the session intra-op pool drops to one thread with spinning off, and `with_xnnpack_threads` sizes XNNPACK's own pool. Elsewhere, or with another provider, the build fails. With `with_cpu_fallback(true)` a failure falls back to a plain CPU session and the report reads `Cpu+Xnnpack -> Cpu: ...`. `probe_providers()` gains `ProviderKind::Xnnpack` (supported only on Android). Whether the shipped runtime contains XNNPACK, and whether it is faster for Parakeet, is unmeasured.
- **NNAPI is not supported here.** The `nnapi` feature remains a pass-through for consumers; this crate registers no NNAPI provider, probes none and claims none. It is a later, measured step.
- **Provisioning**: `ProvisionTarget::AndroidArm64` (`android-arm64`) is parsed and planned, but needs a catalog row first (see the table above).

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
- Added in 0.2.3: `prepend_runtime_dir_to_path(runtime)` puts the runtime's directory first on `PATH` (no duplicate), and `export_discovered_runtime` now does so also when `ORT_DYLIB_PATH` is already set, so an explicit runtime keeps Windows sibling-DLL lookup.
- Added in 0.2.3: `SessionOptions::directml_sequential` & `directml_memory_pattern` (with their `with_*` builders) disable parallel execution & the memory-pattern optimisation when the DirectML provider is selected; both default to off.
- Added in 0.2.3: `explicit_file_candidate` accepts an absolute override with a versioned or renamed file name (relative paths stay refused); `init_environment_default_loader` initialises the environment with telemetry off when no `RuntimeSelection` exists; session builds through `SessionOptions` commit telemetry-off defaults before ort creates the environment. First initialiser wins; see the `environment` module docs for the ordering rules.
- Added in 0.2.3: `verify_runtime_manifest` verifies a runtime bundle; see "Runtime bundle manifest" below.

## Suites that need local assets (skipped on hosted CI)

A hosted runner has none of these assets, so each test prints `SKIP <test>: ...` and passes. Set `RIGHTKIT_REQUIRE_ASSETS=1` on a host that must have them, and the test fails instead.

- macOS, `tests/e2e_runtime.rs`, `tests/default_loader.rs`, `tests/session_first.rs`, `tests/explicit_runtime_versioned.rs`: the approved ONNX Runtime 1.24.4 dylib, enabled by `RIGHTKIT_ORT_TEST_DYLIB=<path>` or the `genright/.cache/.../libonnxruntime.1.24.4.dylib` download, found by walking up from the crate.
- Windows, `tests/e2e_runtime_windows.rs`, `tests/default_loader_windows.rs`, `tests/session_first_windows.rs`, `tests/explicit_runtime_versioned_windows.rs`: the `onnxruntime.dll` and `DirectML.dll` files (catalog row `onnxruntime-1.24.4-windows-x64-directml` is `pinned`, not `approved`, in `rightkit-ffmpeg/catalog/native-libraries.json`), enabled by `RIGHTKIT_ORT_TEST_DLL_DIR=<dir>` or the HeardRight staging folder `heardright/tauri-app-next/src-tauri/resources/runtime`.
