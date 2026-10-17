# Changelog

All notable changes to `rightkit-ort`. Versions follow Cargo semver; while the version is `0.x`, a
minor bump may carry breaking changes. Earlier releases are described in `README.md`.

## 0.2.6 (unreleased)

- **ort `=2.0.0-rc.14`** (was `=2.0.0-rc.13`; Orthic-Labs/ember#230; `Cargo.toml` and README Version policy). NOT BUILT OR RUN here: migrated from the rc.14 release notes and source diff only. The ONNX Runtime the suite approves is unchanged at 1.24.4: rc.14 targets ONNX Runtime 1.31 and defaults to `api-31`, but the `api-24` feature this crate sets still accepts any runtime from 1.24 up, so no runtime manifest, catalog row or digest changes. Do not enable `ort/default` (API floor 31, runtime 1.31).
  - **Environment handle (breaking for code that builds sessions through the `ort` re-export).** rc.14 removed the implicit global environment: `Session::builder` takes `&Environment`, `ort::init().commit()` is gone (`EnvironmentBuilder::build()` returns the handle), only one environment may be built per process and a dropped one cannot be rebuilt. This crate now owns the single process handle. New `rightkit_onnx::ort_environment() -> ort::Result<&'static ort::environment::Environment>` returns it (committing the default options, telemetry off and private pools, when nothing was initialised); use `ort::session::Session::builder(rightkit_onnx::ort_environment()?)?`. `init_environment*` still select the runtime, telemetry and shared pool; eager mode now builds the environment immediately, deferred mode (`defer_initialization`) loads the dylib and stores the options and the first `ort_environment()` call (every session build in this crate) builds it. An environment built directly with `ort::init().build()` cannot be shared (rc.14 exposes no handle to it), so it makes this crate's initialisation fail instead of reporting `AlreadyInitialized`; `tolerate_already_initialized` now only covers repeated calls. `SessionOptions` builds go through `ort_environment()` and are otherwise unchanged.
  - `probe_providers()` calls the rc.14 associated `ExecutionProvider::is_available()` (was a method on a default instance).
  - New dependency floor for consumers: crates that call `ort_environment()` require `rightkit-onnx` 0.2.6.
- Add `verify_library_in_manifest(lib: &Path) -> Result<VerifiedFile, RuntimeError>` and `configure_runtime_verified(candidates) -> Result<RuntimeSelection, RuntimeError>` (crate version 0.2.6, additive; Orthic-Labs/rightkit#178). `verify_library_in_manifest` checks one library against `files[]` of the `manifest.json` in its own directory, for apps whose `runtime/` holds more than ONNX Runtime. Tolerant: the schema, platform and `onnxruntime` fields, unknown fields, other entries (valid, malformed or absent on disk) and unlisted files are ignored. Strict for the library: the manifest must exist with a `files` array listing it exactly once (key = file name) with a 64-hex `sha256` and a `bytes` count, and the file must be a regular file inside its directory with matching size and full SHA-256; a mismatch is never tolerated. `configure_runtime_verified` resolves the candidates like `configure_runtime`, verifies the selected library, and only then binds `ORT_DYLIB_PATH`; any failure returns the error and leaves the environment untouched. `verify_runtime_manifest` is unchanged and still rejects unlisted files. Not built or run locally.

- Docs only (Orthic-Labs/rightkit#265): `rightkit-onnx-provision` is the documented way to stage the Windows DirectML bundle and manifest so apps drop their own hashes. The two DirectML catalog rows remain `pinned`; promotion to `approved` needs a live Windows load and a catalog edit outside this crate.

## 0.2.5

- Android CPU/XNNPACK backend (crate version 0.2.5, additive; 0.2.5 is unpublished). NOT COMPILED OR RUN for Android: every `target_os = "android"` path below is unverified. (Orthic-Labs/rightkit#150)
  - `android_runtime_candidate(native_lib_dir)` and `configure_android_runtime(native_lib_dir)` resolve `<nativeLibraryDir>/libonnxruntime.so` (absolute directory only, `UnsafePath` otherwise). With an APK that does not extract native libraries the file is absent; use `init_environment_default_loader` then. `discovery_candidates` no longer adds executable-directory entries on Android.
  - `SessionOptions::xnnpack` / `xnnpack_threads` (`with_xnnpack`, `with_xnnpack_threads`): serve the CPU provider through ort's XNNPACK execution provider on Android; refused (a `SessionError::Runtime`) on other OSes or with a non-CPU provider; XNNPACK owns the pool (session intra-op threads 1, spinning off). `cpu_for_stage()` clears both, and `cpu_fallback` now also covers a failed XNNPACK request (report label `Cpu+Xnnpack -> Cpu`). Adding two public fields to `SessionOptions` breaks an exhaustive struct literal without `..Default::default()`; none exists in this repository.
  - `ProviderKind::Xnnpack` and a matching `probe_providers()` entry (unsupported off Android). `ProviderKind` is not `#[non_exhaustive]`, so an exhaustive `match` downstream needs the new arm.
  - `ProvisionTarget::AndroidArm64` / `android-arm64` (manifest platform string, `from_cargo_env` and `host` mapping, `rightkit-onnx-provision --target android-arm64`, AAR member `jni/arm64-v8a/libonnxruntime.so`). No catalog row exists yet, so provisioning returns a `Row` error until `rightkit-ffmpeg`'s native catalog gains one. `ProvisionTarget` is not `#[non_exhaustive]`, so an exhaustive `match` downstream needs the new arm.
  - Cargo: `ort` gets the `xnnpack` feature on Android targets only (compiles ort's wrapper; load-dynamic links nothing).
  - NNAPI is deliberately not enabled, probed or claimed; it stays a documented later step.
  - Host-runnable unit tests added for the pure rules (candidate shape, XNNPACK refusal and thread rules, probe verdict, target parsing). Not built or run locally.

- Security (behaviour change, crate version 0.2.5): the `ORT_DYLIB_PATH` developer override no longer follows `cfg(debug_assertions)`. It is now behind the explicit, non-default `dev-overrides` feature (Orthic-Labs/rightkit#109). Installed dev builds use the dev profile, where debug assertions are on; without the feature they now behave like release: `developer_override_candidate` returns `None` and `clear_inherited_runtime_for_release` clears an inherited `ORT_DYLIB_PATH`. App migration: enable `rightkit-onnx/dev-overrides` only from a QA/test feature to keep the old debug-build behaviour. Tests: the debug-only tests are gated on the feature and the release-behaviour tests now run by default; run with `--features dev-overrides` to cover the override path. Not built or run locally.

- Add the non-default `provision` feature (crate version 0.2.4, patch because dependents require ^0.2): `rightkit_onnx::provision` and the `rightkit-onnx-provision` binary. For an app's `build.rs` or xtask, `provision_runtime(target, dest_dir, cache_dir)` selects the catalog rows for the target (one `approved` or `pinned` row per library; `planned` and stale rows are refused), downloads each archive over the vetted `rightkit-models` HTTPS client, checks the archive SHA-256 before opening it, extracts only the runtime files (`onnxruntime.dll` and `DirectML.dll` for `windows-x64`; `libonnxruntime.dylib` for `macos-arm64`), checks each against the catalog file digest, writes the `manifest.json` that `verify_runtime_manifest` accepts, verifies the staged bundle, and swaps it into `dest_dir`. A bundle that already matches the catalog is reused with no download. `provision_runtime_with(catalog, target, dest, cache, fetch)` takes an explicit catalog and fetcher; `load_catalog(path)` reads a catalog file. Adds optional dependencies `rightkit-ffmpeg` (catalog) and `rightkit-models` (download, archive extraction), enabled only by `provision`. Without the feature the dependency set is unchanged. Run-once at build time; it adds no continuous work to apps.

Renamed from rightkit-ort; 0.2.2 remains published under the old name.

- Fixed (behaviour change, Windows): `probe_providers()` reported DirectML `available: true` ("compiled into the loaded runtime") with no `DirectML.dll` beside the runtime. DirectML is now `available` only when it is compiled in AND the `DirectML.dll` in the directory of the configured runtime (`configured_runtime()`) loads. Otherwise `available` is false and the detail says why: "compiled in, library missing", the loader's error, or that no runtime path is configured. An app that gates acceleration on the probe now falls back to CPU instead of picking up Windows' own older `DirectML.dll`.

## 0.2.3 - 2026-10-11

- Add `SessionOptions::directml_sequential` and `SessionOptions::directml_memory_pattern` (plus
  `with_directml_sequential` and `with_directml_memory_pattern`). When `true` and the DirectML provider
  is selected, the session is built with parallel execution off and the memory-pattern optimisation
  off, the same settings rightkit-ocr applies. Both default to `false`, so existing sessions are built
  as before. Other providers and `cpu_for_stage()` ignore them.
- Add `verify_runtime_manifest(dir) -> Result<VerifiedRuntime, RuntimeError>`. It reads
  `<dir>/manifest.json` (schema 1: `platform`, `onnxruntime {version, variant, catalogRow}`,
  `files [{path, sha256, bytes}]`), streams every listed file through SHA-256, and checks byte counts.
  It rejects a listed path that is absolute, contains `..`, `.`, `\`, a repeated `/` or a trailing `/`,
  or names `manifest.json`; a missing file; a file whose size or digest differs; a regular file under
  `<dir>` that the manifest does not list; and symbolic links or special files under `<dir>`. Also added
  are `VerifiedRuntime::onnxruntime_library_path()`, which returns the verified library path when the
  manifest lists it, and the types `VerifiedRuntime`, `VerifiedFile` and `OnnxRuntimeIdentity`.
- Add `RuntimeError` variants for these checks: `BundleManifest`, `BundlePath`, `BundleFileMissing`,
  `BundleFileSize`, `BundleFileHash`, `BundleFileUnlisted`, `BundleEntryUnsupported` and
  `BundleFileRead`. `RuntimeError` is not `#[non_exhaustive]`, so an exhaustive `match` in a
  downstream crate needs the new arms.
- Add `prepend_runtime_dir_to_path(runtime)`, which puts the runtime's directory first on `PATH` without
  duplicating it. `export_discovered_runtime` now calls it also when `ORT_DYLIB_PATH` is already set
  (it returned early before), so an explicit runtime keeps Windows sibling-DLL lookup. The discovery
  path now also sets `PATH` when it was unset.
- Depend on `serde`, `serde_json` and `sha2` as normal dependencies (`sha2` was a dev-dependency).
