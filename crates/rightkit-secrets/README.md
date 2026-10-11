# rightkit-secrets

Per-app OS credential storage for Right Suite applications. It provides macOS Keychain and Windows Credential Manager access behind one `Store`, with a per-app `Namespace`. It also provides passive presence checks (`Store::exists` never requests secret bytes or prompts), non-interactive runtime behavior by default, test and development guards, DB-key provisioning (`Store::get_or_create_key`), and migration of legacy items (`migrate_between`).

**Use it** to store per-app secrets such as API tokens, refresh tokens, and database keys in the OS credential store.

**Do not use it** for secrets that must be shared between apps (use the per-app namespace) or for non-secret settings (use `rightkit-settings`, which holds only `SecretRef`s).

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `settings-probe` | off | `rightkit-settings` | Enables the `rightkit-settings` presence adapter (`SecretProbe`). |

## Platform support

- macOS: the user-domain default keychain (explicit on every call; the search list is never used), with the `apple_helper` module driving the synced-credentials Swift helper (`RightKitAppleCredentials`).
- Windows: Credential Manager (`src/windows.rs`).
- Linux: no OS store is wired. Every call fails closed (`src/backend.rs`).

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
- `add_if_absent` is the only atomic create. It takes a process-wide `Mutex`, then a named mutex `Local\rightkit-secrets-<suffix>`. The suffix is the first 16 bytes of a BCrypt SHA-256 over service, NUL and account. A wait over 10 s returns `Error::Timeout`. `Local\` scopes the mutex to the logon session. `set`, `get` and `delete` take no lock.
- Errors: Win32 failures surface as `Error::Backend { op, code }`. `ERROR_NOT_FOUND` is not an error: reads return `None` and delete returns `false`.
- Dependency: `windows-sys` 0.61 under `cfg(windows)`, with features `Win32_Foundation`, `Win32_Security_Credentials`, `Win32_Security_Cryptography` and `Win32_System_Threading`. Consumers enable no Windows features.
- Elevation is not required by any code path in this crate.
- `tests/windows_lock.rs` covers the cross-process lock. It was not run for this section.

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-secrets`.
