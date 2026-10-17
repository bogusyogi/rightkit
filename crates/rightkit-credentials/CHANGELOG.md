# Changelog

## Unreleased

- Cleared version for macOS (Orthic-Labs/rightkit#308, ledger 103). `rightkit-secrets` 0.1.3 and earlier read, modified and deleted through the whole user keychain search list on macOS (the default-keychain defect fixed in 0.2.0 below), so they are not cleared. The cleared line is `rightkit-credentials` 0.2.0 or newer (the crate renamed from `rightkit-secrets`; this release, 0.2.1, is the first that also carries the Linux and Android backends). Migrate with `rightkit-credentials = "0.2.1"` and the new crate name. No code change; documentation only.

## 0.2.1

- Added (rightkit#153): Android Keystore store (`android` module: `AndroidKeystoreBackend`, `KeystoreBridge`, `BridgeError`, `install`; `system_backend()` on `target_os = "android"`). An AES-256-GCM key per namespace service stays inside `AndroidKeyStore`; secrets are encrypted through an injected `KeystoreBridge` (the JNI side belongs to the `rightkit-android` bridge crate, rightkit#149; this crate links no JNI and adds no dependency) and only ciphertext is written to `<files_dir>/rightkit-credentials/credentials-v1.json`, bound to its `(service, account)` through the GCM AAD. Mutations take a per-directory `flock` and replace the file by rename. Before `install` (or with a bridge that reports no context) every call is `Error::Unavailable`; a store file that is unreadable or of an unknown version fails closed and is never overwritten; a credential whose Keystore key is gone is an error (code -4), never `Ok(None)`. Additive, so it stays inside 0.2.1. The module compiles on every platform and is unit-tested on the host with a fake bridge; the real Keystore and multi-process behaviour are unverified on a device.
- Added: Linux Secret Service backend (`SecretServiceBackend`, `system_backend()` on `target_os = "linux"`), built on `secret-service` 5.2.0 (blocking API, `rt-async-io-crypto-rust`). Item layout matches `keyring-core` with `zbus-secret-service-keyring-store` (`service` and `username` attributes, default collection, raw bytes), so existing items stay readable. Fails closed when no session bus, provider or default collection exists; a locked store is `InteractionRequired` unless the call is interactive; an ambiguous match is an error. Mutations take a per-user `flock` in `$XDG_RUNTIME_DIR`. Replaces the app-owned `cfg(linux)` keyring code; apps can drop `keyring-core` and `zbus-secret-service-keyring-store` for Linux. Closes #90.
- Additive: no existing API changes, so this is a patch release and `rightkit-oauth` and `rightkit-record` keep their `0.2.0` requirement. The Linux backend is verified only by CI on Linux (no live Secret Service in the default runner: the live test skips there); the `tests/linux_secret_service.rs` round-trip runs with `RIGHTKIT_TEST_REAL_USER_STORES=1` against a running Secret Service.

Renamed from rightkit-secrets; 0.1.3 remains published under the old name.

- Added (M11-O3): pending-rotation recovery on `Store`: `begin_rotation`, `pending_rotation`, `commit_rotation`, `abort_rotation` (marker item `<account>.rotation-pending` holds the new value; commit is idempotent). Additive; replaces an app-owned pending-key item.
- Tests only (M11-O1): `scope_changing_calls_send_the_documented_command_and_read_copied` pins the wire request and `copied` reply of `AppleHelper::delete_everywhere` and `copy_synced_to_device_only`; the matching semantics are tested in RightKitSwift `AppleCredentialTests`. No product change.

## 0.2.0

Published version number: this release changes or removes public items, so it is a breaking release under Cargo's rules for 0.x versions. It was developed as 0.1.4, which was never published. `migrate_plaintext_file`, `FileMigration`, the `settings-probe` trait version and the CLI `set` behaviour all changed.

- **Windows: a mutation also takes the lock 0.1.0 to 0.1.3 take (comparative review, 2026-10-12).** Those versions hold
  `Local\rightkit-secrets-<hash of service and account>` across the check and the write; this version held only the new
  `Global\rightkit-secrets-t-<hash>`. With an older and a newer build of one app running at once, both could find a database
  key absent, each create one, and the later write would replace the key the other had already returned. Every mutation now
  takes the new lock and then the old one, in that fixed order; failing to take either is an error. An older process takes
  one lock only, so no cycle can form. Tests: `the_legacy_lock_has_the_name_older_versions_take`,
  `a_mutation_waits_for_a_holder_of_the_legacy_lock`. Limit: the old lock is per logon session, so an older process in
  another session of the same user is still not excluded (it never was).
- Not listed before, both breaking: the `settings-probe` feature implements `rightkit_settings::SecretProbe` of
  rightkit-settings **0.2** (0.1.3 implemented the 0.1 trait, so a consumer still on settings 0.1 no longer gets the
  implementation); and the CLI's `set` refuses when a credential exists unless `--replace` is given (0.1.3 replaced).
- Tests: the Windows tests that write to the real Credential Manager (`tests/store.rs` round trip, `windows_target.rs`,
  `windows_lock.rs`, `windows_keyring_compat.rs`) run only on CI (`GITHUB_ACTIONS=true`) or with `RIGHTKIT_TEST_REAL_USER_STORES=1`, and otherwise print why and pass.
  Each asserts that the name it writes starts with its own test prefix, and the round trip now removes its credential
  however it ends. Shared helper: `tests/common/mod.rs`. No product change.
- **Publication-path reduction (2026-10-11), after `docs/audits/2026-10-11-astra-publication-path-consult.md`
  and the fifth read (`docs/audits/2026-10-11-astra-secrets-fifth-read.md`). Removes optional
  machinery, keeps the proven fixes and adds no platform call. 0.1.4 is unreleased and, like the
  rest of it, breaking relative to 0.1.3.**
  - **Plaintext-file retirement is removed.** `LegacyFile` (with `RetireIfUnchanged`), the
    rename-aside, the re-verification, the wipe and the delete of the legacy file, and their tests are
    gone. No step of retiring a file can be made safe against that file's writers (the fifth read's
    remaining race is a writer that holds the file open and writes in place between the last check and
    the wipe), and 0.1.3 wiped and deleted by pathname with no check at all, so the reduction is to
    delete nothing. `migrate_plaintext_file(store, account, path)` has 0.1.3's signature again: it
    opens the file read-only, stores the trimmed value with `WriteIntent::CreateOnly`, reads it back
    and ALWAYS leaves the file exactly as it was. Removing the file is the app's explicit, separate
    step, taken after it has verified the stored value. **`FileMigration` is now `NoFile | Retained |
    StoredValueDiffers | StoreUnavailable`**: `Migrated` and `AlreadyStored` are removed (only the
    retirement path produced them, and keeping them with a changed meaning would let a caller that
    matches `Migrated` assume the file is gone), `SourceChanged` is renamed `StoredValueDiffers` (it
    now means only that the store already holds a value different from the file's, and neither was
    changed), and `Retained` means the store holds exactly the file's value and the file is
    untouched. A source-scan test pins that `migrate.rs` contains no file-mutating call.
  - **macOS: the keychain a mutation locks is the keychain it writes (fifth read, finding 11).**
    The exclusion identity was read by resolving the user-default keychain in `Store`, and the write
    resolved the default again in the worker, so a default switched while an earlier call was stalled
    let that call land on the new default under the old keychain's permit (and a first resolution that
    failed gave a placeholder identity that a later success no longer matched). One function now
    decides: new `Backend::pinned` (default `Ok(None)`, so existing backends are unchanged) lets a
    backend whose target can change fix it ONCE per mutation. `Store` takes the exclusion identity from
    the pinned backend and runs the mutation on that same backend. On macOS `pin_default` resolves the
    default once, reads the identity of its file from that same reference, and the pinned backend keeps
    the reference, so the write and the identity cannot differ. A default that cannot be resolved or
    whose file cannot be identified refuses the mutation (`Error::Unavailable`) before anything is
    locked or written; the placeholder identity is gone. A pinned backend that reports no identity is
    refused too. Reads still resolve the default inside their own call. No new Security.framework call.
    Tests: default-switch and resolution-failure interleavings on a fake backend (all platforms), and
    `pin_default` unit tests on macOS that use no keychain.
  - **Docs: "every write goes through `Store::put`" is narrowed to `Store`.** The claim was false for
    the public `apple_helper::AppleHelper::write` and `copy_synced_to_device_only`, which drive the
    Swift helper outside `Store` and its exclusion and replace by design; they are unchanged from
    0.1.3. The source-scan guard is documented as covering `migrate.rs` only. The `open_file` and README
    text now say what the code does: a write to any explicit keychain is always refused, not only when
    it is not the default.

- **Fourth independent read (2026-10-11): seven fixes, each applied to the whole class of defect
  (`docs/audits/2026-10-11-astra-secrets-fourth-read.md`). API changes are breaking; 0.1.4 is unreleased.**
  - **Destructive replacement needs typed intent (finding 8).** Every operation that can remove or
    overwrite a persistent credential is now reached through one guarded primitive.
    `Store::put(account, secret, WriteIntent::{CreateOnly, Replace})` is the only write (`set` is
    `Replace`, `add_if_absent` is `CreateOnly`); a presence check followed by a write is gone from the
    crate. `migrate_between` refuses a destination that is not durable (new `Store::is_durable`, new
    `Backend::durable`, default `false`; OS backends `true`; `MemoryBackend::simulating_durable()` for
    tests) and lists the accounts in the new `MigrationReport::refused`, copying and removing nothing,
    so a guarded (memory or deny) destination can no longer cost the only persistent copy. The source
    is removed in exactly one function, `remove_copied_source`, which re-checks durability and that the
    destination still holds the copy, then uses `delete_if_equals`. `migrate_legacy_namespaces` writes
    no marker while any account is `refused`. Scope: `Store` and `migrate.rs`; the `apple_helper` driver
    is a separate client of the Swift helper and is outside this primitive.
  - **`import_env(overwrite: bool)` is now `import_env(intent: WriteIntent)` (finding 9).**
    `CreateOnly` is one `add_if_absent` under the target's exclusion, so a credential saved between the
    old `exists` and `set` is no longer overwritten by a stale environment value. `Store::add_if_absent`
    now also rejects an empty secret, like `set`.
  - **Native naming has one function per platform (finding 10).** New `Backend::credential_identity`
    (default: the Windows identity) is what `Store` keys its mutation exclusion on. macOS uses the new
    public `macos_credential_identity` (`macos:<folded service>\u{1f}<folded account>`, Windows prefix
    ignored), Windows `windows_credential_identity` (`windows:<normalised target>`, also the lock
    name's input), `MemoryBackend` its exact key. The native macOS calls take service and account only
    from `Namespace::keychain_item`. Tests pin the strings. A custom backend that does not address by
    Windows target must override `credential_identity`.
  - **macOS exclusion is per keychain file (finding 2).** The user-default target and an explicit
    target on the same file used different identities; both now use the canonical keychain path (the
    default is resolved once per mutation, see the reduction entry above).
  - **Explicit-keychain writes are not compiled into production builds (finding 1).** The fixture
    exemption keyed on a file name is gone: `IsolatedKeychain` and `open_isolated_file` exist only for
    this crate's tests and the non-default `isolated-test-keychain` feature (used by `tests/e2e.rs` and
    the CLI's `--isolated-keychain`). Without it `write_is_refused` holds for every explicit keychain.
  - **Plaintext file retirement (finding 4).** Superseded: the retirement path (`LegacyFile`,
    `RetireIfUnchanged`, move aside, wipe, delete) was removed in the reduction entry above;
    `migrate_plaintext_file` only copies.
  - **Zeroization probe replaced (finding 6).** The allocator-wide scan is deleted. A decoded `value` is
    held in `SecretBuf`, whose `Drop` wipes the initialised bytes and, under test, reports `len`,
    capacity at creation and at drop (equal: no growth, so no unwiped copy), and that every byte it
    wrote reads zero afterwards. Only bytes the program wrote are ever read. Error paths are tested.
  - **Tests (in-memory and fake backends only):** durability refusal for Memory and Deny guards and a
    plain memory backend; the moving case with a durable fake; `put` semantics; import race with a
    fake that saves between check and write; macOS-style naming across Windows prefixes with a stalled
    call; pinned identity strings; a source-scan guard that
    `migrate.rs` reaches writers only through the guarded primitives (plaintext-race tests were removed
    with the retirement path). macOS-only tests (identity of
    default versus explicit target) need `--features isolated-test-keychain`.

- **Third independent read (2026-10-11): six fixes (`docs/audits/2026-10-11-astra-license-secrets-third-read.md`).**
  - **macOS, explicit keychains are never written (finding 1).** The "is it the current default"
    check before the add could not be made atomic with the add (another process can switch the
    default and delete the file in between, and the OS add then stores the secret in the new
    default). A write through `KeychainBackend::open_file` is now ALWAYS `Error::Unavailable`; reads
    and deletes still work. `add_checked` no longer takes a default-keychain probe. The remaining
    non-destination-bound write is the throwaway fixture path (`IsolatedKeychain::backend()` and
    `open_isolated_file`), which holds fake data and is needed by the end-to-end suite and the CLI's
    `--isolated-keychain`; `open_isolated_file` now accepts only a keychain `IsolatedKeychain::create`
    made (directory `rightkit-secrets-test-*`, file `isolated.keychain-db`, under the temp directory),
    not an arbitrary temp file, so the bypass cannot be pointed at real data.
  - **All platforms, reopened `Store` (finding 2).** Mutation exclusion was keyed on the backend's
    `Arc` address, so a `Store` reopened over a fresh `system_backend()` bypassed the timed-out-call
    protection. New `Backend::exclusion_identity()` (default `None`, keeps the per-instance key, so
    existing custom backends compile and behave as before) lets instances of one OS store share a
    key. The macOS user-default keychain (and an explicit keychain by file path) and the Windows
    Credential Manager implement it.
  - **Windows, lock scope (finding 3).** The target mutex was `Local\`, scoped to one logon
    session, while the credential is persisted `CRED_PERSIST_LOCAL_MACHINE` and visible to every
    session of the user. It is now `Global\rightkit-secrets-t-<hash>`, the hash covering the user's
    SID and the normalised target, created with a DACL that grants only that SID. Failure to create
    it (including access denied because another user holds the name) is an error, never an unlocked
    write. Needs the `windows-sys` features `Win32_Security` and `Win32_Security_Authorization`
    (already pulled in by this crate; consumers enable nothing). (Superseded by the first
    entry of this version: the old lock is taken as well, so older and newer processes exclude each other.)
  - **Plaintext migration (finding 4).** Superseded: this entry added deletion of the legacy file
    guarded by an identity and content check. The reduction entry above removed the deletion;
    `migrate_plaintext_file` now never changes the file. The reading side stands: the file is opened
    once and exactly those bytes are stored, and a `Store` whose guard put it in memory or deny mode
    returns `StoreUnavailable` and leaves the file, since copying into process memory and deleting the
    only persistent copy loses the secret.
  - **Windows, decoded secret prefixes (finding 5).** `KeyringCompat` blobs are decoded into one
    zeroizing buffer sized for the worst case (3 UTF-8 bytes per UTF-16 unit), so it never grows and
    an unpaired surrogate after a long valid prefix leaves nothing unwiped. Previously
    `String::from_utf16` grew a plain `String` and freed it on the error path.
  - **Test-only, zeroization probe (finding 6).** The counting allocator in `apple_helper` scanned
    entire freed blocks, including uninitialised capacity (undefined behaviour). While armed it now
    allocates zeroed and scans only the blocks it recorded itself, so every byte read is initialised.
  - **Test coverage (note 7, with `rightkit-license`).** New tests: explicit keychains refuse writes
    even when default; reopened-store exclusion; migration with a rewritten file, a replaced file, a
    guarded store, and the uncontended path; decoder allocates once; Windows lock is `Global\` and
    SID-bound. Not covered (needs two real logon sessions or a hostile peer): cross-session Windows
    exclusion and the access-denied squat case.

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
