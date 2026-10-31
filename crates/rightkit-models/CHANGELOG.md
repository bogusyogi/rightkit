# Changelog

## 0.5.2 (unpublished; rightkit#1222)

- Dependencies: requires `rightkit-daemon` 0.5.0 (was 0.4.0), which publishes first.
- Changed (behaviour): **speech models are HeardRight's alone (rightkit#1316, Adrian 2026-10-31).** New `catalog::SPEECH_OWNER_APP` (`heardright`), `catalog::is_speech_model_id`, `catalog::PARAKEET_TDT_RETIRED_STORE_IDS` and `refs::check_speech_owner`. Registration (`refs::register*`, `register_ref`, `Store::register_app`, every ensure, which refuses before any download) refuses a speech id from any other app. Every GC and prune counts a speech id only in HeardRight's ref, so a foreign speech ref on disk no longer pins an entry. `parakeet_spec()` is in no catalog list: `base_catalog()` / `catalog()` is OCR on Windows and empty elsewhere, so `base_status_at` and `base_size_hint_bytes` no longer include TDT. `PARAKEET_ULTRA_RETIRED_STORE_IDS` gains `parakeet_tdt_v3_dynamic_b64_fp16storage_20260720`.
- Added: `store::retire_speech_models(models_root, app_id, active)` and `ModelStore::retire_speech_models(active)`, plus `refs::strip_ids_all_locked`. This is HeardRight's sole-owner retire after Ultra activation. It strips every retired TDT id from every readable ref and removes the entries. It takes no runtime-family lease, skips corrupt ref files, and still honours install-in-progress and entry locks. Tests: `heardright_speech_retire_ignores_foreign_refs_leases_and_corrupt_refs` and `model_store_release_frees_retired_speech_entries_whatever_foreign_refs_say` (which replaces the multi-app release test) in `tests/suite.rs`; `tests/catalog_tiers.rs` is updated for the empty base set.

- Dependency: `ureq` 3.4 -> 3.5 (minimum version); no source change. (rightkit#1284)

- Changed (behaviour): in a `test-mode` build `suite_root()` no longer refuses the real per-user suite root. With `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR` or `ORTHIC_SHARED_DIR` set it is isolated as before; with none set it resolves the real folders exactly like a normal launch (the earlier panic "refusing the real per-user suite root" crashed dev installs at startup). A relative or unresolvable override still fails closed. Release builds (no `test-mode`) are unchanged. Test: `test_mode_without_override_uses_the_real_folders` in `tests/suite_paths.rs` (replaces the fail-closed test).

## 0.5.1 — unpublished staging

Additive to the 0.5.0 public API (no renamed or removed item, no changed signature), with the behaviour changes marked below. **Not compiled locally; CI is the compiler** (this repository's local build grant is revoked: only `rustfmt --check` ran on each changed file, and the code and tests were re-read against the real signatures). The new and changed tests have not been run.

- **Runtime-artifact lane contract admits the `rightsites-*` buckets (bogusyogi/rightkit#903).** `RuntimeArtifactClient::verify_envelope` (and so `resolve*` and `install`) now accepts `distribution.bucket` in `{rightsites-updates, rightapps-updates}` on `private-r2` and in `{rightsites-downloads, rightapps-downloads}` on `public-r2`; `bundled` still requires a null bucket. **Additive: it accepts more buckets; nothing that verified before is refused now.** The manifest names the real bucket its objects live in (no label/upload split), and the bucket stays inside the signed bytes. Everything else is refused as `InvalidManifest`: a bucket outside the lane's set, a lane naming the other lane's bucket (a `private-r2` manifest naming a `*-downloads` bucket, a `public-r2` manifest naming a `*-updates` bucket) and a null bucket on an R2 lane. The signed fixture `tests/fixtures/runtime-envelope.json` gained a `lanes` object (legacy and rightsites vectors for both lanes, plus six correctly signed but out-of-contract vectors); every earlier key is byte-identical (the signer is deterministic and the generator still uses its test-only seed). Test: `lane_bucket_contract_admits_current_and_legacy_buckets_only` in `tests/rightapps.rs`, mirrored by `packages/release/runtime-artifact-manifest.test.mjs`. The JS validator change ships with `@rightkit/release`; sign `rightsites-*` promotions only after the shipped clients carry this version, since an older client refuses them.
- **Release API and retired-entry pruning (Orthic-Labs/rightkit#898).** New `suite::refs::release(refs_dir, app, ids) -> Result<Vec<String>, String>` (plus `release_locked`, `validate_model_ids`, `referenced_ids_excluding_locked`): drops only the listed ids from one app's refs under the registry guard and returns the listed ids no app references any more, sorted. Idempotent; an invalid id or a corrupt ref file is an error and changes nothing; it deletes nothing. New `ModelStore::release(ids)` / `release_with_report(ids)` (`suite::store::release_models`, `ReleaseReport`) and `Store::release_app_model(app, ids)`. After the refs are released, the suite store prunes the freed ids that a spec retires (every catalog spec's `retired_store_ids` plus Parakeet Ultra's), skipping an entry with an install in progress or a held entry lock and everything while a runtime-family lease is active; other freed ids are reported and left for `gc`.
  **Retired-entry pruning:** every app's refs, including this app's own, protect an id. Apps explicitly release retired refs after the replacement is Ready. Byte-identical payloads are no longer required; validation, install markers, entry locks and runtime-family leases still protect entries. Pruning runs on every Ready path. Rename to a hidden `.id.retiring-<nonce>` directory precedes removal, so partial Windows failures cannot leave a damaged live entry; later prunes sweep leftovers. Removal failures remain non-fatal and appear in `RetiredPruneReport`. Regression tests cover own/other refs, explicit release, Ready retries, locks, partial removal and leftover sweeps.
- **Parakeet Ultra, `parakeet_ultra_spec` (Orthic-Labs/rightkit#897).** moondream/parakeet-ultra weights on the Parakeet TDT v3 graphs (CC-BY-4.0, based on NVIDIA parakeet-tdt-0.6b-v3). Ids `parakeet_tdt_ultra_static1500_fp16int4_20261022` (Windows) and `parakeet-tdt-ultra` (macOS); routes `/parakeet/windows/ultra.tar.gz` and `/parakeet/macos/ultra.tar.gz`; the `tdt` routes and `parakeet_spec()` are unchanged. `ParakeetUltraPins { archive_sha256, archive_size, files, installed_size_bytes }`, `parakeet_ultra_spec_with(pins)` (refuses malformed pins, a file set that is not exactly `PARAKEET_ULTRA_REQUIRED_PATHS`, and an installed size that is not the sum of the file sizes) and `parakeet_ultra_spec() -> Result<ModelSpec, MissingParakeetUltraPins>`. Pins are per platform: `PARAKEET_ULTRA_WINDOWS_PINS` is filled with the sealed Windows archive (`0ad7f5a2...59b9`, 316,610,098 bytes, seven per-file pins); `PARAKEET_ULTRA_MACOS_PINS` is filled with the sealed macOS archive (`parakeet-tdt-ultra.tar.gz`, `23610964...d4ee`, 391,192,167 bytes, 23 per-file pins, installed size 432,894,292 bytes; the required set gains each `.mlmodelc`'s `metadata.json`). Both platforms are live; pins sealed 2026-10-22. The archive's own `manifest.json` is unpinned and unlisted on both platforms, because the suite store writes its own `<entry>/manifest.json` record over it (macOS archive: 3,977 bytes). Only Linux has no Ultra pins and still fails closed. The archive source carries `size: Some(..)` so the store caps and checks it; `legacy_dirs` is empty; `retired_store_ids` is `PARAKEET_ULTRA_RETIRED_STORE_IDS` (the two TDT v3 ids). `ModelStore::parakeet_ultra_spec()` and `resolve_parakeet_ultra()`. Ultra is not in `base_catalog()` or `all_models()`, and `parakeet_variant_descriptor()` still describes TDT v3 (an Ultra descriptor needs its revision and graph and precision facts, not supplied). Tests: `tests/parakeet_ultra.rs`.
- **EmbeddingGemma 2 Core ML is an add-on on macOS (Pulse decision 2026-10-22).** `add_ons()` (and so `all_models()`) now includes `embedding_gemma2_coreml_spec()` on macOS, after Whisper; `base_catalog()` is unchanged. **Behaviour change:** `add_ons()` is no longer entitlement-gated throughout (the Core ML bundle is public-read, no bearer). New `suite::catalog::spec_by_id(id)` over `all_models()` and `ModelStore::spec_by_id`, `resolve_by_id`, `ensure_by_id`, so an app can ensure it by `EMBEDDINGGEMMA2_COREML_ID` on demand. Tests: `tests/embedding_gemma2_bundles.rs`, `tests/catalog_tiers.rs` (the entitlement assertions now apply to entitlement-gated add-ons only).
- **EmbeddingGemma 2 E43 pins wired: `embedding_gemma2_e43_dml_spec()` now returns `Ok` (Orthic-Labs/rightkit#224).** New public `EMBEDDINGGEMMA2_E43_DML_FILES` (21 rows, each with size and SHA-256, `tokenizer.json` included, exactly the layout `rightkit-embed` reads) and `EMBEDDINGGEMMA2_E43_DML_INSTALLED_SIZE` (494,393,592, the sum of the 21 sizes). The bundle manifest row is `embed-manifest.json` (same bytes as the earlier `manifest.json`: SHA-256 `6b6c02a1d78c006d9632710432b34bf7a853a1a9b9d8874677c799c462c7c509`, 1,041 bytes): the suite store keeps writing its own entry record to `<entry>/manifest.json`, which would have replaced a bundle manifest of that name (the entry would read back `Partial` and `rightkit-embed` would find no schema-2 manifest); `rightkit-embed` 0.6.1 reads `embed-manifest.json` first and falls back to `manifest.json`. `EMBEDDINGGEMMA2_E43_DML_PINS` carries the resealed `.tar.gz` as the archive owner sealed it on 2026-10-22: SHA-256 `b181f45c201be7068d5b1c011da6a5350c14c343a3b75707931ad8baadf2d128`, 370,860,871 bytes, deterministic tar. The earlier plain-`.tar` (`c8ad75d9…`), flat `.tar.gz` (`9b892851…`) and `manifest.json` `.tar.gz` (`54f32f8e…`) builds are superseded and are not live pins. Route unchanged (`.tar.gz`, one wrapper directory `embeddinggemma2-e43-dml/`). Test: `tests/embedding_gemma2_e43.rs` (21-row table, embed layout with `embed-manifest.json`, sizes, spec built with fake archive values, pending-or-complete pins).
- Version is 0.5.1 (a patch: additive only), so dependents that require `rightkit-models` `0.5.0` (`rightkit-browser`, `rightkit-onnx`, `rightkit-tauri-shell`) still resolve; only the `rightkit-models` entry of `Cargo.lock` (0.5.0) is outside this crate and needs the matching update.

## 0.5.0 — unpublished staging

- Raises rightkit-daemon to 0.4.0 for current dependency APIs.
- HTTP bodies stream directly through destination-local `rightkit-fs::AtomicWriter` while computing SHA-256; separate `.part` checkpoints retain resume & cancellation behavior. A checked commit syncs & rehashes staging before publication. Rebuildable model payloads use `durable: false` with default `Rename`, so every reported failure preserves previous destination contents & success atomically exposes verified, synced file contents. Parent-directory crash durability is not promised.
- Checkpoints & resume metadata cannot alias destination through existing file identity, resolved parent paths, Windows native case equivalence, or macOS native case/Unicode equivalence. Windows plain-name rules refuse trailing-dot/space aliases before checkpoint writes; Unix distinct names & Unicode filenames remain supported. Dangling or existing endpoint symlinks are refused before checkpoint writes; parent symlinks remain supported. Complete-checkpoint recovery also honors cancellation before publication.

- Tests: suite-touching tests run in a child process with an isolated RIGHTKIT_SUITE_ROOT (the real-root guard is unchanged).

## 0.4.4 (2026-10-17)

- **New: `suite::archive::install_tar_gz_expecting(archive, dest_root, expected_wrapper, installed_name)`** (Orthic-Labs/rightkit#455). Same atomic install as `install_tar_gz`, but the archive's single top-level directory must be named exactly `expected_wrapper`; another name or a second top-level entry is refused before installation, leaving the previous install untouched and no staging directory behind. `install_tar_gz` is unchanged and shares the implementation. Additive; version not bumped here. Tests: `tests/install_tar_gz_expecting.rs`. Not compiled when written.

- **Runner probes, tiered fit, download opt-ins (Orthic-Labs/rightkit#635, #636, #637, #240 items 6, for CodeRight and Ember).** Not compiled or run here; version not bumped (Cargo.toml untouched). (1) New `probe` module: `ProbeRunner` (boxed-future `run(program, args) -> Option<(stdout, exit)>`, the implementation owns confinement and the per-probe budget), `Hardware::probe_with_runner`, `detect_inference_runtimes` / `InferenceRuntime` (ollama, llama-server, vllm; no lm-studio entry, which has no `--version` probe), and the pure parsers `parse_nvidia_smi_csv`, `parse_wmic_video`, `parse_system_profiler_displays`, `parse_memory`. `hardware::total_memory_bytes()` is new, and macOS RAM now comes from `sysctlbyname("hw.memsize")` instead of spawning `sysctl` (same value). `Hardware::detect` is unchanged in behaviour. (2) `TieredRequirements`, `Hardware::tiered_fit`, `FitVerdict {Good, Caution, Unknown, TooLarge}` and `FitConfidence {High, Medium, Low}`, ported from CodeRight's `fit_for_resources` rules in GB. One deliberate difference: when a GPU reports VRAM and the model declares no VRAM target, the verdict says so instead of CodeRight's "VRAM was not reported". (3) `download::download_resumable_with_options` with `DownloadOptions { discard_partial_on_cancel, resume_without_strong_etag }`, both off by default. A weak resume requires a pinned sha256 and the saved total; it is never applied without a pin. `download_resumable` is unchanged; its doc now states the strong-ETag rule. Tests: unit tests in `src/probe.rs`, `src/hardware.rs` (`tiered_tests`), `src/download.rs` (`weak_resume_tests`). Not covered by a test: the discard-on-cancel path (needs a served cancel; reviewed by reading only). Also `src/android.rs` gained the unit test `fit_selection_on_4_6_and_8_gb_android_profiles` (#208: the 4, 6 and 8 GB fit selection).

Version 0.4.4 (0.4.2 content below plus the 0.4.3 and 0.4.4 entries). Not built or run locally; CI is the compiler.

- **EmbeddingGemma 2 E43 route check, and the archive size pin is now enforced (Orthic-Labs/rightkit#224).** Reviewed a6922185 against the issue: the route constants, the typed required `EmbeddingGemma2E43Pins` (archive SHA-256, archive size, per-file pins; `None` shipped, no digest invented, malformed pins refused) and the `SpecSource::Archive` spec are in place, and the whole-archive SHA-256 is checked by the download before extraction. One gap found and closed: `download_archive` ignored `SpecSource::Archive::size`, so the pinned archive size was recorded but never enforced. It now caps the download at that size (`download_resumable_recorded_capped`, nothing past the pin is written) and checks the file length exactly before extraction; archives without a size pin (Parakeet, whisper) behave as before. Still open and not this crate's: the worker `MODEL_SPECS` entry and upload, the real archive digest, size and per-file pins (REQUIRED INPUT from the archive owner), and telling Pulse Mac2 on BOGUS-DELL when it lands so Membrane stops bundling the model. Tests: the existing `tests/embedding_gemma2_e43.rs` (route, missing pins, malformed pins); the size enforcement is not covered by a new test because it needs a served archive.

- **`ErrorCode::Io` for disk failures and `download_resumable_capped` (Orthic-Labs/rightkit#289, for Ember).** (1) `download_resumable` (and the install path that calls it) reports a failure of the local disk, namely creating the download or destination directory, creating, opening, writing or syncing the `.part`, writing the resume record, renaming into place, and reading a file back to hash it, as `ErrorCode::Io`; before, every failure was `ErrorCode::Download`. Network, status and transport failures stay `Download`. `is_connectivity_error` no longer counts a disk failure or a size-cap failure as "offline". A mirror loop already stops on `Io` (a full disk is not the mirror's fault). Behaviour change for a caller that matched `Download` on a disk failure. (2) New `download::download_resumable_capped(client, request, max_bytes: Option<u64>, progress)`: the cap is checked against the size the server reports (`Content-Length` / `Content-Range`) before any byte is written and again against the bytes received, so a server that sends no length still cannot pass the cap; the call fails with `ErrorCode::SizeMismatch` and removes the `.part`. `None` is exactly `download_resumable`. `DownloadRequest` gained no field, so existing literals compile. (3) Weak ETags, documented: byte-range resume needs a strong `ETag` (`W/"..."` is refused) that matches the saved resume record; a server that sends only a weak ETag or `Last-Modified` restarts from zero rather than risk a spliced file. No fallback was added: `Last-Modified` and size cannot prove the bytes are the same object, and the hash pin would only catch a splice after the whole file is downloaded again. (4) Publish part of #289 (rightkit-models 0.4.3 with `FileSpec::mirrors`, `adopt_flat_dir` and the loopback hook) happens at the next publish; not done here. Tests: unit tests in `src/download.rs`, `tests/download_io_code.rs`.

- **EmbeddingGemma 2 E43 download route and whole-archive SHA-256 pin (Orthic-Labs/rightkit#224).** `suite::catalog` gains `EMBEDDINGGEMMA2_E43_DML_ID` / `_REVISION` / `_ROUTE` / `_URL` (`/embeddinggemma2-e43-dml/e43/embeddinggemma2-e43-dml.tar.gz` on the model worker, public-read), the all-required `EmbeddingGemma2E43Pins { archive_sha256, archive_size, files, installed_size_bytes }` and `embedding_gemma2_e43_dml_spec_with(pins)`, which builds a `SpecSource::Archive` spec (the download verifies the whole archive before extraction) and refuses malformed pins. **The real archive digest, size and file table are not in the repository:** `EMBEDDINGGEMMA2_E43_DML_PINS` is `None`, so `embedding_gemma2_e43_dml_spec()` returns `Err(MissingE43Pins)` and nothing downloads an unpinned archive. REQUIRED INPUT from the owner of the built E43 archive: the archive SHA-256 and size, and per-file size and SHA-256 for the query graphs, batch-4 and batch-8 ingest graphs, `model.onnx_data`, `embed_tokens.i8.npy`, `embed_scales.f16.npy` and `tokenizer.json`; also the worker `MODEL_SPECS` entry and upload. Additive; the E40 per-file spec and the Q4 adopt-only spec are unchanged. Tests: `tests/embedding_gemma2_e43.rs`.

- **Shared runtimes load from one fixed path, `tools/<kind>/current` (Orthic-Labs/rightkit#255; "No new id, no new folder, for every app").** `Store::activate_shared_runtime(kind, version)` installs `<tools>/<kind>/<version>` in place at `<tools>/<kind>/current` (copy to `current.staging`, rename the old `current` to the single kept `current.previous`, rename the staging copy into place; a failure renames `.previous` back; idempotent when the files already match) and returns that path; `paths::shared_runtime_current_dir[_at]` names it. Load the runtime library (for example the ONNX Runtime dylib) from there, never from `<kind>/<version>`: a per-version folder is a new path on every update. `current`, `current.previous` and `current.staging` are reserved (`register_shared` and the path helpers refuse them as versions; `gc_shared` never removes them). Model weights under `revisions/` are data and are unchanged.
Rightkit#240, for Ember. NOT COMPILED OR RUN: written without a build. Version not bumped here (the lockfile is not touched); see the compatibility note.

- **Ordered mirrors with fallback.** `FileSpec::mirrors: Vec<String>` (serde default, omitted from JSON when empty) and `FileSpec::urls()` / `FileSpec::with_mirrors()`. A file's URLs are `url` then each mirror, deduplicated (after the store's URL rewrite too) and capped at `MAX_MIRROR_URLS` (16). Each URL gets the normal bounded retries (`ModelHttpOptions::retries`), then the next is tried; every mirror is checked against the same pinned `bytes` and `sha256`, so a mirror serving wrong bytes fails like a bad status and the next one runs. Cancel, pause, disk and I/O failures stop the fallback. When every URL fails, `ErrorCode::AllMirrorsFailed` carries one `mirror i/n scheme://host/path: <error>` entry per URL (query strings and userinfo are dropped, so a signed URL never reaches the message). A file with one URL behaves exactly as before (same error, same code). A documented hook for user mirror lists is left to the app: it builds `FileSpec::mirrors` from its env, user file and defaults, deduplicates and caps as above (or just calls `urls()`).
- **`Store::adopt_flat_dir` / `adopt_flat_dir_with`.** Adopt a model directory already laid out flat (`<dir>/<FileSpec::path>`), verified by size and sha256 in a private `.adopt` tree, recorded only when every file verifies, the directory left untouched. `NotFound` for a missing file, `InvalidSpec` for a non-directory or a symlink, `SizeMismatch` and `HashMismatch` for wrong bytes. A thin layer over `Store::adopt`, which already took any `existing` mapping.
- **Test-run loopback hook, no cargo feature.** `#[doc(hidden)]` `ModelHttpOptions::allow_loopback_for_tests(&Mode)`, `ModelHttp::for_loopback_tests(options, &Mode)` and `Store::with_loopback_for_tests(&Mode)` allow loopback (`127.0.0.1`, `localhost`, `::1`, any port, plain http) and skip environment proxies. They need a `rightkit_daemon::mode::Mode::Test`, which only `rightkit_daemon::mode::decide` in a build with `rightkit-daemon`'s `test-mode` feature (the app's QA or dev build) can return after the runner-bound isolated root checks; in any other build `decide` yields `Mode::Release`, the hooks return an error (`InvalidSpec` for the store) and change nothing, so a release build cannot turn loopback on. Unlike `dev-worker-override` it covers any loopback port. Note that `Store::with_address_policy` was already public and can allow loopback in any build; this hook is the gated form.
- Confirmed, no change: the per-read idle limit already exists here as `ModelHttpOptions::read_timeout` (default 120 s); `rightkit-http` 0.3.2 gains `ClientConfig::idle_timeout` for callers that use its client directly. `InstallOptions::cancel` and `pause` were already `Arc<AtomicBool>`.
- Compatibility note for the release owner: `FileSpec` gained a public field and `ErrorCode` a variant, and neither is `#[non_exhaustive]`, so a downstream full struct literal of `FileSpec` (without `..Default::default()`) or an exhaustive `match` on `ErrorCode` needs one more line or arm. In this repo only `tests/suite.rs` had such a literal and it is updated. Decide the version bump (0.4.2 versus 0.5.0) with that in mind.
- Tests: `tests/mirrors_flat_hook.rs` (fallback on a failing primary, fallback past wrong bytes, all-fail listing without query secrets, single-URL unchanged, cancel stops the fallback, `urls()` order/dedupe/cap, serde, flat adopt ok/wrong bytes/missing/non-directory/symlink, hook refuses a release run and default loopback stays refused, and a `test-mode`-only test of the hook in a real test run) and a unit test in `src/http.rs`. The `test-mode` test only runs with `--features test-mode`.


## 0.4.1

Additive; Android groundwork (Orthic-Labs/rightkit#154). NOT COMPILED OR RUN, here or on a device: the Android-only call sites (Kotlin feeding real values, the `target_os = "android"` cfgs) are unverified. The logic is pure and its tests are written to run on any host.

- New `android` module (re-exported: `AndroidAbi`, `AndroidFacts`, `init_android_files_dir`, `android_files_dir`, `android_models_root`).
  - `Hardware::from_android(&AndroidFacts)`: an `os = "android"` profile from facts the app reads through the Android API (`Build.SUPPORTED_ABIS`, `ActivityManager.MemoryInfo.totalMem`, `isLowRamDevice`, `SDK_INT`, core count). `arch` is the first ABI the suite builds for (`arm64-v8a`, `x86_64`), else the first recognised ABI, else this build's. Profile id `android-<N>gb`, source `injected`, no GPU. The usable pool is the existing no-GPU rule (half of RAM); no Android-specific factor was invented without a measurement. `AndroidAbi::parse`/`arch`/`is_suite_target`.
  - `Hardware::detect()` on Android reads `/proc/meminfo` (shared `meminfo_total_bytes`, same result as before on Linux) and reports no GPU instead of spawning `nvidia-smi`.
  - Storage: `init_android_files_dir(Context.getFilesDir())` once at startup. After it `suite::paths::suite_root()` is that directory (models in `<filesDir>/models`, tools in `<filesDir>/tools`, `app_data_base` in `<filesDir>/app-data`, `legacy_suite_root` stays inside it), and no `dirs`/account-database lookup runs. The same path again is accepted, a different one is `Conflict`, a relative or `..`-bearing one is `InvalidSpec`. The injected root takes precedence over the `test-mode` overrides: do not call it from a test-mode build that expects redirection. `ModelStore::at_android_files_dir(files_dir, app_id)` gives a store with no global state.
- `Platform::matches` accepts `android` (and the `aarch64-linux-android` triple: its `linux` token no longer vetoes an Android host when `android` is named). A bare `linux` filter still does not match Android. A filter that named neither before still fails closed; a filter naming `android` used to match nothing and now matches Android hosts.
- Not changed, check before relying on it: `rightkit-daemon` (`Suite::resolve` is still called when no files directory is injected) and `rightkit-fs` features `fs`/`lock` have not been compiled for Android, and the catalog's non-macOS entries are the desktop ONNX bundles.
- Host-runnable tests: ABI mapping, facts to profile and fit, `/proc/meminfo` parsing, files-directory validation, set-once semantics, Android platform filters, `ModelStore::at_android_files_dir`.

## 0.4.0

- EmbeddingGemma 2 accelerator bundles (gap ledger rows 186, 209), both per-file, size and SHA-256 pinned, neither in `base_catalog()`, `add_ons()` or `all_models()`:
  - `catalog::embedding_gemma2_coreml_spec()` (macOS only), store id `embeddinggemma2-coreml`: the text package of `FluidInference/embeddinggemma-2-coreml` at commit `3f978ddf92cac9aa07eb2dcf6eb36cf450f3dba4` (`EmbeddingGemma2Text.mlpackage` as its three files, `embeddings.bf16`, `tokenizer.json`, `config.json`; 584,748,275 bytes). URLs are Hugging Face `resolve/<commit>/<path>` and live. The audio and vision packages are not listed.
  - `catalog::embedding_gemma2_e40_dml_spec()` (Windows only), store id `embeddinggemma2-e40-dml`, revision `e40-daa72c5`: the five fixed-length graphs, `model.onnx_data`, `embed_tokens.f16.npy` and `tokenizer.json` (592,894,933 bytes), pins from `scripts/embed-export/eg2-directml/README.md`. **Route pending:** URLs are `<MODEL_DOWNLOAD_BASE>/embeddinggemma2-e40-dml/e40-daa72c5/<file>`; the worker has no such route and nothing is uploaded, so a download fails until both exist. Adoption of hand-placed files works.
  - The pin tables (`EMBEDDINGGEMMA2_COREML_FILES`, `..._REMOTE_FILES`, `EMBEDDINGGEMMA2_E40_DML_FILES`, `..._REMOTE_FILES`, sizes, ids, commit, revision) exist on every platform; `embedding_gemma2_platform_specs()` returns the spec for this platform (empty elsewhere).
- Tests: `tests/embedding_gemma2_bundles.rs`.
- Written, unverified: nothing was compiled or run for this change. The Core ML pins come from the Hub tree listing (LFS `oid` is SHA-256) except `Manifest.json` and `config.json`, hashed from the served bytes; no large file was downloaded and re-hashed.

## 0.3.2

- Suite root resolves through `rightkit_daemon::Suite::resolve`, the rule every Right Suite crate uses, instead of its own copy. `RIGHTKIT_SUITE_ROOT` (what an app's test-mode build sets) is now honoured, as are `RIGHT_SUITE_DIR` and `ORTHIC_SHARED_DIR`; before, only the last two were, so models code could reach the real per-user folder while the rest of an isolated test run was redirected. Empty or whitespace-only values count as unset.
- Behaviour change: the overrides apply only to builds with the new `test-mode` cargo feature (forwards `rightkit-daemon/test-mode`). A build without it reads no override variable; `suite::paths::suite_root()` is the account-database per-user default. A caller that set `RIGHT_SUITE_DIR` or `ORTHIC_SHARED_DIR` in a release build must enable `test-mode` in its test build or pass an explicit root (`ModelStore::at_models_root`).
- In `test-mode`, a root that cannot be resolved (for example a relative override) panics rather than falling back to the real folder.
- `legacy_suite_root()` returns the redirected root in `test-mode` with an override, otherwise `RightSuite` beside `Orthic Labs` as before.
- New dependency `rightkit-daemon` 0.2.3 (`default-features = false`), new constant `suite::paths::RIGHTKIT_SUITE_ROOT_ENV`.
- Tests: `tests/suite_paths.rs` covers both modes.

## 0.3.1

- Requires `rightkit-qa` 0.3.0 (was 0.2.8), so a fresh resolution no longer reaches a release that names renamed-away crates.
- `http::ModelHttpOptions` gains, all additive with the previous behaviour as the default (`ModelHttpOptions` is built with `..Default::default()` in every caller in this repository; a struct literal without it needs the new fields):
  - `trust: TrustRoots` (default `Platform`, the operating system's store; `Bundled` is `rightkit-http`'s Mozilla set). `TrustRoots`, `HttpError` and `DEFAULT_RETRIES` are exported from `http`. `rightkit-http` has no caller-supplied-roots option, so an app's own roots are not possible here.
  - `header_timeout: Option<Duration>` (default 60 s, which `rightkit-http` already applied; it is now settable). The existing `connect_timeout` (30 s) and `read_timeout` (the idle limit, 120 s) are unchanged.
  - `total_timeout: Option<Duration>` (default none): one budget for a whole download call, covering every attempt, the pauses between them and the body, and bounding each attempt through the request timeout. When it runs out the download fails with "download exceeded the total time budget of ..." (recorded as a transport failure for `EntitlementError::Transport`) and nothing is retried; the `.part` file is kept. `ModelHttp::total_timeout()` reads it back.
  - `retries: u32` (default `DEFAULT_RETRIES` = 2). The download loop made three attempts in all (`MAX_ATTEMPTS = 3`, now removed), that is two retries after the first attempt, so the default is unchanged. `0` makes one attempt and no pause. `ModelHttp::retries()` reads it back.
  - `loopback_host: Option<LoopbackHost>` and `ModelHttpOptions::allow_loopback_host(host, port)`, honoured only with the new non-default cargo feature `dev-worker-override` (development only; see the README). Without the feature the call is accepted and ignored. New: `DEV_WORKER_OVERRIDE_ENABLED`, `ModelHttp::client_for(url)` (downloads and `ensure_with_grant_via` use it), `ModelHttp::dev_loopback_host()`. `ModelHttp::client()` is still the normal, loopback-refusing client.
  - The user agent was already settable (`user_agent`, `app`). It is now validated as a header value: `ModelHttpOptions::validate()` and the new `ModelHttp::try_new(options) -> Result<ModelHttp, HttpError>` report an invalid one (and an invalid loopback host); `ModelHttp::new` still never fails and falls back to the default user agent / drops the allowance. The default stays `rightkit-models/<version>`.
  - `From<Client> for ModelHttp` uses the default retries and no total budget, as before for the idle limit.
- `Cargo.toml`: new feature `dev-worker-override` (empty, off by default). The `continuous_reason` text now names `ModelHttpOptions::retries` and `total_timeout` as the bound on the retry pause.
- Tests: `tests/http_options.rs`.

- Crate root re-exports `ModelStore` and `EntitlementError` (`pub use suite::{...}`), so a wrapping
  app imports them from one path. The README has a "Wrapping this crate" section.
- Additive, no existing signature changed:
  - Typed entitlement download: `ModelStore::try_ensure_entitled(spec, token, cancelled,
    progress)` returns `Result<ModelStatus, EntitlementError>`. `token` is the raw
    token; the crate sends `Authorization: Bearer <token>`. Surrounding whitespace and one
    leading `Bearer ` (any case) are stripped first, so a pasted `Authorization` value works.
    A value that is then empty, or still has whitespace or non-ASCII characters, is
    refused with `EntitlementError::InvalidRequest` before any request. Variants:
    `NotEntitled { status }` (401 or 403), `Refused { status, message }` (other
    non-success statuses after the transient-status retries), `RateLimited(GrantError)`
    (HTTP 429, `retry_after()` gives `Retry-After`; the same type the grant API
    returns), `Transport(message)` (no response) and `Install(message)` (verify, disk
    or install failure after the transfer). `Display` matches the string text.
    `ModelStore::ensure_entitled` is unchanged; its `bearer` is the full
    `Authorization` header value (`Bearer <token>`) and is sent as given. The same
    convention applies to `suite::whisper::ensure_whisper_model`.
  - `suite::lease::LeaseOptions::interactive()` waits at most `INTERACTIVE_WAIT`
    (120 seconds) with no cancel hook. The struct default is unchanged: `wait: None`
    is unlimited, and `LeaseOptions::within(d)` still sets a bound.

- EmbeddingGemma 2 Q4 catalog entry (ledger row 166): `catalog::embedding_gemma_q4_spec()`, store id
  `embeddinggemma2-q4`, revision `b64acc4`, six files with per-file size and SHA-256 pins, total
  187,632,172 bytes. Not in `base_catalog()`, `add_ons()` or `all_models()`. **Route not yet served:**
  every file has `url: None`, so the entry is adopt-only (files placed in the entry dir and verified) and makes no download.
  `EMBEDDINGGEMMA2_Q4_INTENDED_ROUTE` names the route the worker would need. Licence: to be confirmed.
  Test: `tests/embedding_gemma_spec.rs`.
- Corrected the grant contract: `docs/model-grants-contract.md` no longer says no worker implements
  `/v1/model-grants`. The worker source exists (`rightsites/workers/heardright-models`), serves two
  Parakeet routes, and its deployment is unverified.

## 0.3.0

- **Breaking: `ModelSpec` is `#[non_exhaustive]`.** Struct literals of `rightkit_models::ModelSpec` outside
  this crate no longer compile; build it with `ModelSpec::new(id, revisions)` and `with_label(...)`. New
  fields can now be added without breaking callers. In-crate literals and tests were updated.
- **Grant errors are typed (additive).** `suite::grant::try_acquire_grant_pinned` and
  `try_acquire_grant_with_client` return `GrantError`: `RateLimited { retry_after, message }` for HTTP 429,
  with `Retry-After` parsed as delay-seconds or an IMF-fixdate (`GrantError::retry_after()`), and `Failed`
  for everything else. `parse_retry_after_header` is public. The `String`-returning grant functions are
  unchanged in signature and message text.
- Outbound requests are vetted. Model downloads and grant requests go through
  `rightkit-http` (new `http` module) instead of a direct ureq agent that followed
  ten redirects with no address checks. Destinations that resolve to loopback,
  private, link-local, CGNAT, multicast or metadata addresses are refused on the
  first request and on every redirect hop, and a refusal is reported as
  "address denied" (never as an offline error).
  - Downloads: `ModelHttpOptions` (`proxy` default `ProxyMode::Environment` with
    destinations still vetted, `user_agent` / `app`, `connect_timeout`,
    `read_timeout`, `body_timeout`, `max_redirects`, `address_policy`);
    `http_client()`, `http_client_with_options()`. User agent default
    `rightkit-models/<version>`, or `<app>/<app version> rightkit-models/<version>`
    when `app` is set. `Store::with_http_options` and `Store::with_address_policy`
    configure a store; `ModelStore::with_http_client` and `ensure_with_grant_via`
    supply the transport. Resume (strong ETag + `Range`), SHA-256 verification,
    progress and cancellation are unchanged.
  - Grants: `acquire_grant_pinned` / `acquire_model_grant_pinned` use a
    first-party client pinned to the worker host, HTTPS only, redirects never
    followed, environment proxies honoured (`grant_client`, `grant_policy`).
    `GrantSession::{authorization_pinned, refresh_pinned, with_client}`.
  - Timeouts. The ureq agent set connect 30 s and a receive-body deadline of 120 s
    measured from the response headers to the last byte (a total, per attempt, so
    a body slower than 120 s was cut and resumed, at most three attempts); it set
    no response-header timeout. Now: connect 30 s, response headers 60 s
    (`rightkit-http` default), body with no total deadline unless `body_timeout`
    is set (as in `Client::download_to_path`), and a per-read idle limit
    `read_timeout` (default 120 s) that cuts a stalled body into a resumable
    attempt. `rightkit-http` has no per-read timeout, so the idle limit runs the
    blocking read on a helper thread; a stalled read's thread ends when the socket
    read returns or errors.
  - Compatibility: entry points that took `&ureq::Agent` now take
    `&impl AsHttpClient` (a `ModelHttp`, a `rightkit_http::Client` or, still, a
    `&ureq::Agent`). An agent contributes only its redirect limit; its proxies,
    TLS and timeouts are ignored. `http_agent()`, `acquire_grant`,
    `acquire_model_grant` and `GrantSession::{authorization, refresh}` are
    deprecated; the grant ones ignore the agent. The ureq dependency stays:
    `rightapps` (`RuntimeArtifactClient::resolve`) still uses it directly and is
    not yet vetted.
- Shared Whisper large-v3-turbo, catalogued for HeardRight and ScrapeRight.
  `catalog::whisper_turbo_ggml_spec()` is the ggml entry `whisper-turbo` at
  `<suite>/models/whisper-turbo/whisper_turbo_q5_k.bin` (574,041,195 bytes,
  large-v3-turbo q5_k). Its archive pin is `whisper-win.tar.gz`, SHA-256
  `e4fa834a3b6000ca51b71ac42799277b33496e2f86c43e95b3da57b170f76f9f`; its file
  pin is the `.bin` itself, SHA-256 `fcacbe37c8ddd49a137b3ef7cccd2743bea1f9a9dd20d2b9755978972a02bb0b`.
  The entry is Pro-gated (`requires_auth()`) and an add-on (`ModelTier::AddOn`).
  It is in `add_ons()` and in `all_models()` (base set, then add-ons), and it is
  not in `catalog()`: `catalog()` stays the base set, as in 0.2.8, so setup,
  onboarding and size estimates never require it. `catalog()` returns
  `base_catalog()` (Parakeet, plus OCR on Windows).
- Catalog accessors: `base_catalog()` (every install needs it), `add_ons()`
  (optional, entitlement-gated), `all_models()` (base then add-ons, for code that
  manages the whole store), and `catalog()` (the same as `base_catalog()`).
- Lease waits: `suite::lease::acquire_bounded_at_root` and `ModelLease::acquire_bounded`
  take `LeaseOptions` and return `Result<ModelLease, AcquireError>`. `AcquireError::Held`
  names the holder (`Held`), `Cancelled` is the cancel hook, and `Io` is a file error.
- Archive-only descriptors, not store entries: `catalog::WHISPER_COREML_ARCHIVE`
  (macOS `whisper-multi`, SHA-256 `046ce2ae...`) and `catalog::WHISPER_IOS_ARCHIVE`
  (iOS `whisper-multi`, SHA-256 `a37fc293...`). A store entry needs per-file size
  and SHA-256 pins for every file, and those are not recorded yet, so neither
  bundle is installable.
- `suite::whisper`: `WhisperKind { Ggml, CoreMl }`, `resolve_whisper_model`
  (size-depth, read-only, returns the verified `.bin` path) and
  `ensure_whisper_model` (entitlement-gated: takes the Pro entitlement bearer, no grant; locked, verified, atomic install that
  registers the app ref), each with an `_at` variant for an explicit models
  root. CoreML requests return `NotFound` with the archive digest.
- Not verified at release: the `whisper-win.tar.gz` archive is not reachable.
  On 2026-10-11 the model worker answered 404, not the 401 that its code gives
  an unauthenticated request to a catalogued route, for both `/whisper-win.tar.gz`
  and `/whisper-multi.tar.gz`. The `.bin` pin therefore comes from the local
  ScrapeRight copy, which has the recorded size and matches
  `heardright/model_registry/FINAL_MANIFEST_SHA256.csv`. It has not been
  confirmed against the archive digest. Installs fail closed until the route
  serves the archive and its contents match the `.bin` pin.

## 0.2.8

- Cross-process locks now use `rightkit_fs::FileLock` (POSIX record lock on unix, `LockFileEx`
  on Windows, process-wide registry) instead of three `std::fs::File::try_lock` copies:
  the store operation guard (`lock.rs`, `<entry>.operation.lock`), the single-load model lease
  (`suite/lease.rs`) and the refs registry guard (`suite/refs.rs`). Lock files, holder files
  and the legacy `<entry>.lock` TTL marker keep their names and formats. Not replaced: the
  `<entry>.lock` marker protocol and `.holder.json`, which are data, not locks. Mixed-version
  note: on Linux, `flock` (old builds) and `fcntl` (this build) locks do not exclude each
  other; on macOS and Windows they do.
- Atomic writes: `store::write_atomic`, `suite::manifest::write_atomic` and the refs writer
  use `rightkit_fs::atomic_write` (temp file, fsync, rename, directory fsync). Temp files are
  now named `.<name>.rktmp-*`.
- Fix: on Windows a replaced file is no longer deleted before the rename. `store::write_atomic`
  (state.json, ledger writes), download `commit_part` (the repaired model file) and the resume
  metadata commit now replace the existing file in a single rename, so a failed commit keeps
  the previous file. `pub fn http_agent() -> ureq::Agent` is unchanged.
