# Changelog

## Unreleased

Additive; no existing signature, field, variant or behaviour changed, no dependency change. Not compiled when written; verification is the CI run.

- **Several device ids (bogusyogi/rightkit#610, related #391).** `verify_token_any_device(token, pubkey, app, &[&str], now)` and `authenticate_token_any_device(..)`: the signature is checked once and the token's `device_id` must equal one candidate exactly (raw and hashed ids, for example). An empty list is `VerifyError::DeviceMismatch`; every other failure is device-independent, so the error returned is the token's own (a matching but expired token reports `Expired`, not a mismatch from another candidate). `verify_token` and `authenticate_token` now delegate to these with one id and behave as before. `AuthorizationService::with_device_alternates` belongs to `rightkit-device` and is not part of this change.
- **Revoked entitlement-id denylist (bogusyogi/rightkit#501).** `RevokedIds` (exact-match set) and `verify_token_not_revoked(token, pubkey, app, &[&str], now, &RevokedIds) -> Result<Entitlement, RevocationError>`. The id is read only from an authenticated token and checked before expiry, so a revoked token reports `RevocationError::Revoked { id }` even when expired; other failures arrive as `RevocationError::Verify(VerifyError)`. `RevocationError` is `#[non_exhaustive]`. The existing `VerifyError` enum is unchanged.
- **Unverified claims peek (bogusyogi/rightkit#391).** `peek_unverified_claims(token) -> Result<UnverifiedClaims, VerifyError>` decodes `id`, `app`, `tier`, `iat`, `exp` without any signature, app, device or time check, for decisions taken before verification. Tokens over 16 KiB are refused. The result is untrusted and must never grant access. The process-tree usage half of #391 is `rightkit-child-process` work and is not in this crate.
- Tests: unit tests in `src/lib.rs` and `src/revocation.rs` (second candidate, empty and near-miss lists, exact matching, wrong key and app still fail, listed id refused including after expiry, unlisted and empty list pass, forged token never reaches the denylist, peek of valid, unsigned, malformed, oversized and incomplete tokens).
- Not changed: #164 is already shipped as `SubjectEntitlementSource` (0.2.1, feature `subject-entitlement`); #272 needs backend registration data (host, key, trial issuance) that this crate does not hold.

## 0.2.2

Additive, behind the new cargo feature `engine-gate` (off by default; implies `subject-entitlement`). With the feature off the crate builds and behaves exactly as 0.2.1. One small additive method on the existing `SubjectEntitlementSource`, available whenever `subject-entitlement` is on. Version is a patch bump on purpose: in 0.x a minor bump (0.3.0) would break the `rightkit-license = "0.2.0"` requirement in `rightkit-device`, and 0.2.1 added a feature the same way. Not compiled when written; verification is the CI run (`Cargo.lock` is regenerated there).

- **`engine-gate` (new `gate` module).** The engine-side entitlement check for app engines that can be run outside their shell (Orthic-Labs/rightkit#182; consumer bogusyogi/CutRight#86, decision option B: keep the documented CLI and agent plugin, gate the engine). `EngineGate::new(SubjectEntitlementSource, Enforcement)`; `check()` resolves the entitlement (cache inside 24 hours, else the entitlement service, else the cache inside the 7 day offline grace window) and returns a `GateOutcome { verdict, enforcement }`. If no bearer token can be produced (headless, signed out) it falls back to the local cache alone, so a CLI or another app can run on the entitlement the shell app cached. `check_offline()` is that cache-only read and never touches the token provider or the network.
- **Verdict, typed.** `Verdict::{Allowed { source, expires_at }, Grace { source, expires_at, grace_ends_at }, Denied(DenyReason)}` (`#[non_exhaustive]`), `DenyReason::{NotEntitled, Expired { expired_at }, Unauthorized, SignedOut(String), Unavailable(String), Misconfigured(String)}` (`#[non_exhaustive]`). `Verdict::from_result` / `from_decision` / `from_error` are pure, so a caller that already holds a decision (the licence and device come from the caller) can map it without a source. Every error maps to a refusal; none grants. A grace window never turns a non-Pro or lapsed answer into a grant.
- **Exit codes and messages.** `Verdict::exit_code()` / `GateOutcome::exit_code()`: `EXIT_ALLOWED` 0, `EXIT_DENIED` 77 (not entitled, expired, unauthorised, signed out), `EXIT_UNAVAILABLE` 69 (could not establish and no grace covers it), `EXIT_CONFIG` 78 (rejected request, bad configuration). `message()` is never empty and carries no token, key or response body. `GateOutcome::must_report()` is true for everything but a plain `Allowed`; `emit(&mut dyn Write)` writes it.
- **Enforcement is a release-time switch.** `Enforcement::{Enforce, Advisory}` (`from_switch(bool)`): `Advisory` lets the engine run after a refusal but `must_report()` stays true and the message says "not enforced". No environment or command-line override exists in the crate, and there is no parent-process, launch-token or handshake check: headless use is never blocked by how the engine was started.
- **`SubjectEntitlementSource::cached_decision()` (feature `subject-entitlement`).** The decision the local cache alone supports, no token request and no network: `Cached` inside the re-check interval, `OfflineGrace` inside the grace window, `None` otherwise (none, another app, clock earlier than the fetch time, past grace). A lapsed answer comes back with `pro == false`.
- Tests: `tests/engine_gate.rs` (loopback fake, no network: allowed, free, lapsed, grace, past grace, offline with no cache, 403 clears the cache, unknown app, headless fresh cache, headless stale cache, no token and no cache, lapsed cache, cache-only read makes no request or token call, advisory) and unit tests in `src/gate.rs` (verdict mapping, error table and exit codes, enforcement, UTC formatting).
- Trust note unchanged from `subject-entitlement`: server JSON over TLS and a cache that is not tamper-proof. The gate stops the accidental and casual unlicensed run; the Ed25519 `EngineEntitlements` path remains the offline cryptographic enforcement.

## 0.2.1

Additive, behind the new cargo feature `subject-entitlement` (off by default). With the feature off, the crate builds and behaves exactly as 0.2.0: no existing signature, field, token format, lifecycle, refresh, clock or StoreKit-related behaviour changed. Not compiled when written; verification is the CI run (`Cargo.lock` is regenerated there).

- **`SubjectEntitlementSource` (new `subject` module, feature `subject-entitlement`).** An entitlement source keyed to the Google subject, backed by the shared entitlement service (`bogusyogi/rightsites#81`, consumer issue Orthic-Labs/rightkit#164). `entitlement()` returns a `SubjectDecision { entitlement, freshness, pro }` for Pro bought through Play Billing, a Windows licence activation, or a StoreKit purchase linked to the account; `is_pro()` is `false` on any error. Calls use `GET /v1/entitlement?app=<app_id>` with `Authorization: Bearer <Google ID token>`; `link_play_purchase` (`POST /v1/licenses/bridge/from-play`, body `{app, package_name, product_id, purchase_token}`), `link_storekit` and `link_activation` (`POST .../from-storekit`, `.../from-activation`) return and cache the entitlement object `{app, pro, source: play|storekit|license|none, expires_at: RFC3339|null, checked_at}`. rightsites#81 does not give the from-storekit or from-activation request bodies; the minimal ones used are `{app, jws}` and `{app, activation_token, device_id}` and need confirming against the deployed service.
- **Token provider, no `rightkit-oauth`.** The caller supplies a `BearerTokenProvider` (`bearer_token(force_refresh: bool) -> Result<String, TokenError>`; any matching closure works). A 401 triggers one forced refresh and one retry; a second 401 or any 403 is `Unauthorized`, clears the cache and gets no grace.
- **Cache, expiry, offline grace.** Answers are cached with the local fetch time (`SubjectCacheStore`, in-memory `MemorySubjectCache` by default, caller-supplied for persistence across restarts). Served from cache for at most 24 hours (`SUBJECT_RECHECK_SECS`, also the clamp ceiling), `expires_at` honoured strictly (a lapsed answer never grants Pro and forces a re-check). Offline grace is 7 days (`SUBJECT_OFFLINE_GRACE_SECS`, also the ceiling) from the last good check, and applies only to transient failures (unreachable, timeout, 5xx, 429, unreadable answer); it never extends `expires_at`. A local clock earlier than the cached fetch time distrusts the cache. No background task: the caller invokes `entitlement()` when it needs a decision.
- **Error mapping (`SubjectEntitlementError`, `#[non_exhaustive]`).** `Token`, `Unauthorized` (401 after refresh, 403), `Offline` (transport, timeout), `RateLimited { retry_after }`, `Server { status }`, `BadResponse` (unparsable JSON or timestamps, or a response for another app), `Rejected { status }` (other 4xx), `Config` (address policy, HTTPS required, bad request). `is_transient()` is true for `Offline`, `RateLimited`, `Server`, `BadResponse`. Error text carries no token, key or response body; `PlayPurchase` redacts the purchase token in `Debug`.
- **HTTP.** Optional dependency on `rightkit-http` 0.3 (new in this crate), pulled only by the feature; default client is HTTPS-only with the public-web address policy; `with_client` takes a caller-built `Client`. Dev-dependency on `rightkit-http` with `testing` for the loopback fake server.
- **Trust note.** The answer is server JSON over TLS, not an offline-verifiable signed token, and a persisted cache is not tamper-proof. The Ed25519 path remains the offline enforcement primitive.
- Tests: `tests/subject_entitlement.rs` (loopback `MockServer`, no network) and unit tests for the RFC 3339 parser in `src/subject.rs`.

## 0.2.0

Additive (M13, engine-side entitlement). Default behaviour and existing signatures unchanged. Not compiled when written; verification is the CI run.

- **`EngineEntitlements` (new `engine` module).** An engine process (sidecar, stdio helper, daemon, headless CLI) verifies the stored licence itself: `EngineEntitlements::new(EngineEntitlementsConfig::new(pubkey, app, data_root, device_source))` reads `<data_root>/license.token` (`with_token_file` for another plain name; path separators ignored), authenticates it with `verify_token` (signature, app, device binding from the engine's own `device_source`, `iat`, expiry, lifecycle) against a `TrustedClock` (`with_clock`, default `SystemClock`), and exposes `require(feature)`, `require_pro()`, `require_requirement(&LicenseRequirement)` and `current()`. A verified result is cached for a bounded TTL (default 60s, clamped to `MAX_CACHE_TTL_SECS` = 300s, `0` disables); expiry is re-checked on every call and `invalidate()` drops the cache. There is deliberately no input for a pushed flag, snapshot or environment override. Failures are typed (`EngineEntitlementError::{NoLicense, Unreadable, DeviceUnavailable, Invalid(VerifyError), Clock, NotEntitled}`), all fail closed. Token files over 16 KiB are refused unread. The window-to-engine pattern (window writes the signed token, sends only a content-free "licence changed" hint, engine calls `invalidate()` and re-verifies) is documented in the module docs. Tests in `tests/engine.rs`: valid, missing, tampered, wrong signer, expired, other device, other app, device-source failure, free tier and missing feature, pushed flag/snapshot/env ignored, cache bound and expiry, TTL clamp and path escape, empty and oversized file.

Additive, from ViewRight gaps 1 to 3 of the second round. Not compiled when written; verification is the CI run.

- **`ClosureClockStore` lock policy.** `with_lock_failure(LockFailure::{Fail (default), Degrade})`: under `Degrade` a lock file that cannot be OPENED lets the call proceed without cross-process exclusion, reported once through `with_degraded_report(|&io::Error|)` and `is_degraded()`; calls still serialise within the process. A lock HELD by another process is never bypassed. The security trade-off (weaker cross-process rollback protection while degraded) is on `LockFailure::Degrade`. `with_lock_wait(LockWait::{Block (default), DoNotWait})`: `DoNotWait` turns a held lock into `ClosureClockStoreError::Lock` with `WouldBlock`; this is the bounded wait available without a timer (the crate must not sleep or spawn). `CrossProcessLock::try_acquire(path) -> io::Result<Option<Self>>` for apps that poll against their own deadline. Doc: the `get`/`set` closures run while the lock is held, so a closure that blocks on a keychain prompt blocks other processes. New exports `LockFailure`, `LockWait`. Tests in `tests/file_store.rs`.
- **Message formatter.** `LifecycleContext::with_formatter(&Arc<LifecycleMessageFormatter>)` where the formatter is `Fn(&LifecycleMessageContext) -> Option<String> + Send + Sync`. `LifecycleMessageContext` (`#[non_exhaustive]`) carries `kind: LifecycleMessageKind` (the eight kinds), `product_id`, `not_before`, `expiry`, `issued_at`, `last_trusted`, `detail`. `None` falls back to the static `LifecycleMessages` override, then the built-in text; a panic falls back to the built-in text. Attached to `LifecycleContext`, not `LifecycleMessages`, so both stay `Copy` (an `Arc` field would remove `Copy` from `LifecycleMessages` and `LifecycleContext` and break existing by-value reuse). Evaluation order is unchanged and documented as deliberate (authenticate, then clock). `LifecycleContext` now has a hand-written `Debug`. Tests in `tests/lifecycle.rs`.

Additive. Closes consumer gaps 5 to 8 reported from ViewRight (its licence code stayed local because of
them). No existing signature, field or match changes. The version number is not bumped here.

### Added

- **Cross-process-atomic store from closures (gap 5a).** `ClosureClockStore::new(lock_path, get, set)`
  implements `ClockStore` over two host closures (a keychain item, a credential entry) and serialises
  `load` (shared), `save` and `compare_and_save` (exclusive, `get` + compare + `set` under one lock) on
  the OS lock of `lock_path`, using the crate's existing `CrossProcessLock` (no new dependency).
  `ClosureClockStoreError::{Lock(io::Error), Store(E)}`.
- **Pure judgement (gap 5b).** `judge_refresh_response(params, response, prior: Option<&ClockState>) ->
  RefreshJudgement` decides a refresh response with no I/O; `refresh_and_reanchor` now calls it, and
  the re-anchor decision both use is the pure `plan_reanchor(prior, iat, wall_now, was_initialized) ->
  Result<ReanchorPlan, AnchorRefusal>` (`AntiRollbackClock::reanchor` calls it too). `AnchorRefusal`
  converts into `ClockError`. Behaviour of `refresh_and_reanchor` and `reanchor` is unchanged; a test
  compares the outcome with the judgement across ledger states. The cached-token keep-or-drop verdict
  depends on the live clock and remains in `refresh_and_reanchor` (`RefreshJudgement::NoNewToken`).
- **Message overrides (gap 6).** `LifecycleMessages` (`#[non_exhaustive]`, all `Option<&'static str>`,
  default `None` = unchanged text) attached with `LifecycleContext::with_messages`. Placed on
  `LifecycleContext` and not on `LifecyclePolicy` because `LifecyclePolicy` is built with full struct
  literals (including `const`s) by callers and a new field there would break them.
- **`Rejected(NotActive)` detail (gap 6).** `RefreshRejection` and `RefreshOutcome::Rejected(..)` are
  exhaustive tuple shapes, so they are unchanged. The token's claims are available as
  `RefreshJudgement::Rejected { rejection, token: Option<RejectedToken { iat, exp, lifecycle }> }`
  (`#[non_exhaustive]` struct), obtained by judging the same response (pure).

### Decided (gap 7, no behaviour change)

- A first re-anchor with no ledger keeps starting at `max(wall_now, iat)`. Storing `iat` alone would
  make the first anchor weaker than a ledger-less start (which trusts `max(wall, monotonic)`): a valid
  but old token would let the clock be set back to its era. The wrong-future-wall cost is bounded and
  self-repairing (the next online refresh carries a newer `iat` and lowers the mark). Reasoning is in the
  `plan_reanchor` docs; tests pin both halves.

### Tests

- `tests/file_store.rs` already spawns with `move` closures in the current file (no E0373 found by
  inspection); closure-store tests added in the same style.

- **Test coverage (third Astra read, note 7).** No library change. The concurrent-clock test in
  `tests/clock.rs` no longer ignores every failed `trusted_now`: it accepts only a rollback or a
  contended ledger, fails on any other refusal, and requires that calls succeeded. The "two
  processes" ledger test in `tests/file_store.rs` now runs two real child processes (the test
  binary re-run on `clock_child_process`, coordinated through marker files) instead of two handles
  in one process.

Additive release. `verify_token` and every existing item keep their behaviour and signatures. One
derive was added to an existing type (`VerifyError` is now `Clone`). A source-level break is possible
only for code that exhaustively matches a type marked `#[non_exhaustive]` below, all of which are new.

The three owners the apps each re-implemented now live here: lifecycle classification, the
anti-rollback clock, and refresh-and-reanchor orchestration (gap ledger #47). Device-lease envelope
verification is added for the Pro bridge (ledger #54).

### Added

- **Lifecycle classification.**
  - `Lifecycle` (`#[non_exhaustive]`, serde snake_case): `Unlicensed`, `MigrationRequired`, `Trial`,
    `ExpiredTrial`, `Active`, `Grace`, `Expired`, `LifetimeProvisional`, `VerificationRequired`,
    `Lifetime`, `ClockRepairRequired`, `Revoked`, `Invalid`. This is the union of the states ViewRight
    and HeardRight snapshot today, plus `Grace`, `Expired` and `Revoked` (which the JS client already
    distinguishes). `Lifecycle::snapshot_kind()` returns the existing app strings (`active_trial`,
    `expired_trial`, `active_legacy`, `active_lifetime_provisional`, `verification_required`,
    `active_lifetime`, `clock_repair_required`, `migration_required`, `free`, `invalid`) so a renderer
    keyed on them needs no change. `grants_access()` and `needs_online_refresh()` are provided.
  - `Entitlement::lifecycle(now, &LifecyclePolicy)` classifies an authenticated entitlement.
  - `Entitlement::has_valid_lifecycle()` is now public (was private).
  - `authenticate_token(token, pubkey, app, device)`: signature, app and device checks only, with no
    time or lifecycle-shape check. It lets classification read the authenticated claims of an expired
    token. It never grants access by itself.
  - `evaluate_lifecycle(&LifecycleContext, &AntiRollbackClock, StoredCredential, wall_now)`: the whole
    native pipeline (authenticate, reconcile clock, issue-time skew check, requirement check, classify,
    commit clock) returning a `LifecycleReport`. The skew check
    (`effective_now + 300 < iat` gives `Invalid`) runs on every path, including already-expired tokens,
    where the per-app code used to re-verify at `exp - 1` and bypass `verify_token`'s own skew test.
  - `LifecyclePolicy { grace_secs, lifetime_survives_rollback }`. Defaults are `0` and `false`, which
    reproduce what every shipping app does today (hard expiry, no rollback exemption).
- **Anti-rollback clock.**
  - `reconcile_clock(wall_now, monotonic_now, process_id, prior) -> ClockDecision`: the pure decision
    logic, ported unchanged from ViewRight/HeardRight `license_clock.rs`.
  - `AntiRollbackClock<S: ClockStore>` with `reconcile`, `commit` (throttled to one write per
    `CLOCK_WRITE_INTERVAL_SECS`) and `reanchor`. `reconcile` does not write; `commit` ignores rollback
    readings. A missing ledger on an initialised install is `ClockError::Missing`, never a silent reset.
  - Ledger safety (Astra read 2026-10-11): every observation is kept in memory for the life of the
    clock and only the durable write is throttled; load/decide/write is serialised across all clocks in
    the process and every write goes through the new `ClockStore::compare_and_save` (a **required**
    method with no default, see "Cross-process atomicity" below; `MemoryClockStore` implements it
    atomically), so a commit only moves the mark
    forward and is dropped if a newer server anchor landed, and `reanchor` cannot overwrite a newer
    `last_server_iat`. A successful `reanchor` also re-bases the clock's monotonic reference
    (`AntiRollbackClock::monotonic_now`) and replaces its memory. New: `ClockError::Contended`,
    `CLOCK_WRITE_ATTEMPTS`.
  - Cross-process atomicity (second Astra read 2026-10-11, finding 1). The first 0.2.0 draft gave
    `ClockStore::compare_and_save` a default (load, compare, save). That is not atomic against another
    process: A checks a ledger of 900, B saves 1100, A saves 1000, and a restart at 1001 accepts tokens
    that were already seen to expire. The default is removed; `compare_and_save` is now a required
    trait method. **This is the one source-level break against the unpublished draft**: a `ClockStore`
    that only has `load` and `save` no longer compiles (pinned by a `compile_fail` doctest on the
    trait). It is acceptable because 0.2.0 is unpublished; nothing published implements the trait.
    An implementation must make the comparison and the write one atomic step against every other
    writer in any process: a real compare-and-swap, or an OS lock held around read, compare and write.
  - `FileClockStore` (new, additive): a file-backed `ClockStore` that serialises every operation on an
    OS advisory lock of a sidecar `<ledger>.lock` file (`std::fs::File::lock`, shared for `load`,
    exclusive for writes) and writes through a flushed temporary file and an atomic rename, so it is
    atomic across processes. A damaged ledger file is `FileClockStoreError::Corrupt`, never "no ledger".
    `CrossProcessLock` (new, additive) is the same lock as a guard for hosts that keep the ledger in a
    keychain or credential entry and want to serialise their own `compare_and_save` with it.
  - `ClockStore` trait (apps persist the ledger where they already do), `MemoryClockStore` (tests and
    ephemeral QA builds only), `ProcessAnchor` (process id plus monotonic offset).
  - `ClockState::encode()` / `decode()` read and write the exact JSON record the apps already store
    (`{"v":1,"highest_seen":..,"trusted_process":"..","last_server_iat":..}`), and accept the original
    scalar form as `trusted_process = "legacy"`. Existing installs keep their ledger.
- **`TrustedClock` hook for authorization.** `trait TrustedClock { fn trusted_now(&self, wall_now,
  clock_was_initialized) -> Result<i64, ClockRefusal> }`, object safe, implemented by `SystemClock`
  (wall clock, today's behaviour) and by `AntiRollbackClock` (reconciled time, refuses on rollback).
  See "Adopting the hook in rightkit-tauri" below.
- **Refresh and reanchor.** `refresh_and_reanchor(&RefreshParams, &impl RefreshTransport,
  &AntiRollbackClock) -> Result<RefreshOutcome, ClockError<_>>`. The crate does no HTTP; the host
  implements `RefreshTransport`. Outcomes:
  - `Stale { stale_since, error }`: transport failure. Cached state is untouched; the caller stores
    `stale_since` (the first failure of the episode, passed back in via `RefreshParams::stale_since`).
  - `Revoked { message }`: the server answered with a non-`active` status.
  - `TokenReplaced { token, entitlement, lifecycle, reanchored, response }`: new token verified and the
    clock re-anchored on its `iat` and persisted before returning. The caller then persists the record
    (clock first, so an interrupted second write can only fail closed).
  - `TokenKept { reason, lifecycle, valid_until, response }`: the server said active and sent no token
    (or the one we hold). The cached token is kept only if it is still valid now, judged through the
    anti-rollback clock.
  - `TokenDropped { reason, response }`: same, but the cached token is absent or lapsed.
  - `Rejected(RefreshRejection)`: the server said active but the token is unusable (bad signature,
    wrong app/device, not active at its own `iat`, requirement unmet, or a replayed `iat`).
  `RefreshOutcome::as_str()` gives the JS names (`tokenReplaced`, `tokenKept`, `tokenDropped`, `stale`,
  `revoked`, `rejected`).
- **Device-lease envelope verification.** `verify_lease_envelope(envelope_json, pubkey) ->
  Result<LeaseEnvelope, LeaseError>` and `canonical_lease_envelope`. The signing input is compact JSON
  of seven camelCase fields in the order `leaseId, deviceIdHash, pool, issuedAt, expiresAt,
  graceSeconds, generation` (serde declaration order, byte-identical to ScrapeRight's
  `pro_bridge.rs::verify_lease_envelope_signature` and HeardRight's `license/lease.rs`), Ed25519
  non-strict `verify`, base64url signature. Test vector `test-vectors/lease-envelope-v1.json` was
  signed by Node `crypto`, independently of this crate. The nested `entitlement` is **not** covered by
  the envelope signature; it is returned as `unverified_entitlement` and must go through
  `verify_token` / `evaluate_lifecycle`. Lease expiry, device binding and generation ordering are the
  caller's to enforce. `verify_lease_envelope_fields(&LeaseEnvelope, signature, pubkey)` verifies
  already-parsed fields through the same canonicalisation function. **Legacy v1 token verification is not provided here and
  remains app-side migration code.**
- `VerifyError` now derives `Clone`.

### Dependency change: ed25519-dalek 2 to 3, base64 0.22 to 0.23 (gap ledger #77)

`rightkit-license` now links `ed25519-dalek` 3 and `base64` 0.23, matching the apps, so a product no
longer carries two dalek majors. No verification rule changed: signature checks still use plain
(non-strict) `verify`, public keys are still parsed with `VerifyingKey::from_bytes`, and
signatures with `Signature::from_slice`. Public types are unchanged.

The claim "2.x and 3.x accept and reject identical inputs" is pinned by data rather than by the
upstream changelog. `test-vectors/dalek-parity-v1.json` holds 111 cases (token and lease envelope)
whose expected outcomes were recorded on the 2.x build (ed25519-dalek 2.2.0, base64 0.22.1) before the
switch, by the ignored `record_dalek_parity_vectors` test. `tests/dalek_parity.rs` replays them on the
new dependencies and also checks that the case builder still yields byte-identical inputs (Ed25519
signing is deterministic, so this compares 3.x signing with 2.x signing). The cases cover:
non-canonical `S` (`S + L`, `S + 2L`, `S = L`, high bits), invalid, non-canonical and sign-flipped `R`,
wrong-length signatures, 14 public keys (identity, small-order, non-canonical `y`, "negative zero" `x`,
off-curve), an `R = identity, S = 0` signature against each key, and base64 padding, trailing bits,
wrong alphabet, whitespace, extra, empty and missing separators.

Recorded behaviour worth knowing (unchanged by the switch): verification is non-strict, so weak
public keys accept crafted signatures (for example the identity key accepts `R = identity, S = 0` for
any message). That is not reachable with the real compiled-in key, but a mis-embedded weak key would
verify forged tokens. Do not switch to `verify_strict` without a deliberate, versioned decision.

### Behaviour notes

- Re-anchoring may **lower** the high-water mark (`Reanchored::lowered`). That is the clock-repair path
  for a ledger inflated by a clock once set far ahead. It requires a strictly newer server `iat` than
  the last anchor (replay guard). See the README for the residual risk.
- `LifecyclePolicy::grace_secs > 0` makes native authorization accept an expired term/legacy/final
  token. It exists to mirror the JS client's `graceDays` and is off by default.

### Adopting the hook in rightkit-tauri

`rightkit-tauri` is not changed by this release. Its `AuthorizationService::authorize` currently calls
`current_unix_time` (wall clock). To make it consult the anti-rollback clock:

1. `Cargo.toml`: `rightkit-license = "0.2.0"` (this also keeps the workspace resolving once this crate
   is bumped to 0.2.0).
2. Add `clock: &'a dyn TrustedClock` to `AuthorizationService` (default `&SystemClock`) with a
   `with_clock(self, clock)` builder, and a defaulted `LicenseRecord::clock_initialized(&self) -> bool
   { false }`.
3. The call-site change is one line, after the record is read:
   `let now_unix = self.clock.trusted_now(current_unix_time()?, license.clock_initialized()).map_err(|e| AuthorizationError::Clock(e.to_string()))?;`
   in place of `let now_unix = clock().map_err(AuthorizationError::Clock)?;`.

With the default `SystemClock` nothing changes for existing callers.

### Not in this release

- No HTTP client, no keychain code (hosts supply `RefreshTransport` and, for a keychain-held ledger,
  a `ClockStore` whose `compare_and_save` they serialise across processes, for example with
  `CrossProcessLock`).
- No change to the token wire format or the signer.

## 0.1.3

- Lifecycle claims on `Entitlement` (`license_kind`, `entitlement_stage`, `license_issued_at`,
  `confirmation_due_at`), `is_final_lifetime`, and `VerifyError::InvalidLifecycle` for partial or
  inconsistent claims (git: `27b96e6e`, 2026-07-17).

## 0.1.2

- Requirement checks fail closed: unknown tiers, blank feature names and empty requirements never
  authorise (git: `31490a97`, 2026-07-14).

## 0.1.1

- First release in this repository: offline Ed25519 entitlement verification consolidated from the
  per-app clients (git: `d7599c16`, 2026-07-12).

Versions 0.1.1 to 0.1.3 are reconstructed from git history; crates.io publication dates were not
checked.
