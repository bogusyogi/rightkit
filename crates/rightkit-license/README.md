# rightkit-license

Rust enforcement primitive for Right Suite license entitlements. The crate verifies a compact Ed25519-signed entitlement offline, checks its app and device binding, rejects expiry, and exposes native tier and feature requirements.

It is the security half of the licensing system. The JavaScript `@rightkit/license` package is the convenience half. Applications must enforce paid commands at the Rust boundary. Renderer state is never license authority.

Catalog capability: `licensing`. crates.io has 0.1.3; the source is 0.2.2, which is unpublished (see [INDEX.md](../../INDEX.md) and CHANGELOG 0.2.0). Licensed under MIT or Apache-2.0, at your option.

## What it owns

- `verify_token(token, pubkey, expected_app, device_id, now_unix)`: verifies the signature with a 32-byte public key compiled into the app, then checks app, device and expiry. Returns an `Entitlement`, or a `VerifyError`.
- `Entitlement`: the verified fields, plus `satisfies(&LicenseRequirement)`.
- `LicenseRequirement`: tier and feature requirements (`pro()`, `all_features`, `any_feature`).
- `LicenseKind`, `EntitlementStage`: optional fields carried by the entitlement.

- `authenticate_token(...)`: the signature, app and device checks of `verify_token` without any time check, for reading the authenticated claims of an expired token. It never grants access by itself.
- `Lifecycle`, `Entitlement::lifecycle`, `evaluate_lifecycle`: the license state the UI keys on (`Trial`, `ExpiredTrial`, `Active`, `Grace`, `Expired`, `LifetimeProvisional`, `VerificationRequired`, `Lifetime`, `ClockRepairRequired`, `MigrationRequired`, `Revoked`, `Unlicensed`, `Invalid`), computed from an authenticated entitlement and a trusted clock.
- `AntiRollbackClock`, `ClockStore`, `reconcile_clock`, `TrustedClock`: a persisted high-water mark of trusted time that refuses to extend validity on a rolled-back clock.
- `FileClockStore`, `CrossProcessLock`: a file-backed `ClockStore` that is atomic across processes, and the OS lock it uses, for hosts that keep the ledger elsewhere.
- `refresh_and_reanchor`, `RefreshTransport`, `RefreshOutcome`: online refresh that re-anchors the clock on the server-signed issue time.
- `verify_lease_envelope`: device-lease envelope signature verification.

The token is minted by the backend signer outside this repository (`rightapps` `packages/api/src/licensing/sign.ts`; not checked in this audit). Revocation propagates through the short `exp` and by re-fetching on refresh.

## When to use it

- Use it at the Rust command boundary of a paid feature, to check an entitlement offline.

Do not use it to issue or sign licences. Signing belongs to the backend signer.

## Cargo features

`subject-entitlement` (Google-subject entitlement client, adds `rightkit-http`) and `engine-gate` (engine startup gate: Allowed / Grace / Denied verdict, exit codes, messages; implies `subject-entitlement`; see CHANGELOG 0.2.2). Both are off by default. Dependencies: `ed25519-dalek` 3, `base64` 0.23, `serde`, `serde_json` (0.2.0 moved from dalek 2 and base64 0.22; verification semantics are pinned by `test-vectors/dalek-parity-v1.json`, see `CHANGELOG.md`).

## Usage

Taken from `tests/policy_contract.rs` and the crate docs. `token` is the string from the app's licence store, and `public_key` is the 32-byte key compiled into the binary:

```rust
use rightkit_license::{verify_token, LicenseRequirement};

let entitlement = verify_token(token, &public_key, "heardright", device_id, now_unix)?;
assert!(entitlement.satisfies(&LicenseRequirement::pro()));
```

### Lifecycle with an anti-rollback clock

The host implements `ClockStore` over the keychain entry it already uses, creates one `ProcessAnchor` per process, and asks for the lifecycle at the point of use:

```rust
use rightkit_license::{
    evaluate_lifecycle, AntiRollbackClock, LicenseRequirement, LifecycleContext,
    ProcessAnchor, StoredCredential,
};

let wall_now = rightkit_license::system_now_unix()?;
let clock = AntiRollbackClock::new(my_clock_store, ProcessAnchor::new(wall_now)); // once per process
let pro = LicenseRequirement::pro();
let ctx = LifecycleContext::new(&public_key, "heardright", device_id).with_requirement(&pro);

let report = evaluate_lifecycle(
    &ctx,
    &clock,
    StoredCredential::Signed { token, clock_initialized: record.clock_initialized },
    wall_now,
)?;
if report.clock_initialized && !record.clock_initialized {
    record.clock_initialized = true; // persist: deleting the ledger later is then detected
}
if report.lifecycle.needs_online_refresh() { /* offer refresh_and_reanchor */ }
if report.grants_access() { /* unlock */ }
```

`ClockState::encode()` / `decode()` use the JSON record ViewRight and HeardRight already store, so adoption does not reset existing ledgers.

#### Clock rules

- No ledger yet: trust `max(wall, monotonic)` and start the ledger there. The first server anchor on a fresh install does the same: the ledger starts at `max(wall, iat)`, never at `iat` alone (see the `plan_reanchor` docs: a valid but old token must not let the clock be set back to its era; a first run on a clock wrong far into the future is repaired by the next online refresh, which lowers the mark to its newer `iat`).
- Wall clock more than 300 seconds behind the high-water mark: rollback. `Lifecycle::ClockRepairRequired`, ledger untouched.
- A restarted process must see wall time strictly past the mark. Restarting at the same wall second cannot freeze expiry.
- Inside one process, a wall clock that falls behind keeps advancing on the monotonic offset instead.
- A ledger missing on an install whose record says it was initialised is an error (`ClockError::Missing`), not a fresh start.
- `reanchor` accepts a server `iat` only if it is strictly newer than the last anchor, checked against the ledger at write time. It may lower the mark (`Reanchored::lowered`), which repairs a ledger inflated by a clock once set far ahead, and it also re-bases the clock's monotonic reference on the new mark so the old, too-far-ahead process anchor cannot pull time back up. It is never called on unauthenticated time: the caller passes the `iat` of a token it has already verified (`refresh_and_reanchor` does).
- Every observation is kept in memory for the life of the clock; only the durable write is throttled (`CLOCK_WRITE_INTERVAL_SECS`). Writes are serialised across all clocks in the process and go through `ClockStore::compare_and_save`, so a stale writer can only move the mark forward and cannot overwrite a newer `last_server_iat`.
- `ClockStore::compare_and_save` is a **required** method with no default. A load followed by a save is not atomic against another process (A checks 900, B saves 1100, A saves 1000, and trusted time has moved backwards), and the in-process lock cannot cover another process. An implementation must make the comparison and the write one atomic step against every writer in any process: a real compare-and-swap, or an OS lock around read, compare and write. `FileClockStore` does this for a ledger kept in a file (OS file lock on `<ledger>.lock`, flushed temporary file, atomic rename). A host that keeps the ledger in a keychain or credential entry implements `ClockStore` over it and takes a `CrossProcessLock` on a lock file shared by all its processes inside `compare_and_save`. `MemoryClockStore` is atomic but process-local, for tests only.
- `ClosureClockStore::new(lock_path, get, set)` is that keychain case packaged: `get` returns `Result<Option<ClockState>, E>`, `set` writes a `&ClockState`, and every operation runs under the OS lock of `lock_path` (shared for `load`, exclusive for writes, with `compare_and_save` doing `get`, compare and `set` under one lock). Every process of the app must pass the same lock path. Errors are `ClosureClockStoreError::{Lock, Store}`; a failed `get` is an error, never an empty ledger.
- `plan_reanchor(prior, iat, wall_now, was_initialized)` is the pure re-anchor decision `AntiRollbackClock::reanchor` writes (`ReanchorPlan`, or `AnchorRefusal::{Missing, Replayed}`), for previews and tests with no store.

**Residual risk.** The replay guard is the last anchored `iat` only. A user who can intercept TLS on their own device could withhold a genuine refresh response, wind the clock back, and later deliver that older response to lower the mark. Closing it needs a freshness nonce in the signed token (a signer change). Until then re-anchoring is a convenience for honest clock errors, not a defence against a determined local attacker, as with any offline licence check.

#### Authorization hook

`TrustedClock` is the one-method trait an authorization layer consults instead of reading the wall clock. `SystemClock` is today's behaviour; `AntiRollbackClock` returns the reconciled time and refuses on rollback. See `CHANGELOG.md` for the `rightkit-device` adoption.

### Refresh and reanchor

```rust
let params = RefreshParams { cached_token: Some(token), clock_initialized: true, ..RefreshParams::new(&public_key, "heardright", device_id, activation_token, wall_now) };
match refresh_and_reanchor(&params, &my_transport, &clock)? {
    RefreshOutcome::TokenReplaced { token, response, .. } => { /* persist record, clock already saved */ }
    RefreshOutcome::TokenKept { .. } => {}
    RefreshOutcome::TokenDropped { .. } => { /* drop cached token, ask to re-activate */ }
    RefreshOutcome::Stale { stale_since, .. } => { /* keep record, store stale_since */ }
    RefreshOutcome::Revoked { .. } => { /* delete token, show Revoked */ }
    RefreshOutcome::Rejected(reason) => { /* keep record, surface reason */ }
    _ => {}
}
```

`RefreshTransport::refresh` must return `Ok` for any authoritative answer (including an HTTP 4xx whose body is `ok:false` with a non-`active` status) and `Err` for no network, 5xx or an unparsable body. Persist the clock before the record. See the module docs in `src/refresh.rs` for the full outcome table.

`judge_refresh_response(&params, &response, prior_ledger)` is the same judgement with no I/O and no clock (`RefreshJudgement`: `Revoked`, `Rejected { rejection, token }`, `ReplaceToken { .., anchor }`, `NoNewToken`, `LedgerMissing`); `refresh_and_reanchor` calls it internally, so the two cannot drift. `Rejected { token: Some(RejectedToken { iat, exp, lifecycle }) }` carries the claims of an authentic token that was refused (for `NotActive`: "expired at .."); `RefreshOutcome::Rejected(RefreshRejection::NotActive(..))` is unchanged, so judge the same response to get them. The cached-token keep-or-drop verdict needs the live clock and stays in `refresh_and_reanchor`.

#### Message texts

`LifecycleContext::with_messages(LifecycleMessages)` replaces any of the report's user-facing `message` strings (`revoked`, `migration_required`, `unsigned`, `invalid_token`, `clock_repair_required`, `issued_in_the_future`, `expired`, `invalid_lifecycle`); unset fields keep the built-in English text exactly as before. `Lifecycle` itself remains the machine-readable verdict.

### Device-lease envelopes

`verify_lease_envelope(json, &public_key)` checks the envelope's own Ed25519 signature over compact JSON of `leaseId, deviceIdHash, pool, issuedAt, expiresAt, graceSeconds, generation` (non-strict `verify`, base64url signature). `verify_lease_envelope_fields(&envelope, signature, &key)` checks already-parsed fields through the same canonicalisation. The nested `entitlement` is not signed by the envelope: it comes back as `unverified_entitlement` and must still pass `verify_token`. Lease expiry, device binding and generation ordering are the caller's to enforce; a verifying signature does not check them. Legacy v1 token verification is not part of this crate and stays app-side migration code.

## Requirement contract

`Entitlement::satisfies` fails closed unless the entitlement tier is exactly `free`, `plus` or `pro`, and the requirement has at least one effective constraint. A requirement may name a recognised tier, a non-empty named feature, a non-empty `all_features` set, or a non-empty `any_features` set. Unknown or misspelled tiers, blank feature names, and default or empty requirements never authorise. Tier order is `free < plus < pro`.

## Wire contract

The token is:

```text
base64url(canonical_payload_json).base64url(ed25519_signature)
```

The canonical payload is compact JSON in this field order: `v`, `id`, `app`, `tier`, `features`, `device_id`, `iat`, `exp`. base64url is URL-safe with no padding (Node's `Buffer.toString("base64url")`). The public, non-secret cross-language fixture is `test-vectors/license-v2.json`. It must match the signer byte for byte.

## Platform support

Pure Rust, standard library only. `FileClockStore` and `CrossProcessLock` use `std::fs::File::lock` (an OS advisory lock: `flock` on Unix, `LockFileEx` on Windows) and, on Unix, flush the parent directory after a rename.

## Tests

`tests/policy_contract.rs` (requirement rules), `tests/public_vectors.rs` (checks against `test-vectors/license-v2.json`), `tests/lifecycle.rs` (classification table, `evaluate_lifecycle`), `tests/clock.rs` (rollback, anchors, persistence, the `TrustedClock` hook), `tests/file_store.rs` (the cross-process file ledger and lock), `tests/refresh.rs` (refresh outcomes against a fake server) `tests/lease.rs` (lease envelope, with the Node-signed vector `test-vectors/lease-envelope-v1.json`) and `tests/dalek_parity.rs` (111 token and lease-envelope cases whose outcomes were recorded on ed25519-dalek 2.2.0 / base64 0.22.1 and are asserted on the current dependencies).

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Changes: [CHANGELOG.md](CHANGELOG.md).

Use an exact crates.io version in product repositories. Do not use a Git or local path dependency after the Phase 2 migration gate.
