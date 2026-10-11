# Changelog

## 0.2.0

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
