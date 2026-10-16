# rightkit-sync-keys

End-to-end sync key management for Right Suite apps. The server only ever sees ciphertext and
opaque relay blobs.

## Key manager (#144)

```rust
let store = rightkit_credentials::Store::open(Config::new(Namespace::new("app.example.myapp")?));
let keys = KeyManager::new(store, account_id)?;
let (data_key, created) = keys.create_if_absent()?;   // first device generates; others adopt
let rotated = keys.rotate()?;                          // version + 1, new key id, new scope
```

* The account `DataKey` is 32 random bytes with a version and a content-derived `KeyId`
  (`k` + 16 hex, an HKDF output, safe to store server-side).
* Storage is `rightkit-credentials` only: `sync-keys/<account>/current` and
  `sync-keys/<account>/v<N>`, create-only writes, never minted over an unreadable key.
* `DataKey::zone_key(app, zone)` is HKDF-SHA256 with a length-prefixed label, so zones are
  unrelated keys. `account_scope(app, zone, account, key_id)` is the engine scope string; a
  rotation or reset changes it, so the sync engine resets its ledger.
* Rotation keeps older versions until `retire`. Other devices do not get the new key by
  themselves: approve each one again with the transfer below.

## Connecting to `rightkit-sync`

`KeyProvider` (`current_key()`, `key_by_id()`) is what an encrypting backend needs. No crate
depends on the other; a small adapter bridges them (see the `provider` module docs). `ZoneKeys`
implements it over a `KeyManager`; `StaticKeys` is for tests. Seal the key id next to the
ciphertext and in the associated data; treat an unknown id as an undecodable record.

## Device-approval transfer (#145)

Protocol `rightkit-sync-keys-transfer/1`, four opaque messages through any relay:

1. `Requester::begin` -> **Begin** (X25519 public key, commitment, device label)
2. `Approver::receive_begin` -> **Offer** (X25519 public key, nonce)
3. `Requester::receive_offer` -> **Reveal** (nonce); `Approver::receive_reveal` checks the commitment
4. Both screens show a 6-digit code. The user types each screen's code into the *other* device.
   `Approver::approve(typed, &key)` -> **Transfer**; `Requester::accept(typed, &transfer)` -> `DataKey`.

The code is the `rk-pair/1` SAS of `rightkit-daemon` (commit-reveal, 1-in-10^6 per session,
3 tries) with both X25519 public keys, the app and the account bound in. The key wrap is
XChaCha20-Poly1305 under an HKDF key from the X25519 secret, salted and authenticated with the
transcript hash. Neither side can wrap or unwrap without the code check. See `src/transfer.rs`
for the security argument and its limits (the relay must expire sessions).

`src/pairing_sas.rs` is a local copy of the transport-free `rk-pair/1` functions and should be
replaced by a shared crate (see its header).

## Not included

Recovery-phrase (Argon2id) wrapping from the design, and the relay endpoints.
