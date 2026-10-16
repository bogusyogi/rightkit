# Changelog

## Unreleased

## 0.1.0

- Added (#144): `KeyManager` over a `rightkit_credentials::Store`: generate the account data key on the first device (`create_if_absent`), read the current key, `rotate` to a new version, `retire` old versions, `install` a transferred key. `DataKey` carries a version and a content-derived `KeyId`; per-zone keys come from HKDF-SHA256 (`DataKey::zone_key`). `KeyProvider` is the trait an encrypting sync backend takes; `ZoneKeys` implements it over a `KeyManager`. `account_scope` computes the engine's scope string from the key id.
- Added (#145): device-approval key transfer, protocol `rightkit-sync-keys-transfer/1`: the daemon's `rk-pair/1` short-code commit-reveal (local copy in `pairing_sas.rs`, to be replaced by a shared extraction) with an X25519 exchange, both public keys bound into the code and the transcript-bound HKDF, a 6-digit code both users type into the other device, and an XChaCha20-Poly1305 wrap of the data key. Typed one-shot state objects (`Requester`, `Approver`) make "wrap before the code is confirmed" unrepresentable. The relay sees only opaque messages.
- Unverified at release: written without a build or test run (standing order); first CI run is the first compile.
