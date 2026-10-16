# Changelog

## Unreleased

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
