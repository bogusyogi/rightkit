# rightkit-license

Rust enforcement primitive for Right Suite license entitlements.

The crate verifies a compact Ed25519-signed entitlement token offline, checks
its app and device binding, rejects expiry, and exposes native tier/feature
requirements. Applications must enforce paid commands at the Rust boundary;
renderer state is never license authority.

## Wire contract

The token is:

```text
base64url(canonical_payload_json).base64url(ed25519_signature)
```

The signed compact JSON field order is `v`, `id`, `app`, `tier`, `features`,
`device_id`, `iat`, `exp`. The public, non-secret cross-language fixture is
shipped in `test-vectors/license-v2.json`.

## Usage

```rust
use rightkit_license::{verify_token, LicenseRequirement};

let entitlement = verify_token(token, public_key, "heardright", device_id, now_unix)?;
assert!(entitlement.satisfies(&LicenseRequirement::pro()));
# Ok::<(), rightkit_license::VerifyError>(())
```

Use an exact crates.io version in product repositories. Do not use Git or local
path dependencies after the Phase 2 migration gate.

Licensed under either MIT or Apache-2.0, at your option.
