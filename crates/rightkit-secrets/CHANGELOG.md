# Changelog

## 0.1.4

- **Second independent read (2026-10-11): nine fixes. Behaviour consumers will see.**
  - **Windows, `WindowsTarget::KeyringCompat` (HeardRight).** The blob encoding is now chosen by the
    namespace and never guessed: UTF-16LE text for reads and writes, byte-compatible with the
    `keyring` crate. The old heuristic read UTF-16LE text whose bytes were also valid UTF-8 as the
    wrong string (`"\u{4142}"` came back as `"BA"`). Consequences: a secret that is not valid UTF-8
    text can no longer be written in this layout (`Error::NotUtf8`), and a stored blob that is not
    valid UTF-16LE (odd length, unpaired surrogate) reads as `Error::NotUtf8` instead of passing
    through. Items that 0.1.1 and 0.1.2 stored as raw UTF-8 are read through the new
    `WindowsTarget::KeyringCompatLegacyUtf8` (same target name, raw bytes, read and delete only);
    migrate by reading through it and writing through `KeyringCompat`. Items written by the
    `keyring` crate and by 0.1.3+ are unaffected. `Namespace::try_windows_target` is new.
  - **Windows, target names.** A `WindowsTarget::Slash` prefix, and the service and account, must be
    non-empty, at most 250 bytes and free of control characters (NUL included). Windows ends a
    target name at the first NUL, so `prefix = "victim\0x"` addressed `victim` for any account and
    `delete` could remove an unrelated credential. Every `Backend` rejects such a namespace with
    `Error::Invalid` before any OS call, `Store` rejects it for every operation, and the new
    `Namespace::try_windows_target` rejects it at construction (`windows_target` keeps its
    signature and cannot fail, so it defers to the backend check).
  - **Windows, locking.** `set`, `delete`, `add_if_absent` and the new `delete_if_equals` now all run
    under one named mutex keyed on the normalised native target name (case-folded), held across the
    presence check and the write. Before, only `add_if_absent` locked, so a concurrent `set` between
    its check and its write was overwritten while it reported success, and two services sharing a
    prefix locked different names for the same entry. Reads take no lock.
  - **All platforms, timeouts.** A timed-out mutation keeps running inside the OS call. Mutations of
    one native credential are now serialised per target and the target stays claimed until the OS call
    returns: a retry arriving while a timed-out call is still running gets the new
    `Error::InProgress { op }` (counted by `is_unavailable_or_blocked`) instead of racing it and being
    overwritten later. A call still within its caller's deadline is waited for. `Error` gains this
    variant; code that matches it exhaustively needs an arm.
  - **All platforms, migration.** `migrate_between` (and `migrate_legacy_namespaces`) no longer delete
    the source unconditionally. It deletes only through the new `Backend::delete_if_equals` /
    `Store::delete_if_equals`: remove the item only if it still holds the value that was copied.
    The memory and Windows backends implement it atomically; the trait default is
    `ConditionalDelete::Unsupported`, which removes nothing. **On macOS the source is now kept** (the
    keychain has no compare-and-delete), so PostRight and Pulse migrations copy and verify but leave
    the legacy item in place; the new `MigrationReport::retained` lists those accounts and `moved`
    lists only accounts whose source was actually removed. A source replaced between the read and the
    delete is retained instead of destroyed.
  - **macOS, where a write can land.** Apple's creation calls do not fail closed for an explicit
    keychain. Source-derived, not run: `SecKeychainAddGenericPassword`, and `SecItemAdd` with
    `kSecUseKeychain` (it delegates to `SecKeychainItemCreateFromContent`), catch every failure to use
    the requested keychain and retry through the default keychain. So the post-write relocation check
    (request the item, compare its keychain, delete it) is removed (a crash or a failed delete left the
    secret in the wrong keychain) and the guarantee is made before the write: the destination must be
    a valid reference whose file exists and is unlocked (locked and non-interactive is
    `Error::InteractionRequired`), and a write is only made where the fallback destination is the
    requested one. The user-domain default target (what apps use) qualifies. Any other explicit
    keychain opened with `KeychainBackend::open_file` is read and delete only: a write is
    `Error::Unavailable` unless the keychain is the current default. Throwaway test keychains
    (`IsolatedKeychain`, and the new `KeychainBackend::open_isolated_file`, which accepts only a file
    under the temp directory) can still be written. The CLI's `--isolated-keychain` uses
    `open_isolated_file`.
  - **`apple_helper`, secret copies.** The `value` of a `read` reply is decoded by a small in-crate
    JSON string unescaper straight from the zeroised reply line into one pre-sized `Zeroizing` buffer
    (no reallocation, wiped on every error path), instead of through serde_json, whose scratch buffer
    for escaped strings was freed unwiped. A regression test uses a counting allocator to prove no
    freed block still holds the decoded secret.
  - **`apple_helper`, deadline.** `AppleHelper::timeout` is now one deadline over writing the request,
    reading the reply and the helper exiting. The request is written on its own thread, so a helper
    that never reads stdin no longer blocks the call; the helper is killed and reaped on every path,
    so one that closes stdout but stays alive no longer blocks the final wait.

- **Credential-safety fixes (independent review of 0.1.4).**
  - macOS `set`: a failed update (`errSecAuthFailed`, read-only keychain) now returns that failure;
    it no longer deletes the existing item and re-adds (a failed add lost the credential).
  - Every `Backend` (macOS, Windows, memory, unavailable) validates service and account before any
    OS call and returns `Error::Invalid`; an empty account used to omit account filtering on macOS,
    so `delete` could remove the first item for the service. `Store` already validated.
  - `Secret::ct_eq` compares full lengths (a `u8`-truncated XOR made 32- and 288-byte secrets with
    a shared prefix equal, so migration could delete the original after a lossy copy).
  - macOS add verified the created item's keychain after the write and deleted it if misplaced.
    Superseded by the second read above: that check is removed and the destination is gated before
    the write.
  - `apple_helper`: the secret is serialized into, and the reply read into, zeroed buffers; no
    `serde_json::Value` holds it. Still unwiped: OS pipe buffers and the helper process's own memory
    (serde_json's scratch buffer was fixed by the second read above).
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
