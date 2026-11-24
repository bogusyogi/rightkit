# Changelog

## Unreleased

- Wipe on revoke (Y3): `issue_revoke_notice` (called before `rotate`, opaque bytes for any channel) and `RevokeWipe` (`resume` at every start, `handle_notice` per notice, `is_pending`) with the host-implemented `LocalCopy`. A good notice for this device writes a durable pending mark, erases every registered copy, destroys all key versions, and clears the mark; a crash is finished by `resume`. New `Error::WipeFailed`. Limits are in the `revoke` module notes: a device that is off or never reconnects keeps its copy; a deadman wipe is not built. Written, not compiled. No unit tests, per the 2026-11-03 rule.
- `KeyProvider::key_by_version(u32)` (provided, so existing implementors still compile; the default returns an error, not `None`), implemented by `ZoneKeys` and `StaticKeys`: `rightkit-sync`'s cipher seals a key version and needs the lookup by it. The adapter example in `provider.rs` now targets `rightkit_sync::cipher::VersionedKeyProvider`. `account_scope` documents its twin in `rightkit-sync` (`scope::account_scope`). Written, not compiled. No unit tests, per the 2026-11-03 rule.
## 0.1.0 (2026-10-17)

- The local `pairing_sas.rs` copy of `rk-pair/1` is deleted (#965); `transfer` uses `rightkit_identity::pairing` (`pairing_commit`, `pairing_code`, `label_bytes`, the three constants). New dependency `rightkit-identity` 0.1.1. Behaviour unchanged (same bytes). Not compiled here.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0

- Added (#144): `KeyManager` over a `rightkit_credentials::Store`: generate the account data key on the first device (`create_if_absent`), read the current key, `rotate` to a new version, `retire` old versions, `install` a transferred key. `DataKey` carries a version and a content-derived `KeyId`; per-zone keys come from HKDF-SHA256 (`DataKey::zone_key`). `KeyProvider` is the trait an encrypting sync backend takes; `ZoneKeys` implements it over a `KeyManager`. `account_scope` computes the engine's scope string from the key id.
- Added (#145): device-approval key transfer, protocol `rightkit-sync-keys-transfer/1`: the daemon's `rk-pair/1` short-code commit-reveal (local copy in `pairing_sas.rs`, to be replaced by a shared extraction) with an X25519 exchange, both public keys bound into the code and the transcript-bound HKDF, a 6-digit code both users type into the other device, and an XChaCha20-Poly1305 wrap of the data key. Typed one-shot state objects (`Requester`, `Approver`) make "wrap before the code is confirmed" unrepresentable. The relay sees only opaque messages.
- Unverified at release: written without a build or test run (standing order); first CI run is the first compile.
