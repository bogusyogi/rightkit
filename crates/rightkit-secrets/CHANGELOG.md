# Changelog

## 0.1.4

- **Credential-safety fixes (independent review of 0.1.4).**
  - macOS `set`: a failed update (`errSecAuthFailed`, read-only keychain) now returns that failure;
    it no longer deletes the existing item and re-adds (a failed add lost the credential).
  - Every `Backend` (macOS, Windows, memory, unavailable) validates service and account before any
    OS call and returns `Error::Invalid`; an empty account used to omit account filtering on macOS,
    so `delete` could remove the first item for the service. `Store` already validated.
  - `Secret::ct_eq` compares full lengths (a `u8`-truncated XOR made 32- and 288-byte secrets with
    a shared prefix equal, so migration could delete the original after a lossy copy).
  - macOS add requests the item reference and verifies it is in the intended keychain; an item the
    native add placed elsewhere (documented default-keychain fallback) is deleted and the call
    returns `Error::Unavailable`.
  - `apple_helper`: the secret is serialized into, and the reply read into, zeroed buffers; no
    `serde_json::Value` holds it. Still unwiped: serde_json's internal scratch buffer when the reply
    string contains JSON escapes, OS pipe buffers, and the helper process's own memory.
  - macOS default-keychain resolution: every failure is `Error::Unavailable`.

- **macOS: the system backend now targets the user-domain default keychain explicitly.** 0.1.3
  passed a null keychain to `SecKeychainFindGenericPassword`, which means the whole user keychain
  search list, so a duplicate service and account item in another keychain on the list could be
  read, modified (`set`) or deleted instead of the default keychain's own item. `get`, `exists`,
  `set` (including its replace-on-duplicate path), `add_if_absent` and `delete` now pass the
  default keychain returned by `SecKeychainCopyDomainDefault(kSecPreferencesDomainUser)`, resolved
  on every call (the same keychain the `keyring` crate's apple-native store used, so existing
  installs keep reading the same items). "Not found in the default keychain" is `Ok(None)` /
  `Ok(false)` even when another keychain holds a match. If the default keychain cannot be
  resolved the call returns `Error::Unavailable`; it never falls back to the search list. Item
  names, attributes and the data-protection / synchronizable `apple_helper` paths are unchanged.
  `KeychainBackend::open_file` (explicit keychain file) is unchanged. Add passes the same
  explicit keychain instead of null (`SecKeychainCopyDefault`); the target is identical for an
  ordinary user process and now cannot differ from the keychain `find` searches.

## 0.1.3

(0.1.2 was already on crates.io; the entries below ship as 0.1.3.)

- Dropped the `base64` dependency: minted DB keys (`get_or_create_key`) are encoded by a 15-line
  in-crate standard-base64 helper with RFC 4648 vector tests. Output is unchanged.

- **`WindowsTarget::KeyringCompat` is byte-compatible with the `keyring` crate's windows-native store.** Writes store UTF-8 text as UTF-16LE with no terminator (non-UTF-8 secrets stay raw bytes); reads decode UTF-16LE blobs back to UTF-8 and pass 0.1.x UTF-8 and binary blobs through. Existing keyring-written items no longer read back as NUL-interleaved bytes. The default `Slash` target is unchanged.
