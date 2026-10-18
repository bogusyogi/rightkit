# rightkit-credentials

Per-app OS credential storage for Right Suite applications. It provides macOS Keychain, Windows Credential Manager and Linux Secret Service access behind one `Store`, with a per-app `Namespace`. It also provides passive presence checks (`Store::exists` never requests secret bytes or prompts), non-interactive runtime behavior by default, test and development guards, DB-key provisioning (`Store::get_or_create_key`), and migration of legacy items (`migrate_between`).

**Use it** to store per-app secrets such as API tokens, refresh tokens, and database keys in the OS credential store.

**Do not use it** for secrets that must be shared between apps (use the per-app namespace) or for non-secret settings (use `rightkit-app-settings`, which holds only `SecretRef`s).

## Cleared versions

On macOS, use `rightkit-credentials` 0.2.0 or newer. `rightkit-secrets` 0.1.3 and earlier (the former name of this crate) used the keychain search list and are not cleared for storing secrets; see the 0.2.0 entry in the changelog. Apps still on `rightkit-secrets` rename the dependency and import path.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `settings-probe` | off | `rightkit-app-settings` | Enables the `rightkit-app-settings` presence adapter (`SecretProbe`). |
| `isolated-test-keychain` | off | none | TEST ONLY. Compiles the throwaway macOS test keychain (`IsolatedKeychain`, `KeychainBackend::open_isolated_file`) and the CLI's `--isolated-keychain`. Without it there is no write path to an explicit keychain. Enable it only as `cargo test --features isolated-test-keychain` for `tests/e2e.rs`; never in a shipped build. The crate has no `default` feature, no other feature enables it, and no dependent in this repository names it; only `ci/gate.config.json` passes it, to `cargo test -p rightkit-credentials`. |

## Platform support

- macOS: the user-domain default keychain (explicit on every call; the search list is never used), with the `apple_helper` module driving the synced-credentials Swift helper (`RightKitAppleCredentials`). Apple's creation calls fall back to the default keychain when an explicit keychain is unusable, so a write is gated before it is made: the destination must be valid, present on disk and unlocked, and an explicit keychain from `KeychainBackend::open_file` is read and delete only, whether or not it is the current default (`Error::Unavailable` on write). Throwaway test keychains (`IsolatedKeychain`, `KeychainBackend::open_isolated_file`, temp directory only) can be written only in this crate's own tests and under the `isolated-test-keychain` feature; a production build has no write path to an explicit keychain. The mutation-exclusion identity of a macOS credential is its keychain file plus Keychain service and account (`macos_credential_identity`); a `WindowsTarget` prefix plays no part in it. A mutation resolves the default keychain exactly once (`Backend::pinned`): `Store` takes the exclusion identity from that one resolved keychain and makes the write on it, so the two cannot differ if the default is switched while an earlier call is stalled; a default that cannot be resolved or identified refuses the mutation before anything is locked or written. See `src/macos.rs`.
- Windows: Credential Manager (`src/windows.rs`).
- Linux: the Secret Service on the user's session bus (GNOME Keyring, KWallet, KeePassXC) through `secret-service` 5.2 (`src/linux.rs`). Items use the `keyring-core` layout (`service` and `username` attributes, default collection, raw bytes), so items written by `keyring-core` with `zbus-secret-service-keyring-store` stay readable and the reverse. It fails closed: no session bus, provider or default collection is `Error::Unavailable`; a locked item or collection is `Error::InteractionRequired` unless the call is `Interaction::Interactive`; more than one match for one `service` and `username` is an error. `exists` never unlocks or reads the secret. Mutations hold a per-user `flock` on `$XDG_RUNTIME_DIR/rightkit-credentials.lock`. A `WindowsTarget` has no effect on Linux. `MemoryBackend`, the guard variables and `GuardMode` behave as on the other platforms. Verified on Linux by PR CI only (see CHANGELOG); platforms without a wired store still get `UnavailableBackend`.

- Android: an AES-256-GCM key in `AndroidKeyStore` (one per namespace service) encrypts each secret; only ciphertext is written, to `<files_dir>/rightkit-credentials/credentials-v1.json` (`src/android.rs`). The JNI access is injected: the `rightkit-android` bridge crate implements `android::KeystoreBridge` and calls `android::install(bridge, files_dir)` once at startup. Before that, or when the bridge has no JNI context, every call is `Error::Unavailable`; there is no file, memory or in-crate key fallback. A credential whose Keystore key is gone (for example after an app-data restore) is an error, never absent; exclude `files/rightkit-credentials/` from Auto Backup. Mutations hold a per-directory `flock`. A `WindowsTarget` has no effect. The module compiles on every platform and is unit-tested on the host with a fake bridge; the Keystore itself is not verified on a device.

Test guards: `RIGHTKIT_NO_KEYCHAIN` (the universal guard) and per-app variables swap in an in-memory backend or deny access.

Published version: see `INDEX.md` at the repository root. Release notes: [CHANGELOG.md](CHANGELOG.md).

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/store.rs` and `tests/e2e.rs`.

```rust,ignore
// Open a Store for the app's Namespace, check presence passively,
// and read or provision a key through get_or_create_key.
```

## Windows

- Backend: Credential Manager generic credentials (`CredReadW`, `CredWriteW`, `CredDeleteW`), persisted per user with `CRED_PERSIST_LOCAL_MACHINE`. Target names are `{prefix}/{account}` for the slash form, or `{account}.{service}` for `WindowsTarget::KeyringCompat`, which reads and migrates items the `keyring` crate wrote.
- Credential Manager never prompts. The `Interaction` argument is ignored, so interactive and non-interactive calls behave the same. `exists` reads the credential and discards the blob without copying it.
- Every mutation (`set`, `add_if_absent`, `delete`, `delete_if_equals`) takes one named mutex, `Global\rightkit-secrets-t-<suffix>`, for its whole read-check-write. The suffix is the first 16 bytes of a BCrypt SHA-256 over a fixed tag, NUL, the **current user's SID**, NUL and the **normalised native target name** (case-folded), so two services that share a `Slash` prefix, or names that differ only in case, lock the same entry. It then also takes the mutex 0.1.0 to 0.1.3 use for the same service and account (`Local\rightkit-secrets-<suffix>`, per logon session), always second, so an older and a newer build of one app exclude each other while both run. A wait over 10 s on either returns `Error::Timeout`. `Global\` makes the mutex visible to every logon session of the user, matching the scope of a `CRED_PERSIST_LOCAL_MACHINE` entry; it is created with a DACL granting only the user's SID, and creation failure (for example access denied) is an error, never a fallback to no lock. Reads (`get`, `exists`) take no lock. Credential Manager has no compare-and-set, so the lock orders writers that use this crate; it cannot order the `keyring` crate or other tools.
- Target names: prefix, service and account must be non-empty, at most 250 bytes and free of control characters, and the whole name is within 32 767 UTF-16 units. A NUL would end the native name early (`"victim\0x"` addresses `victim`), so it is rejected with `Error::Invalid` by every backend and by `Namespace::try_windows_target`.
- `WindowsTarget::KeyringCompat` stores UTF-16LE text, as the `keyring` crate does, for reads and writes; the encoding comes from the namespace and is never guessed (UTF-16LE text can be byte-identical to different UTF-8 text). A non-text secret is `Error::NotUtf8`, and a blob that is not valid UTF-16LE reads as `Error::NotUtf8`. Items stored as raw UTF-8 by 0.1.1 and 0.1.2 are read through `WindowsTarget::KeyringCompatLegacyUtf8` (read and delete only) and rewritten through `KeyringCompat`.
- Errors: Win32 failures surface as `Error::Backend { op, code }`. `ERROR_NOT_FOUND` is not an error: reads return `None` and delete returns `false`.
- Dependency: `windows-sys` 0.61 under `cfg(windows)`, with features `Win32_Foundation`, `Win32_Security`, `Win32_Security_Authorization`, `Win32_Security_Credentials`, `Win32_Security_Cryptography` and `Win32_System_Threading`. Consumers enable no Windows features.
- Elevation is not required by any code path in this crate.
- `tests/windows_lock.rs` covers the cross-process lock, `tests/windows_target.rs` the NUL-prefix isolation and shared-prefix locking, and `tests/windows_keyring_compat.rs` the codec. They were not run for this section.

## Timeouts, retries and migration

- A call that times out keeps running inside the OS. Mutations of one native credential are serialised and the credential stays claimed until the OS call returns, so a retry that arrives while a timed-out call is still running gets `Error::InProgress` (retry after a short delay) instead of racing it and being overwritten.
- `migrate_between` copies and verifies, then removes the source only through `Store::delete_if_equals` (delete only if it still holds the copied value). Memory and Windows can do that atomically; macOS cannot, so there the source is **kept** and listed in `MigrationReport::retained`. A source that changed after it was read is also kept. A destination that is not durable (`Store::is_durable`: a guard put it in memory or deny mode, or its backend does not report `Backend::durable`) takes nothing and removes nothing: every account is listed in `MigrationReport::refused`.
- Every write a `Store` makes that can replace a stored value takes an explicit `WriteIntent` (`CreateOnly` or `Replace`) through `Store::put`; `Store::set` is `Replace` and `Store::add_if_absent` is `CreateOnly`. `Store::import_env` takes the intent (`CreateOnly` cannot overwrite a credential saved concurrently, because the check and the write are one call). The CLI `set` is `CreateOnly` unless `--replace` is given. This is a statement about `Store` only: `apple_helper::AppleHelper` is a separate client of the Swift helper, outside `Store` and its exclusion, and its `write` and `delete_everywhere` replace and remove by design.
- `migrate_plaintext_file(store, account, path)` COPIES a legacy plaintext key file into the store: it reads the file once, read-only, stores the trimmed value with `CreateOnly`, reads it back, and never modifies, wipes, moves or deletes the file, whatever it returns. `FileMigration::Retained` means the store holds exactly the file's value; `StoredValueDiffers` means it holds a different one and neither was changed. Deleting the file is the app's explicit, separate step, taken after it has verified the stored value and when nothing else writes the file: this crate shares no lock with the file's writers, so it cannot make that deletion safe.
- `Store::generate_key(bytes)` returns a fresh random key (base64) without storing it, for rotation keys. `apple_helper::resolve_bundled(name)` finds and contains a helper inside the running app bundle (placement check only, no signature check).
- `apple_helper`: one deadline covers the request write, the reply read and the helper exit; the helper is killed and reaped on every path. A `read` reply's `value` is unescaped into a zeroised buffer without a serde_json intermediate.

## Retained rotation store & native child execution

Use `store.pinned()?` before `begin_rotation`, retaining that handle through external rekeying
& `commit_rotation` or `abort_rotation`. Its reads, writes & rotation locks remain bound to one
backend when default keychain changes. Ordinary Store handles continue following current default.
After restart, reselect original default keychain for commit recovery; production explicit-file
macOS backends remain read/delete-only.

`rightkit-credentials exec --namespace N --account A --env NAME -- /absolute/native/executable args...`
reads one credential from an unguarded durable OS store & puts its UTF-8 value only in child
environment. Missing credentials, read errors, NUL bytes, invalid UTF-8 & scripts prevent child
launch. No shell, parent environment mutation, secret argument or secret file is used. Child output
is inherited & child exit status is returned; selected executable controls its own output.

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-credentials`; add `--features isolated-test-keychain` on macOS for `tests/e2e.rs`.
