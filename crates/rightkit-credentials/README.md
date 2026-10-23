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
| `os-auth` (0.4.0) | off | `objc2`, `block2`, `objc2-foundation` (macOS only), `windows` (Windows only); nothing on Linux or Android | Compiles `os_auth`: one blocking OS user-presence prompt (see "OS authentication"). It gates a UI action and returns no secret. |
| `db-adapter` | off | `rightkit-db`, `sha2` | `db_adapter::CredentialsKeyStore` implements DB's `ProtectedKeyStore` over existing credential storage/rotation. Application must also enable its existing DB `sqlcipher` or `sqlcipher-system` provider; adapter selects neither. |

## Platform support

- macOS: the user-domain default keychain (explicit on every call; the search list is never used), with the `apple_helper` module driving the synced-credentials Swift helper (`RightKitAppleCredentials`). Apple's creation calls fall back to the default keychain when an explicit keychain is unusable, so a write is gated before it is made: the destination must be valid, present on disk and unlocked, and an explicit keychain from `KeychainBackend::open_file` is read and delete only, whether or not it is the current default (`Error::Unavailable` on write). Throwaway test keychains (`IsolatedKeychain`, `KeychainBackend::open_isolated_file`, temp directory only) can be written only in this crate's own tests and under the `isolated-test-keychain` feature; a production build has no write path to an explicit keychain. The mutation-exclusion identity of a macOS credential is its keychain file plus Keychain service and account (`macos_credential_identity`); a `WindowsTarget` prefix plays no part in it. A mutation resolves the default keychain exactly once (`Backend::pinned`): `Store` takes the exclusion identity from that one resolved keychain and makes the write on it, so the two cannot differ if the default is switched while an earlier call is stalled; a default that cannot be resolved or identified refuses the mutation before anything is locked or written. See `src/macos.rs`.
- Windows: Credential Manager (`src/windows.rs`).
- Linux: the Secret Service on the user's session bus (GNOME Keyring, KWallet, KeePassXC) through `secret-service` 5.2 (`src/linux.rs`). Items use the `keyring-core` layout (`service` and `username` attributes, default collection, raw bytes), so items written by `keyring-core` with `zbus-secret-service-keyring-store` stay readable and the reverse. It fails closed: no session bus, provider or default collection is `Error::Unavailable`; a locked item or collection is `Error::InteractionRequired` unless the call is `Interaction::Interactive`; more than one match for one `service` and `username` is an error. `exists` never unlocks or reads the secret. Mutations hold a per-user `flock` on `$XDG_RUNTIME_DIR/rightkit-credentials.lock`. A `WindowsTarget` has no effect on Linux. The guard variables and `GuardMode` behave as on the other platforms (`MemoryBackend` and `GuardMode::Memory` exist only in debug and test builds; a release build denies). Verified on Linux by PR CI only (see CHANGELOG); platforms without a wired store still get `UnavailableBackend`.

- Android: an AES-256-GCM key in `AndroidKeyStore` (one per namespace service) encrypts each secret; only ciphertext is written, to `<files_dir>/rightkit-credentials/credentials-v1.json` (`src/android.rs`). The JNI access is injected: the `rightkit-android` bridge crate implements `android::KeystoreBridge` and calls `android::install(bridge, files_dir)` once at startup. Before that, or when the bridge has no JNI context, every call is `Error::Unavailable`; there is no file, memory or in-crate key fallback. A credential whose Keystore key is gone (for example after an app-data restore) is an error, never absent; exclude `files/rightkit-credentials/` from Auto Backup. Mutations hold a per-directory `flock`. A `WindowsTarget` has no effect. The module compiles on every platform and is unit-tested on the host with a fake bridge; the Keystore itself is not verified on a device.

Test guards: `RIGHTKIT_NO_KEYCHAIN` (the universal guard) and per-app variables swap in an in-memory backend or deny access.

Published version: see `INDEX.md` at the repository root. Release notes: [CHANGELOG.md](CHANGELOG.md).

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/store.rs` and `tests/e2e.rs`.

```rust,ignore
// Open a Store for the app's Namespace, check presence passively,
// and read or provision a key through get_or_create_key.
```

## OS authentication

Feature `os-auth`. `os_auth::{availability, authenticate, Request, Verified, OsAuthenticator, SystemAuthenticator, OsAuthError, Availability}`: one blocking prompt that asks the OS whether the device owner is present. It is the primitive an app-lock coordinator calls; the coordinator (lock state, PIN fallback, action policy) is the app's or the shell's, not this crate's.

- macOS: `LocalAuthentication` `LAContext`, policy `deviceOwnerAuthentication` (Touch ID, Apple Watch or the account password), through the Objective-C runtime already used by other RightKit crates. Needs a session that can show the system sheet.
- Windows: Windows Hello (`UserConsentVerifier`) only; there is no CredUI or password fallback. A user with no Hello method set up gets `Availability::NotEnrolled` and `OsAuthError::NotEnrolled` and nothing is shown (fail closed), so the app-lock coordinator must offer its own fallback or ask the person to set up Hello. An unpackaged desktop app passes its window with `Request::owner_window(hwnd)` so the dialog appears in front of it; without one, a non-packaged app gets an `Os` error (fail closed).
- Android: install a `BiometricBridge` with `os_auth::install_biometric_bridge` (the `rightkit-android` bridge crate implements it over `BiometricPrompt`, `BIOMETRIC_STRONG | DEVICE_CREDENTIAL`, no `CryptoObject`). Before that, `Availability::Unavailable` and `OsAuthError::Unavailable`.
- Linux and other targets: `Availability::Unsupported` and `OsAuthError::Unsupported`.

Rules the API enforces. `availability()` is passive and never prompts; only `authenticate` shows UI. The only success value is `Verified` (no public constructor, no serde, no secret); every other outcome is an `OsAuthError`, so a caller that treats `Err` as "stay locked" fails closed, and an unknown OS result code is an error, never a pass. `OsAuthError::is_user_decision()` is true only for `Cancelled`, `Failed` and `LockedOut`; a prompt the system or the app cancelled (`SystemCancelled`) is not the person's decision, so a caller counting failed attempts should not count it. A timeout cancels the prompt and a late OS success is discarded. One prompt per process at a time (`OsAuthError::Busy`). `RIGHTKIT_NO_KEYCHAIN` (and `Request::guard_env`) make every call `Guarded` without touching the OS. `ScriptedAuthenticator` (debug and test builds only, like `MemoryBackend`) stands in for the OS in coordinator tests through the `OsAuthenticator` trait.

What it is not: a key release. A `Verified` proves one OS prompt succeeded in this process, nothing cryptographic; it does not unlock, read or bind any stored credential, and a caller that needs a secret released only after authentication needs a separate OS-bound key design. There is deliberately no renderer-facing getter or event here: a webview host exposes at most the app-lock state it owns.

Calls block: run them off the UI thread. Not verified on real hardware (see CHANGELOG).

## Encrypted database keys

With `db-adapter`, construct `db_adapter::CredentialsKeyStore::new(store, primary_account, recovery_account, legacy_staging_accounts)`. `store` supplies existing namespace, interaction/guard policy & shared `rotation_lock_dir`; it is pinned once. Account slots must be distinct, including `<primary>.rotation-pending`; only explicitly mapped legacy accounts can be read/deleted through the adapter. Missing slots return `None`; malformed, denied or unavailable slots return classified errors. Pending revisions contain a digest, never plaintext key material.

DB owns transitions, readback, recovery & decisions to retire viable keys. Adapter exposes only bounded `lock_rotation`, which returns an owned `ProtectedKeySession` containing all key-slot methods. Session can move threads; unrelated callers must acquire their own session & cannot borrow its authority. It owns credentials' existing canonical process/file fence through DB work; timed-out credential mutation workers retain its file hold. Authentication remains caller policy; an OS-auth result does not release a database key. Standalone qualification must pair `db-adapter` with `rightkit-db/sqlcipher` or `rightkit-db/sqlcipher-system`, matching application provider.

Timeouts return `Unsettled`: a worker may still mutate storage. Factory admission & every session slot operation refuse while any mapped slot has an in-flight mutation; an old-primary read never proves settlement. Dropping a session cannot release a durable worker's file hold. DB retains viable keys & journal state until a later exclusive session can reconcile actual settled storage.

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

Rotation is exclusive across processes: set `Config::rotation_lock_dir(dir)` (absolute, private to
the user, the same directory in every process; a durable OS store refuses to rotate without it).
Each begin, commit & abort holds that credential's lock file for its whole read-write-remove
sequence and waits at most the store timeout (`Error::InProgress`). A backend call that timed out keeps
the lock until it returns, and the lock file is named from the native store & credential identity (not
the backend name), so aliases of one native store share it. The caller that began a
rotation commits it with `commit_rotation_of(account, &new)` (abort: `abort_rotation_of`): it acts
only while `new` is still the pending value, so a rotation aborted or replaced by another process
is refused with `Error::Invalid { what: "pending rotation", .. }` and never overwrites the newer
primary. `commit_rotation` / `abort_rotation` act on whatever is pending, for recovery after a restart.

`rightkit-credentials exec --namespace N --account A --env NAME -- /absolute/native/executable args...`
reads one credential from an unguarded durable OS store & puts its UTF-8 value only in child
environment. Missing credentials, read errors, NUL bytes, invalid UTF-8 & scripts prevent child
launch. No shell, parent environment mutation, secret argument or secret file is used. Child output
is inherited & child exit status is returned; selected executable controls its own output.

## Helpers, sidecars and CLIs (owner-binary hand-off)

Only the owner binary reads the OS store. Cross-binary keychain sharing is settled as never (#884, #771); every other binary of the app receives secrets from the owner through `handoff`, which compiles on every OS and is transport-agnostic.

- **Sidecar the owner spawns:** the owner sends a Reply frame (`Message::reply_bundle` or `reply_snapshot`, written with `write_message`) to the child's private stdin; the child calls `read_stdin_snapshot()` (or `read_stdin_bundle()` for strict), which returns the correlation id with the data (`Delivery { id, bundle }`), applies it and answers with `delivery.ack()`, which the owner checks with `expect_ack`. Never argv, never the environment: the spawner must also call `scrub_inherited_env(&mut command, &[..names])` so an ambient `GROQ_API_KEY` is not inherited. `read_stdin_*` read an owned, unbuffered duplicate of stdin (no unsafe code; a closed or null stdin is a typed error; one stdin session per process, a second is `SessionBusy`), never `std::io::stdin()`, whose process-wide buffer would keep an unwiped copy. HeardRight: the app's `SetProviderKeys` delivery becomes a snapshot of its optional provider keys (`groq`, `nvidia`, `openrouter`).
- **Independent helper or CLI:** it sends `Message::request(id, mode, names)` over the app's authenticated local IPC (rightkit-daemon's authenticated channel, or the app's own existing one). The owner verifies the caller (same app, same user, signed path) first, which is the transport's job and must happen before `answer`, then calls `policy.respond(&store, &request)` and writes the result: a Reply with the request's id, or an Error with a typed code (`NotAllowed`, `NotFound`, `StoreError`, `Invalid`) and no value. The client accepts it only through `expect_reply(msg, id, mode, &names)`, which rejects a wrong id (a stale reply), wrong mode or any name set other than the requested one. Pulse: the owner opens `Namespace::new("pulse.send-identity")` and builds `HandoffPolicy::new([("send.identity", "identity-v1")])?`; `pulse` and `pulse-engine` ask for `send.identity` instead of reading the keychain (service and account as in `pulse/core/src/sharing.rs:58-59`).
- **Policy:** `HandoffPolicy::new` takes explicit `(logical name, account)` pairs; a logical name is never used as an account. `answer` (strict: any missing name is `NotFound`) and `answer_snapshot` (optional keys: each name Present, Absent or ReadFailed with a code) force non-interactive reads whatever store they are given, so a request can never raise an OS prompt, and validate every name before the first read. Log request names, never values.
- **Snapshot replacement rule:** the receiver replaces its whole set with the Present entries (`Snapshot::replace_into`) and wipes every name that is Absent, ReadFailed or not mentioned; an all-Absent snapshot clears it.
- **Frame:** `RKHO` magic, version `1`, kind (Request 1, Reply 2, Error 3, Ack 4), big-endian `u64` correlation id, big-endian `u32` body length (18-byte header); names `[a-z0-9._-]{1,64}`, unique; default 64 KiB body and 32 names, 1 MiB ceiling. Wrong magic, version, kind, oversize, bad or duplicate names, truncation and bytes left inside the declared body are typed `HandoffError`s that carry no value bytes. One read consumes exactly one frame: bytes after it stay in the stream (concatenated frames are read one call at a time); only `read_single_message` also demands EOF.
- **Deadlines and buffers:** all reads are synchronous on a caller-owned reader and have no deadline; the transport owns it (a read timeout, or closing the transport from outside, as HeardRight's supervisor does with its 3 s acknowledgment timeout). Readers and writers you pass in must be unbuffered or wipe their buffers; this module zeroizes its own scratch buffers on every path. The encoder validates a message (names, count, strict replies Present-only, sizes) before writing anything.


## Verification

Run through the RightKit wrapper: `rustright cargo test -p rightkit-credentials`; add `--features isolated-test-keychain` on macOS for `tests/e2e.rs`.
