# rightkit-license

Rust enforcement primitive for Right Suite license entitlements. The crate verifies a compact Ed25519-signed entitlement offline, checks its app and device binding, rejects expiry, and exposes native tier and feature requirements.

It is the security half of the licensing system. The JavaScript `@rightkit/license` package is the convenience half. Applications must enforce paid commands at the Rust boundary. Renderer state is never license authority.

Catalog capability: `licensing`. Published to crates.io (see [INDEX.md](../../INDEX.md)). Licensed under MIT or Apache-2.0, at your option.

## What it owns

- `verify_token(token, pubkey, expected_app, device_id, now_unix)`: verifies the signature with a 32-byte public key compiled into the app, then checks app, device and expiry. Returns an `Entitlement`, or a `VerifyError`.
- `Entitlement`: the verified fields, plus `satisfies(&LicenseRequirement)`.
- `LicenseRequirement`: tier and feature requirements (`pro()`, `all_features`, `any_feature`).
- `LicenseKind`, `EntitlementStage`: optional fields carried by the entitlement.

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

`tests/policy_contract.rs` (requirement rules) and `tests/public_vectors.rs` (checks against `test-vectors/license-v2.json`).

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.

Use an exact crates.io version in product repositories. Do not use a Git or local path dependency after the Phase 2 migration gate.
