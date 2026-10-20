# rightkit-license 0.2.4 registry consumer (PK05)

Standalone consumer, outside the workspace, pinning `rightkit-license =0.2.4` exactly with default
features. It is pure and offline. It verifies the published `license-v2` vector with `verify_token`
and rejects a wrong app, a wrong device, an expired token, a malformed token and a tampered payload.
It verifies the published `lease-envelope-v1` vector with `verify_lease_envelope` and rejects a
changed generation. It checks that a fresh `FileClockStore` reads an absent ledger as `Ok(None)`. It
evaluates `NativeEngineEntitlements` over a native source with no credential, which must return an
`Unlicensed` report that grants no access.

The public keys and vectors are copied verbatim from the crate's `test-vectors/` directory. They are
public test material. The consumer does not depend on that directory existing in the registry
package.

State: a temp directory removed on exit. No network, no OS keychain, no real user data. Run only by
the CI consumer lane.

Not covered: the `client`, `update-auth`, `engine-gate` and `subject-entitlement` features, which
need HTTP, the credential store or a shell. Native records with a signed token are not exercised.
