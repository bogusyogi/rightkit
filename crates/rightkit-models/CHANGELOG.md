# Changelog

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
