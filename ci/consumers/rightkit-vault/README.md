# rightkit-vault 0.2.0 registry consumer (draft, PK13)

Standalone consumer, outside the workspace, pinning `rightkit-vault =0.2.0` exactly. It exercises
the sealed-blob family an app's vault calls: `Vault::new` over four in-memory stores with a fixed
clock, `load` of an absent blob, `save` then `load` of a one-entry secret map, and a `load` by a
different subject that must be refused. It also pins `rightkit-credentials =0.4.2`, because
`SecretMap` carries the `Secret` type from that crate.

State: in-memory stores, a fixed server key and client-secret fixture, and a fixed clock. No
network, no files, no real user data. Run only by the CI consumer lane.

Not covered: `HttpServerKeySource` and `rightkit-http`, the credentials-backed cache and client
secret, the Drive or CloudKit blob store, rotation, reset, sign-out, offline grace expiry, and
payloads with more than one entry. PK13 must be revisited if the vault API changes.
