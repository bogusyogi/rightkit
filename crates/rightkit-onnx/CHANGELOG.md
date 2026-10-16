# Changelog

All notable changes to `rightkit-ort`. Versions follow Cargo semver; while the version is `0.x`, a
minor bump may carry breaking changes. Earlier releases are described in `README.md`.

## Unreleased

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
