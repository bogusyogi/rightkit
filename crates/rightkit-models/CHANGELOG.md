# Changelog

## 0.2.9

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
  The entry is Pro-gated (`requires_auth()`). It is now in `catalog()`, so
  code that iterates the catalog sees it; filter on `can_download()` and
  `requires_auth()` where a Pro-gated model must not be offered.
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
