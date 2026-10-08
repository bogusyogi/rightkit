# rightkit-secrets

Per-app OS credential storage for Right Suite applications. It provides Keychain or Credential Manager access, passive presence checks, non-interactive runtime behavior, test guards, DB-key provisioning, and migration helpers.

The `settings-probe` feature enables the `rightkit-settings` presence adapter.

## Windows

- Backend: Credential Manager generic credentials (`CredReadW`, `CredWriteW`, `CredDeleteW`), persisted per user with `CRED_PERSIST_LOCAL_MACHINE`. Target names are `{prefix}/{account}` for the slash form, or `{account}.{service}` for `WindowsTarget::KeyringCompat`, which reads and migrates items the `keyring` crate wrote.
- Credential Manager never prompts. The `Interaction` argument is ignored, so interactive and non-interactive calls behave the same. `exists` reads the credential and discards the blob without copying it.
- `add_if_absent` is the only atomic create. It takes a process-wide `Mutex`, then a named mutex `Local\rightkit-secrets-<suffix>`. The suffix is the first 16 bytes of a BCrypt SHA-256 over service, NUL and account. A wait over 10 s returns `Error::Timeout`. `Local\` scopes the mutex to the logon session. `set`, `get` and `delete` take no lock.
- Errors: Win32 failures surface as `Error::Backend { op, code }`. `ERROR_NOT_FOUND` is not an error: reads return `None` and delete returns `false`.
- Dependency: `windows-sys` 0.61 under `cfg(windows)`, with features `Win32_Foundation`, `Win32_Security_Credentials`, `Win32_Security_Cryptography` and `Win32_System_Threading`. Consumers enable no Windows features.
- Elevation is not required by any code path in this crate.
- `tests/windows_lock.rs` covers the cross-process lock. It was not run for this section.

Publish through the RightKit release workflow.
