# rightkit-license

Rust enforcement primitive for Right Suite license entitlements. The crate verifies a compact Ed25519-signed entitlement offline, checks its app and device binding, rejects expiry, and exposes native tier and feature requirements.

It is the security half of the licensing system. The JavaScript `@rightkit/license` package is the convenience half. Applications must enforce paid commands at the Rust boundary. Renderer state is never license authority.

Catalog capability: `licensing`. Published to crates.io (see [INDEX.md](../../INDEX.md)). Licensed under MIT or Apache-2.0, at your option.

## What it owns

- `verify_token(token, pubkey, expected_app, device_id, now_unix)`: verifies the signature with a 32-byte public key compiled into the app, then checks app, device and expiry. Returns an `Entitlement`, or a `VerifyError`.
- `Entitlement`: the verified fields, plus `satisfies(&LicenseRequirement)`.
- `LicenseRequirement`: tier and feature requirements (`pro()`, `all_features`, `any_feature`).
- `LicenseKind`, `EntitlementStage`: optional fields carried by the entitlement.

- `authenticate_token(...)`: the signature, app and device checks of `verify_token` without any time check, for reading the authenticated claims of an expired token. It never grants access by itself.
- `Lifecycle`, `Entitlement::lifecycle`, `evaluate_lifecycle`: the license state the UI keys on (`Trial`, `ExpiredTrial`, `Active`, `Grace`, `Expired`, `LifetimeProvisional`, `VerificationRequired`, `Lifetime`, `ClockRepairRequired`, `MigrationRequired`, `Revoked`, `Unlicensed`, `Invalid`), computed from an authenticated entitlement and a trusted clock.
- `AntiRollbackClock`, `ClockStore`, `reconcile_clock`, `TrustedClock`: a persisted high-water mark of trusted time that refuses to extend validity on a rolled-back clock.
- `refresh_and_reanchor`, `RefreshTransport`, `RefreshOutcome`: online refresh that re-anchors the clock on the server-signed issue time.
- `verify_lease_envelope`: device-lease envelope signature verification.

The token is minted by the backend signer (`rightapps` `packages/api/src/licensing/sign.ts`). Revocation propagates through the short `exp` and by re-fetching on refresh.

## When to use it

- Use it at the Rust command boundary of a paid feature, to check an entitlement offline.

Do not use it to issue or sign licences. Signing belongs to the backend signer.

## Cargo features

None. The crate has no `[features]` table. Dependencies: `ed25519-dalek` 2, `base64` 0.22, `serde`, `serde_json`.

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

- No ledger yet: trust `max(wall, monotonic)` and start the ledger there.
- Wall clock more than 300 seconds behind the high-water mark: rollback. `Lifecycle::ClockRepairRequired`, ledger untouched.
- A restarted process must see wall time strictly past the mark. Restarting at the same wall second cannot freeze expiry.
- Inside one process, a wall clock that falls behind keeps advancing on the monotonic offset instead.
- A ledger missing on an install whose record says it was initialised is an error (`ClockError::Missing`), not a fresh start.
- `reanchor` accepts a server `iat` only if it is strictly newer than the last anchor. It may lower the mark (`Reanchored::lowered`), which repairs a ledger inflated by a clock once set far ahead.

**Residual risk.** The replay guard is the last anchored `iat` only. A user who can intercept TLS on their own device could withhold a genuine refresh response, wind the clock back, and later deliver that older response to lower the mark. Closing it needs a freshness nonce in the signed token (a signer change). Until then re-anchoring is a convenience for honest clock errors, not a defence against a determined local attacker, as with any offline licence check.

#### Authorization hook

`TrustedClock` is the one-method trait an authorization layer consults instead of reading the wall clock. `SystemClock` is today's behaviour; `AntiRollbackClock` returns the reconciled time and refuses on rollback. See `CHANGELOG.md` for the `rightkit-tauri` adoption.

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

### Device-lease envelopes

`verify_lease_envelope(json, &public_key)` checks the envelope's own Ed25519 signature over compact JSON of `leaseId, deviceIdHash, pool, issuedAt, expiresAt, graceSeconds, generation` (non-strict `verify`, base64url signature). The nested `entitlement` is not signed by the envelope: it comes back as `unverified_entitlement` and must still pass `verify_token`. Legacy v1 token verification is not part of this crate and stays app-side migration code.

## Requirement contract

`Entitlement::satisfies` fails closed unless the entitlement tier is exactly `free`, `plus` or `pro`, and the requirement has at least one effective constraint. A requirement may name a recognised tier, a non-empty named feature, a non-empty `all_features` set, or a non-empty `any_features` set. Unknown or misspelled tiers, blank feature names, and default or empty requirements never authorise. Tier order is `free < plus < pro`.

## Wire contract

The token is:

```text
base64url(canonical_payload_json).base64url(ed25519_signature)
```

The canonical payload is compact JSON in this field order: `v`, `id`, `app`, `tier`, `features`, `device_id`, `iat`, `exp`. base64url is URL-safe with no padding (Node's `Buffer.toString("base64url")`). The public, non-secret cross-language fixture is `test-vectors/license-v2.json`. It must match the signer byte for byte.

## Platform support

Pure Rust. No platform-specific code.

## Tests

`tests/policy_contract.rs` (requirement rules), `tests/public_vectors.rs` (checks against `test-vectors/license-v2.json`), `tests/lifecycle.rs` (classification table, `evaluate_lifecycle`), `tests/clock.rs` (rollback, anchors, persistence, the `TrustedClock` hook), `tests/refresh.rs` (refresh outcomes against a fake server) and `tests/lease.rs` (lease envelope, with the Node-signed vector `test-vectors/lease-envelope-v1.json`).

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Changes: [CHANGELOG.md](CHANGELOG.md).

Use an exact crates.io version in product repositories. Do not use a Git or local path dependency after the Phase 2 migration gate.
