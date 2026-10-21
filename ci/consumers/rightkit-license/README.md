# rightkit-license 0.2.4 registry consumer (PK05)

Standalone consumer, outside the workspace, pinning `rightkit-license =0.2.4` exactly and enabling
`client`. It is pure and offline. It verifies the published `license-v2` vector with `verify_token`
and rejects a wrong app, a wrong device, an expired token, a malformed token and a tampered payload.
It verifies the published `lease-envelope-v1` vector with `verify_lease_envelope` and rejects a
changed generation. It checks that a fresh `FileClockStore` reads an absent ledger as `Ok(None)`. It
evaluates `NativeEngineEntitlements` over a native source with no credential, which must return an
`Unlicensed` report that grants no access. It constructs `LicenseClient::new(LicenseSetup::Disabled, ...)`
with a synthetic device adapter and checks that `status()` returns disabled mode and no status.

The public keys and vectors are copied verbatim from the crate's `test-vectors/` directory. They are
public test material. The consumer does not depend on that directory existing in the registry
package.

State: a temp directory removed on exit. Disabled client path uses no network, OS keychain or real
user data. Run only by the CI consumer lane.

Evidence limit: this compiles the published `client` feature & exercises only its explicitly disabled
status path. Configured client operations, credential persistence, network behavior, `update-auth`,
`engine-gate`, `subject-entitlement`, and native records with a signed token are not exercised.
