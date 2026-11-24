# rightkit-sync-keys 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-sync-keys =0.1.0` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed assertion.

## What it proves

- `DataKey::generate`: the version asked for, version 0 refused, a key id of `k` and 16 lower-case hex characters, two random keys with different ids.
- `KeyId::parse`: a well-formed id round-trips through `as_str` and `Display`; wrong length, wrong prefix, upper case and non-hex are refused.
- `DataKey::zone_key`: deterministic, a different key for another zone, app or data key, never the data key itself, and carrying the data key's version and id.
- `account_scope`: deterministic, 64 lower-case hex characters, and changed by each of app, zone, account id and key id.
- `StaticKeys` as a `KeyProvider`: the highest version is current, `key_by_id` and `key_by_version` find a held key, and an unheld id or version is `None`; an empty set is not enrolled.

## What it deliberately does not prove

- `KeyManager` and `ZoneKeys`: they keep the key in the OS credential store, which a registry consumer must not write to.
- The device-approval key transfer, wipe on revoke, and the `sync` feature (`SyncKeys`).
- Anything about a particular OS.

State: in-memory values only; no network, no files, no process, no user data.
