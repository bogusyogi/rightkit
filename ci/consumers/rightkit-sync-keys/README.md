# rightkit-sync-keys 0.1.0 registry consumer (draft, PK22)

Standalone consumer, outside the workspace, pinning `rightkit-sync-keys =0.1.0` exactly. It exercises
the in-memory families: `DataKey::from_bytes`, `zone_key`, `account_scope`, `StaticKeys` through the
`KeyProvider` trait, and the device-approval transfer (`Requester` and `Approver`) run end to end in
one process. A wrong typed code is refused with `SasMismatch`, and a spent session is refused.

State: in-process only. Key bytes are fixed constants. The transfer's nonces and ephemeral keys come
from the OS random source, so the six-digit code differs per run; every assertion holds for any code.
No keychain, no files, no network, no real user data. The `zeroize` dependency is named directly
because `DataKey::from_bytes` takes a `Zeroizing` value. Run only by the CI consumer lane.

Not covered: `KeyManager` (create, rotate, install, retire, forget) and `ZoneKeys`, which need the OS
credential store through `rightkit-credentials`; the relay's session expiry and rate limiting; the
recovery-phrase wrap, which is not in this crate; the other protocol error paths. PK22-2 may remove
families that no consumer names, so this recipe must be revisited if the list changes.
