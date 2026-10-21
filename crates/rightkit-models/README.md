# rightkit-models

Shared local-model manifest and installer for Right Suite apps. It owns model revisions and quantizations, resumable verified downloads (ETag and Range, SHA-256), hardened archive extraction (tar.gz and zip), atomic activation with rollback, shared runtime directories with reference counting, and hardware-fit preflight (disk, RAM, GPU). It is product-neutral: apps supply the manifest.

**Use it** when an app must download, verify, adopt, activate, or garbage-collect local model or runtime files, and needs the same install semantics as other Right Suite apps.

**Do not use it** to run models, to choose which model an app should use, or to fetch arbitrary files. Inference lives in the app or in crates such as `rightkit-tts-qwen`, `rightkit-onnx`, and `rightkit-ocr`.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `rightapps` | off | `ed25519-dalek`, `base64`, `url`, `chrono` | Enables `RuntimeArtifactClient`, which resolves and installs RightApps promoted private artifacts with signed-manifest verification. Without it, only the app-supplied manifest path compiles. |
| `dev-worker-override` | off | nothing | **Development only. Never enable it in a release build.** Makes `ModelHttpOptions::allow_loopback_host(host, port)` take effect, so one named loopback host and port can be reached for a local debug worker. Without the feature the call is accepted and does nothing, and loopback stays refused. |

The default feature set is empty.

## Platform support

Platform-specific code is gated with `cfg(windows)`, `cfg(target_os = "macos")`, and `cfg(unix)`, mainly in `alias.rs`, `disk.rs`, `hardware.rs`, and the `suite/` modules. The store layout, locking, and path validation are the same code on each platform. Symlink and hard-link behavior follows the host file system. `Hardware::probe_with` accepts an app-supplied override for hosts where the probe is unreliable.

### Android (planned; nothing compiled or run on a device)

Android has no `HOME`, no per-user suite folder and no shared store between apps. The app calls `init_android_files_dir(Context.getFilesDir())` once at startup (or builds a store with `ModelStore::at_android_files_dir(files_dir, app_id)`); models then live in `<filesDir>/models`. `Hardware::from_android(&AndroidFacts { total_ram_bytes, supported_abis, low_ram_device, sdk_int, cpu_cores })` builds the hardware profile from facts the app reads through the Android API (profile `android-<N>gb`, CPU only, usable pool half of RAM as for any host without a GPU). `Platform::matches` understands `android` and `aarch64-linux-android`. See the 0.4.1 changelog entry for what is unverified.

Published version: see `INDEX.md` at the repository root. Release notes: see [CHANGELOG.md](CHANGELOG.md) and `INDEX.md`.

## Example

Illustrative. The model store and install calls below are shown with the types from `src/store.rs` and `src/install.rs`. For compiled usage, see `tests/install.rs`, `tests/adoption.rs`, and `tests/e2e_resume.rs`.

```rust,ignore
use rightkit_models::{InstallOptions, Store};

let store = Store::new("/application-data/models", "my-app");
// Build a manifest for the app's model (see the crate's spec module), then
// install it through the store's install API. Progress and activation events
// are delivered to a callback supplied by the app.
```

### Building a `ModelSpec`

`ModelSpec` is `#[non_exhaustive]` from 0.3.0, so build it with the constructor rather than a struct literal:

```rust,ignore
use rightkit_models::{ModelSpec, RevisionSpec};

let spec = ModelSpec::new("whisper-tiny", vec![RevisionSpec { /* revision, files, ... */ ..Default::default() }])
    .with_label("Whisper tiny");
```

### Grant rate limits

`suite::grant::try_acquire_grant_pinned` and `try_acquire_grant_with_client` return `GrantError`. `GrantError::RateLimited { retry_after, message }` is an HTTP 429 from the model worker, with the parsed `Retry-After` (delay-seconds or an IMF-fixdate, converted to a delay from now). `GrantError::retry_after()` returns it. `GrantError::Failed` covers everything else. The grant POST is never replayed, so the caller decides when to retry. The `String`-returning functions (`acquire_grant_pinned`, `acquire_grant_with_client`) keep their signatures and messages.

### Download HTTP options

Downloads stream HTTP bytes directly into destination-local `rightkit-fs::AtomicWriter` staging while computing SHA-256. Separate `.part` checkpoints preserve cancellation & Range resume. Before publication, `commit_checked` syncs file contents & rehashes staged bytes; transfer, digest, sync, verification & rename failures preserve existing destination contents. Model payloads are rebuildable, so publication uses `durable: false` with default `Rename`: success atomically exposes verified, synced file contents, without parent-directory sync or rename persistence across power loss. This policy avoids reporting an ordinary failure after replacing previous content.

Checkpoint, resume metadata & metadata-staging names must differ from destination, including when destination is absent. Endpoint symlinks, including dangling links, are refused before checkpoint writes; parent symlinks remain supported. Parent resolution & existing file identities detect path aliases. Windows uses native ordinal case comparison in matching parent directories & RightKit filesystem plain-name rules to refuse trailing-dot/space aliases; Unicode names remain supported. macOS probes native case & Unicode equivalence inside an exclusively created private directory, then removes it before downloading. Case-sensitive volumes retain distinct names; Unix trailing-dot, trailing-space & Unicode names remain supported.

`http::ModelHttpOptions` configures the transport for downloads (`http_client_with_options`, `ModelHttp::new`, `Store::with_http_options`, `ensure_with_grant_via`). Every default is the previous behaviour.

| Option | Default | Meaning |
|---|---|---|
| `trust` | `TrustRoots::Platform` | Which roots verify servers. `Platform` is the operating system's trust store. `Bundled` is the Mozilla root set compiled into `rightkit-http`, ignoring the machine's store. `rightkit-http` offers no other source, so an app cannot supply its own roots. Verification cannot be turned off. |
| `user_agent`, `app` | `rightkit-models/<version>` | Full value, or `<app>/<version> rightkit-models/<version>`. The value is validated as a header value (printable ASCII, not empty, no edge whitespace): `ModelHttp::try_new` and `ModelHttpOptions::validate` report a bad one; `ModelHttp::new` falls back to the default instead of failing. |
| `connect_timeout` | 30 s | Connection phase limit. |
| `header_timeout` | 60 s | Longest wait for the response headers. |
| `read_timeout` | 120 s | Longest wait for the next body bytes (idle limit). |
| `body_timeout` | none | Total time for one body, from the headers. |
| `total_timeout` | none | One budget for a whole download call: every attempt, the pauses between them and the body. Bytes arriving does not extend it. When it runs out the download fails with "download exceeded the total time budget of ..." and nothing is retried; the `.part` file is kept for a later resume. |
| `retries` | 2 | Automatic retries after the first attempt, so three attempts by default (`DEFAULT_RETRIES`). `0` makes one attempt. Retried: transport failures, stalled bodies, 408, 429 and 5xx; the pause grows from 250 ms to 1 s and never passes `total_timeout`. |
| `max_redirects`, `proxy`, `address_policy` | 10, environment, public-web | As before. |

A single-budget download, like the 3600 s one HeardRight used, is `ModelHttpOptions { total_timeout: Some(Duration::from_secs(3600)), retries: 0, ..Default::default() }`.

#### Debug worker override (feature `dev-worker-override`, development only)

`ModelHttpOptions::allow_loopback_host("127.0.0.1", 8787)` lets a development build reach one named loopback host on one port, for example a local worker, which the public-web address policy otherwise refuses. It is narrow by construction and compiled out of effect in a release build:

- It is honoured only when `rightkit-models` is built with the non-default feature `dev-worker-override` (`http::DEV_WORKER_OVERRIDE_ENABLED`). Without the feature the call is accepted and ignored, so configuration cannot enable it in a release build.
- A request whose URL names exactly that host and port is sent by a separate client with no proxy and no redirects, and that client may reach loopback for that host only. Every other URL, including the same host on another port, goes through the normal client, where loopback stays refused. `ModelHttp::client_for(url)` makes that choice; downloads and `ensure_with_grant_via` use it, and `ModelHttp::client()` is always the normal client.
- Redirects never ride on it. The normal client carries no allowance, so a redirect to that host and port from any other server is refused like any loopback redirect, and a redirect answered by the allowed host is not followed (the 3xx fails the download).
- `host` is a name or IP literal (`localhost`, `127.0.0.1`, `[::1]`); the allowance applies to loopback addresses only, so a name that resolves elsewhere gains nothing.

## Additional capabilities

Added in 0.2.0:

- `Store::adopt` / `adopt_with`: verify an existing install against the pins
  and record it without downloading (hard link on the same volume, else copy;
  atomic swap; fails closed, never records a partial adoption).
- `Store::resolve` / `resolve_or_legacy`, and the opt-in legacy flat alias
  (`Store::with_legacy_alias_root`: `<alias root>/<id>` links to the active
  revision, maintained on install, adopt, rollback, removal, uninstall and GC).
- Disk preflight credits staged, same-key installed (hard-linked) and shared
  files whose pins match (`Event::PreflightCredit`).
- Shared runtimes: `FileSpec::shared = Some(kind)` places a file under the
  app-configured `Store::with_shared_root(kind, dir)`; traversal-checked,
  pinned, never overwritten (`ErrorCode::Conflict`), kept on uninstall.
- `InstallOptions::pause` keeps staging and resumes (HTTP Range with a strong
  ETag, else a clean restart); `cancel` discards staging. `Store::paused`,
  `Store::discard_staging`.
- `Requirements::{cuda_vram_gb, mps_memory_gb, remote_only}`,
  `FitKind::{RentalOnly, Api}`, `Hardware::probe_with(Option<HardwareOverride>)`
  (app-supplied path or JSON, lenient shape), `ProfileAlias`.

Added in 0.2.1:

- `Store::adopt_then_install`: partial adoption in one locked install: adopt
  every present, pin-matching file (hard link, else copy), download only the
  missing or mismatching ones, one atomic swap. Pins are required; nothing is
  recorded until every file verifies; preflight counts only bytes still needed.
- `Store::with_legacy_alias_map` / `with_legacy_alias_paths`: per-id alias
  locations, including inside the store root (`<root>/<id>/rN`, never a
  store-owned name) or a flat runtime dir allowed with
  `Store::with_legacy_alias_allowed_root`. Real paths are never replaced.
- `Store::with_url_rewrite(base)`: test/dev seam that sends downloads to a
  loopback mock (`http://` on `127.0.0.0/8`, `localhost` or `[::1]` only;
  never read from the environment). `Store::download_url` shows the result.

Added in 0.2.2:

- Shared-runtime reference counting: `Store::register_shared(app, kind, version)` records that an app uses `<tools root>/<kind>/<version>` in `tools/refs` (the tools root is `tools` beside the store root, or `Store::with_tools_root`); `Store::unregister_shared(app)` drops them, and `Store::unregister_app` now does so too (its return value is still model ids).
- `Store::gc_shared(kinds)` removes, under the registry lock, unreferenced `tools/<kind>/<version>` directories for the listed kinds only (unlisted tools such as yt-dlp are never touched; an empty list removes nothing) and returns the `(kind, version)` pairs; symlinks are never followed, `tools/refs` is untouched, and a corrupt ref aborts with nothing removed.
- `suite::paths::shared_runtime_dir(kind, version)` / `shared_runtime_dir_at(tools_root, kind, version)` validate both as single safe path segments (`InvalidSpec` otherwise).

See `tests/install.rs`, `tests/adoption.rs`, `tests/partial_alias_rewrite.rs`,
`tests/e2e_resume.rs`, and `tests/shared_runtime_gc.rs` for end-to-end usage. The tests use the
`rightkit-http` testing `MockServer` (Range, dropped connections, servers without resume) as a dev-dependency.
`tests/http_options.rs` covers the HTTP options above (trust roots, user agent validation, timeouts,
retries, the total budget) against in-process listeners; its `dev-worker-override` tests run when the
feature is enabled, and one test checks that the allowance is ignored when it is not.

## Runner probes, tiered fit and download options (unreleased)

- `probe::ProbeRunner` (an async `run(program, args)` returning stdout and exit status) lets a confined caller run every probe child through its own adapter. `Hardware::probe_with_runner(&runner)` runs `nvidia-smi` plus `wmic` (Windows) or `system_profiler` (macOS) through it; RAM comes from `hardware::total_memory_bytes()`, which reads the OS in-process (macOS: `sysctlbyname`). The runner must bound each probe itself; the crate has no runtime to time it out.
- `probe::detect_inference_runtimes(&runner)` runs `--version` for `ollama`, `llama-server` and `vllm` and returns detected/version/install hint for each.
- Pure parsers on captured output: `parse_nvidia_smi_csv`, `parse_wmic_video`, `parse_system_profiler_displays`, `parse_memory` (GB).
- `TieredRequirements { min_memory_gb, recommended_memory_gb, min_vram_gb, recommended_vram_gb }` and `Hardware::tiered_fit(&req)` return `TieredFit { verdict: FitVerdict::{Good, Caution, Unknown, TooLarge}, confidence: FitConfidence::{High, Medium, Low}, reason, detected_memory_gb, detected_vram_gb }`. RAM and VRAM are compared against the recommended and minimum figures; `Requirements` and `estimate_fit` are unchanged.
- `download::download_resumable_with_options(client, request, max_bytes, DownloadOptions, progress)` adds two opt-ins to the default download. `discard_partial_on_cancel` deletes the `.part` and its resume record on cancel. `resume_without_strong_etag` resumes when the server sends only a weak ETag, `Last-Modified` or nothing; it applies only when the request pins a sha256 and the response continues at the saved offset with the saved total, and a same-size splice then fails the pin (`ErrorCode::HashMismatch`). Without the pin it is ignored.
- Byte-range resume still requires a strong ETag by default. A weak ETag restarts from zero (documented on `download_resumable`).

## Shared Whisper model

One Whisper large-v3-turbo model serves HeardRight and ScrapeRight. Both apps
must resolve the same entry, `whisper-turbo`, through `suite::whisper`:

```rust,ignore
use rightkit_models::suite::{ensure_whisper_model, resolve_whisper_model, WhisperKind};

// Engine start: read-only, size-verified, no network.
let model_file = resolve_whisper_model(WhisperKind::Ggml)?;
// Install or repair: entitlement-gated (the app's signed Pro entitlement is the bearer,
// no worker grant), locked, SHA-256 verified, atomic.
let model_file = ensure_whisper_model(WhisperKind::Ggml, "heardright", Some(&entitlement_bearer), &|| false, |_| {})?;
```

- **Format.** whisper.cpp 1.9.3 loads the ggml `.bin` file
  (`whisper_turbo_q5_k.bin`, q5_k quantization). It does not read GGUF. Any
  check that looks for a `.gguf` extension will never find this model.
- **Location.** `<suite>/models/whisper-turbo/whisper_turbo_q5_k.bin`. The store
  id must be one path segment and each archive replaces its whole entry dir, so
  the other Whisper artifacts are separate entries and do not appear in this
  directory. `resolve_whisper_model(WhisperKind::CoreMl)` returns `NotFound`:
  the macOS and iOS `whisper-multi` bundles are pinned by archive digest only.
- **Pins.** The archive (`whisper-win.tar.gz`, SHA-256 `e4fa834a...`) is the
  download trust anchor, and the `.bin` file is pinned by SHA-256 and size
  (`fcacbe37...`, 574,041,195 bytes), so an installed copy is proven on its own.
  Install and repair fail closed on any mismatch.
- **Access.** The archive is Pro-gated (`requires_auth`). The grant route for
  this model has not been exercised, so `GrantConfig::model` for it is unverified.
- **Availability.** On 2026-10-11 the worker answered 404 for
  `/whisper-win.tar.gz`, so `ensure_whisper_model` cannot install this entry
  until the route serves the archive. `resolve_whisper_model` returns `NotFound`
  until then.

## EmbeddingGemma 2 Q4 (route not yet served: adopt a local copy)

The catalog entry `catalog::embedding_gemma_q4_spec()` (store id `embeddinggemma2-q4`,
revision `b64acc4`) describes the six files of one flat
directory: `model_q4.onnx`, `model_q4.onnx_data`, `tokenizer.json`, `tokenizer_config.json`,
`config.json`, `special_tokens_map.json`. Total 187,632,172 bytes. Every file is pinned by size
and SHA-256 (`catalog.rs`, `EMBEDDINGGEMMA2_Q4_FILES`). `rightkit-embed` reads `model_q4.onnx`, `tokenizer.json` and `config.json` (`rightkit-embed/src/embedder.rs:189`, `:209`, `:709`) and checks `model_q4.onnx_data` (`:740`); it does not read `tokenizer_config.json` or `special_tokens_map.json`.

- **Route not yet served.** The model worker has no route for this bundle. Its only served
  routes are two Parakeet archives (`rightsites/workers/heardright-models/src/index.ts`,
  `MODEL_SPECS`). The intended grant route, `EMBEDDINGGEMMA2_Q4_INTENDED_ROUTE`, is a name only.
  A grant also needs one whole-object SHA-256, and this bundle is pinned per file, so an archive
  pin would have to be added first.
- **Adopt, do not download.** Every `RemoteFile` has `url: None`, so `can_download()` is false
  and `ensure` makes no network request; it returns "no download endpoint exists" for an entry
  that is not on disk. To make the entry ready, place the six files byte-identical in
  `<models-root>/embeddinggemma2-q4/`. The verify-before-adopt step then checks each file's size
  and SHA-256 against the pins, writes the manifest, and registers the ref. A seed-dir copy
  would not work for this spec, because `download_files` (the only caller of the seed branch) runs
  only when some file has a URL. This adoption path is unexercised: the test covers the spec only.
- **Placement.** Not in `base_catalog()` (not every install needs it), not in `add_ons()` (that
  list asserts entitlement gating, and this bundle has none), and so not in `all_models()`.
  Reach it with `embedding_gemma_q4_spec()`.
- **Licence.** To be confirmed. The existing specs in this catalog carry no licence field, and
  none was sourced for this bundle.

## EmbeddingGemma 2 E43 archive (route defined, pins required)

E43 (int8 token table, batched ingest graphs; read by `rightkit-embed` 0.6.0) ships as one worker-served `.tar.gz` at `EMBEDDINGGEMMA2_E43_DML_URL`, verified against a whole-archive SHA-256 and size before extraction. The real digest and the per-file pins are not recorded in this repository, so `EMBEDDINGGEMMA2_E43_DML_PINS` is `None` and `embedding_gemma2_e43_dml_spec()` returns `Err(MissingE43Pins)`: nothing downloads an unpinned archive. Once the owner of the built archive supplies the pins, record them in `EMBEDDINGGEMMA2_E43_DML_PINS` (or build the spec with `embedding_gemma2_e43_dml_spec_with(EmbeddingGemma2E43Pins { .. })`). See bogusyogi/rightkit#224.

## EmbeddingGemma 2 accelerator bundles (Core ML on macOS, E40 on Windows)

The suite embedding model is EmbeddingGemma 2. Two platform bundles are described, both per-file, pinned by size and SHA-256, and reached only by their own functions (not in `base_catalog()`, `add_ons()` or `all_models()`). The pin tables exist on every platform; `embedding_gemma2_platform_specs()` returns the one for this platform.

| | macOS | Windows |
|---|---|---|
| Spec | `embedding_gemma2_coreml_spec()` | `embedding_gemma2_e40_dml_spec()` |
| Store id | `embeddinggemma2-coreml` | `embeddinggemma2-e40-dml` |
| Files | `EmbeddingGemma2Text.mlpackage/{Manifest.json, Data/com.apple.CoreML/model.mlmodel, Data/com.apple.CoreML/weights/weight.bin}`, `embeddings.bf16`, `tokenizer.json`, `config.json` (584,748,275 bytes) | `model_32/64/128/256/512.onnx`, `model.onnx_data`, `embed_tokens.f16.npy`, `tokenizer.json` (592,894,933 bytes) |
| URLs | `https://huggingface.co/FluidInference/embeddinggemma-2-coreml/resolve/3f978ddf92cac9aa07eb2dcf6eb36cf450f3dba4/<path>` (live) | `<MODEL_DOWNLOAD_BASE>/embeddinggemma2-e40-dml/e40-daa72c5/<file>` (**route pending**) |

- **E40 route pending.** The worker (`rightsites/workers/heardright-models`) has no route and the files are not uploaded, so `ensure` fails on download until both exist. `can_download()` is true because the URLs are set. Placing the eight files byte-identical in the entry dir adopts them. The layout follows the Parakeet precedent (model name, then revision, then file). Whether the route needs a bearer is undecided; the entry assumes none.
- **Core ML.** Only the text encoder is listed; the repository's audio and vision packages and `position_embeddings.f16` are not. An `.mlpackage` is a directory, so every file inside it is listed. `rightkit-embed` cannot run it yet (README of that crate).
- `manifest.json` of the E40 build is not listed: it records the build time and is not byte-stable.
- Licence: Core ML card says Apache-2.0; E40 derives from `onnx-community/embeddinggemma-2-ONNX`, licence not confirmed here.

## Wrapping this crate

An app that wraps the suite crate (a shared-store wrapper or a host SDK) re-exports the
handle types at the crate root, so the wrapper and its callers need only one import path:

```rust
use rightkit_models::{EntitlementError, ModelStore};
use rightkit_models::suite::catalog::ModelSpec; // the suite's catalog spec, not the root `ModelSpec`
```

- `ModelStore::shared(app_id)` binds the machine-wide shared store (the root is `rightkit_daemon::Suite::resolve`: in a build with the `test-mode` feature `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR` or `ORTHIC_SHARED_DIR` overrides it; a build without `test-mode` reads none of them); `ModelStore::at_models_root(root, app_id)`
  binds an explicit directory (tests, sandboxes). `with_http_client` swaps the transport.
- `EntitlementError` is what `try_ensure_entitled` returns. Match on it to tell "not entitled"
  (`NotEntitled`), "back off" (`RateLimited`), "retry" (`Transport`) and "local failure" (`Install`).
- The root `ModelSpec` re-export is the declarative manifest type from `spec`. The suite's catalog
  type is `suite::catalog::ModelSpec`, and `try_ensure_entitled` takes that one.

## Entitlement downloads and bounded lease waits

Two ways to call the entitlement-gated add-on download. They differ in the bearer
they take, so check which one you call:

- `ModelStore::ensure_entitled(spec, bearer, ...)` and `suite::whisper::ensure_whisper_model`
  take the full `Authorization` header value, for example `Bearer <token>`, and send it
  unchanged. A bare token is not corrected, so the server answers 401.
- `ModelStore::try_ensure_entitled(spec, token, ...)` takes the raw token. The crate
  sends `Authorization: Bearer <token>`. Surrounding whitespace and one leading `Bearer `
  (any case) are stripped, so a pasted `Authorization` value works. A value that is then
  empty, or still has whitespace or non-ASCII characters, is refused before any request
  with `EntitlementError::InvalidRequest`.

`try_ensure_entitled` returns typed errors so an app can branch on them:
`NotEntitled { status }` for 401 or 403; `Refused { status, message }` for other
non-success statuses after the transient-status retries; `RateLimited(GrantError)` for
429, with `retry_after()` giving the server's `Retry-After` (the same type the grant API
returns); `Transport` when no response arrived; and `Install` for verify, disk, hash or
install failures. Its `Display` text matches the string API.

A lease wait with no bound is unlimited: `LeaseOptions { wait: None, .. }` waits as long
as another process holds the model. An interactive app should bound the wait and show who
holds it. `LeaseOptions::interactive()` waits at most `suite::lease::INTERACTIVE_WAIT`
(120 seconds) and has no cancel hook; set `cancel` and `waiting` with struct update
syntax. `LeaseOptions::within(d)` sets any bound. The struct default is unchanged.

## RightApps promoted private artifacts (`rightapps` feature)

The example below is illustrative and not compiled as a doctest. It shows the call sequence, and the host in the example is a placeholder, not a verified endpoint.

`RuntimeArtifactClient` resolves an artifact ID, current promotion row ID or
signed manifest pointer through `POST /v1/apps/:appKey/runtime-artifacts/resolve`.
Pass RightApps
`signedToken` from license activation/refresh, not
`activationToken` or a purchase key. Server checks current Pro entitlement,
active device & license status before minting an object-scoped download grant
that expires within five minutes. Model access does not require an active
software-update window.

Pin the raw 32-byte Ed25519 public key corresponding to
`rightkit-runtime-artifacts-2026-07` in application source/build configuration.
It must equal server `RIGHTSITES_RUNTIME_ARTIFACT_PUBLIC_KEY`; never trust a key
returned by resolve. Signing-key material is not needed in consumers.

```rust,ignore
# #[cfg(feature = "rightapps")]
# fn install(signed_token: &str, runtime_public_key: [u8; 32]) -> Result<(), Box<dyn std::error::Error>> {
use rightkit_models::{RuntimeArtifactClient, Store, InstallOptions};
use rightkit_models::rightapps::RuntimeArtifactTarget;

let client = RuntimeArtifactClient::new("https://api.example.invalid", runtime_public_key)?; // placeholder host
let artifact = client.resolve(
    "genright",
    "qwen3-customvoice",
    signed_token,
    &RuntimeArtifactTarget::current(),
)?;
let store = Store::new("/application-data/models", "genright"); // use app's native data directory
let installed = client.install(
    &artifact, &store, "qwen3-customvoice-talker", &InstallOptions::default(),
    |event| { /* forward progress, pause, activation events to UI */ },
)?;
let model_path = installed.dir.join(&artifact.envelope().manifest.object.filename);
# let _ = model_path;
# Ok(()) }
```

Run this blocking API on a worker thread (`spawn_blocking` in Tauri/Tokio).
Installation reuses model-store locks, signed SHA-256 & size pins, resume with
strong ETags, immutable digest revision directories & atomic activation. Raw
artifact bytes are installed; archives remain archives. Configure
`InstallOptions::pause` for retained partial downloads; cancellation discards
staging. After `ExpiredToken`, refresh the signed license token, resolve again
& install with the same model ID. An unchanged object digest resumes partials
even when its grant URL changes. Network errors retain staging too.

Errors include `NotEntitled`, `ExpiredToken`, `SignatureMismatch`,
`DigestMismatch`, `SizeMismatch`, `Network`, `InvalidManifest`, `NotFound` &
`Store`. Download URLs are sensitive capabilities: do not log or persist them;
`ResolvedRuntimeArtifact` Debug output omits its URL. Resolve & download do not
follow redirects or forward a license token to the download host.

OS-independent bytes use promotion target `{ "os": "any", "arch": "any" }`.
Consumers send their concrete native target. Half-wildcards are invalid. One
`artifactId` promotion serves its exact target or `any/any`; object keys always
bind digest & filename. Artifact IDs use lowercase slug syntax
`^[a-z0-9]+(?:-[a-z0-9]+)*$` with a 128-byte limit. A pointer explicitly names
its promotion slot; resolution never substitutes a different pointer. Legacy
manifests omit `artifactId` & retain their original signed JSON.

Enable with `features = ["rightapps"]`.

JS interoperability fixture regeneration (test-only deterministic key):

```sh
node crates/rightkit-models/tests/fixtures/generate-runtime-vectors.mjs
rightkit cargo test -p rightkit-models --features rightapps
```

Test-only commands. Run Rust through `rightkit cargo` per the workspace's `docs/rules/rightkit.md`.
