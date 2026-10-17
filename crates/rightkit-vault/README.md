# rightkit-vault

Split-key sync for a small map of secrets (for example the user's own API keys per provider).
Drafted for bogusyogi/rightkit#163; depends on the entitlement endpoints
`GET /v1/vault/wrapping-key` and `POST /v1/vault/rotate` (bogusyogi/rightsites#81).

Status: written without a build or test run (standing order). The first CI run is the first
compile.

## Trust path (read this first)

The server half of the key comes from the entitlement service, so **the entitlement service is
in the trust path**. Orthic plus access to the blob (or Orthic plus a stolen Google ID token plus
the blob) is not enough to decrypt, because the client half never leaves the user's devices; but
a service that serves a wrong key, withholds the key, or erases the vault can deny access, and a
service that colludes with someone who also holds the blob **and** a device's client secret can
read it. That is acceptable for API keys the user can re-issue. It is not a design for
irreplaceable data. Treat the vault as a convenience copy of re-creatable secrets.

## Construction

Same primitives as `rightkit-sync`'s `cipher` module: ChaCha20-Poly1305 and HKDF-SHA256 from
`ring`, a random 16-byte salt per message from which a one-use key and nonce are derived,
length-prefixed associated data. No new crypto crates.

```text
dek      = 32 random bytes, fresh on every save
data     = salt_d || AEAD(HKDF(ikm = dek, salt = salt_d, info = data label), payload)
wrapped  = salt_w || AEAD(HKDF(ikm = server_key || client_secret, salt = salt_w,
                               info = wrap label || app || subject || key_id), dek)
```

- **Server key** (32 bytes): released by the service to the signed-in Google subject, per
  subject and per app, with a service-assigned `key_id`.
- **Client secret** (32 bytes): generated on the first device, stored by `rightkit-credentials`
  (Keychain / Credential Manager / Secret Service). Never sent to the service, never written to
  the cloud folder.
- **Combining**: the HKDF input keying material is `server_key || client_secret`. Neither half
  alone is the wrap key and neither alone decrypts (tests cover each half wrong in turn).
- **AAD** binds the format version, app and subject to both ciphertexts, and the `key_id` to
  the wrap, so a blob cannot be moved to another account or app, relabelled to another key id,
  or downgraded to another version. The payload AAD omits the key id so rotation can re-wrap
  without touching the payload.
- **Blob** (JSON, one small file in the user's cloud app folder): `v`, `app`, `subject`,
  `key_id`, `wrapped_dek`, `data` (base64). Secret names and values are inside `data`.

## Storage

The crate depends on a two-method trait, `BlobStore { read, write }`, not on a sync engine.
Implement it over one Drive `appDataFolder` file (the `rightkit-sync` `DriveBackend`, a single
file put) or a CloudKit record. `write` replaces the whole blob.

## Threat model

| Attacker holds | Result |
| --- | --- |
| Google (blob only) | Cannot decrypt: needs both halves. |
| Orthic (server key only) | Cannot decrypt: never sees the blob or the client secret. |
| Blob + stolen Google ID token | Can fetch the server key, still lacks the client secret: cannot decrypt. |
| Blob + Orthic collusion or breach | Lacks the client secret: cannot decrypt. |
| Blob + one device's OS credential store | Has the client secret and the cached server key: can decrypt. Same as any local secret. |
| Device with an unlocked keychain | Has both halves (see "Offline cache"). |
| Network attacker | TLS via `rightkit-http` (build the client `https_only`); the AEAD authenticates the blob end to end. |
| Blob tamper, or swap between users or apps | Fails authentication. |
| Rollback to an older blob | **Not detected.** |
| Concurrent saves from two devices | **Last writer wins** (a save verifies it can open the blob it replaces, so it cannot clobber a blob it cannot read, but two valid writers overwrite each other). |

Not protected: secrets in process memory after `load`. Values are `Secret` (zeroed on drop) and
the payload is decoded directly into them, but ring's HKDF state, serde_json's scratch buffer
for strings that contain escape sequences, and the HTTP client's response buffers are outside
this crate's reach.

## Multi-device and the client secret

A blob written on one device opens on another only if that device holds the same client secret.
Distribute it with the app's device-approval flow (for example `rightkit-sync-keys`' transfer
protocol, which carries a key over an authenticated channel) or derive it from an account key
the devices already share, then `CredentialsClientSecret::install_if_absent`. A device with a
blob but no client secret gets `Error::ClientSecretMissing`, and `save` refuses to overwrite:
it never mints a new secret on top of an existing blob. Losing every device's client secret
makes the blob unreadable; `Vault::reset` starts over.

## Rotation

- **Data key**: every `save` draws a new data key and new salts.
- **Server key**: the service starts returning a new `key_id` and lists still-valid old keys in
  `previous`. `Vault::rotate` fetches, unwraps the data key under the old key and client secret,
  and re-wraps it under the new key; the payload ciphertext is untouched. The service may retire
  an old key once clients have re-wrapped. A blob still under a retired key is
  `Error::KeyUnavailable` (recover with `reset`).
- **Client secret**: no rotation operation in 0.1.0. Re-keying means `reset` and re-entering the
  secrets.

## Pro lapse, account deletion, erase

| Service answer | Error | Meaning |
| --- | --- | --- |
| 401 | `Unauthorized` | The ID token is stale. Refresh it and retry. |
| 403 | `Denied` | Not entitled (Pro lapsed). The cached server key is cleared. The blob is untouched and decrypts again when entitlement returns and the service still holds the same key. |
| 410 `vault_revoked` | `Revoked` | The vault was erased (account deleted, or an explicit erase). The cached key is cleared. The old blob is unreadable **by design**, because the server half is gone. |

After `Revoked` the app asks the user, then calls `Vault::reset`: it POSTs `/v1/vault/rotate`
(the service re-initialises with a new key), discards the old blob, and saves a fresh empty
vault. The client secret is kept. `reset` is destructive; if its last write fails, call it
again. A lapse or erase is noticed at the next service contact: `refresh`, a cache miss, or
grace expiry.

## Offline cache

The server half is cached, unwrapped, in the OS credential store
(`CredentialsKeyCache`, one secret per app and subject), with the time the service last
confirmed it. Inside `offline_grace_secs` (default 7 days) `load` and `save` need no network
and do not call the ID-token closure. Past the grace with no service they fail closed with
`Error::Offline`. A cache timestamp in the future (clock set back) is not trusted.

Trade-offs, stated plainly:

- The cached key and the client secret sit in the same OS store. Anything that can read both
  on an unlocked device can decrypt without the service. The split protects against the cloud
  account, the service and a stolen ID token, not against a compromised device.
- A longer grace is a longer delay before a lapse or erase takes effect on an offline device.
  A shorter one means more "offline" failures. The app picks it.
- Without the cache (`sign_out`, or a keychain that cannot be written) every decrypt needs the
  service. A failed cache write does not fail the call; it only costs offline use.

## What the entitlement service logs (for rightsites#81)

Per request: timestamp, `app`, a hash of the subject (not the raw `sub`), `key_id` served,
status. Never the key, the ID token, or a response body. Rate-limit per subject. Rotation, erase
and reset events are audit records. The service never receives the blob, the data key, the
client secret or any ciphertext. This crate logs nothing; its errors never carry key material,
tokens or secret values, and never quote parser text.

## Assumed response shape

`{"key_id": "...", "key": "<base64, 32 bytes>", "previous": [{"key_id": "...", "key": "..."}]}`
for both `GET /v1/vault/wrapping-key` and `POST /v1/vault/rotate`, with
`Authorization: Bearer <google id token>` and `X-Right-App: <app>`. Not yet confirmed against
rightsites#81.

## Not in 0.1.0

Rollback detection, compare-and-swap on concurrent saves, client-secret rotation, a Drive
`BlobStore` (apps implement the trait over `DriveBackend`).
