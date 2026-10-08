# rightkit-secrets

Per-app OS credential storage for Right Suite applications. It provides macOS Keychain and Windows Credential Manager access behind one `Store`, with a per-app `Namespace`. It also provides passive presence checks (`Store::exists` never requests secret bytes or prompts), non-interactive runtime behavior by default, test and development guards, DB-key provisioning (`Store::get_or_create_key`), and migration of legacy items (`migrate_between`).

**Use it** to store per-app secrets such as API tokens, refresh tokens, and database keys in the OS credential store.

**Do not use it** for secrets that must be shared between apps (use the per-app namespace) or for non-secret settings (use `rightkit-settings`, which holds only `SecretRef`s).

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `settings-probe` | off | `rightkit-settings` | Enables the `rightkit-settings` presence adapter (`SecretProbe`). |

## Platform support

- macOS: Keychain, with the `apple_helper` module driving the synced-credentials Swift helper (`RightKitAppleCredentials`).
- Windows: Credential Manager (`src/windows.rs`).
- Linux: no OS store is wired. Every call fails closed (`src/backend.rs`).

Test guards: `RIGHTKIT_NO_KEYCHAIN` (the universal guard) and per-app variables swap in an in-memory backend or deny access.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/store.rs` and `tests/e2e.rs`.

```rust,ignore
// Open a Store for the app's Namespace, check presence passively,
// and read or provision a key through get_or_create_key.
```

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-secrets`.
