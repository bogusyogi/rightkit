# HeardRight storage and network review: live 0.1.213 vs main 0.1.231 vs RightKit

Read-only comparison. Nothing was built or run; every finding is from reading source.

Trees (S = `<scratch>`):

- LIVE = `S/survey/heardright-live/tauri-app-next`
- MAIN = `S/survey/heardright-main/tauri-app-next`
- RK = `/home/user/rightkit/crates`

## Caveats that apply to every section

- **RightKit source is newer than main's pins.** I read RightKit at HEAD, not the pinned releases.

  | Package | main pins | RK HEAD |
  |---|---|---|
  | rightkit-db | 0.5.5 | 0.5.6 |
  | rightkit-credentials | 0.5.0 | 0.5.1 |
  | rightkit-fs | 0.4.12 | 0.4.13 |
  | rightkit-http | 0.3.9 | 0.3.12 |
  | rightkit-sync | 0.2.2 | 0.2.3 (unpublished) |
  | rightkit-crypto | 0.1.0 | 0.1.3 |

  Differences between the pinned and HEAD versions are **not verified**.
- `MAIN/../rightkit-mandatory.json` is stale: it still records db 0.5.2, fs 0.4.7, credentials 0.4.2, http 0.3.8.
- The inventory JSON lists only `cloud_sync*.rs`, `credential_store.rs` and `secret_store.rs` for these packages. Everything else below came from searching the code.
- "Verified" means verified by reading. "Suspicion" means inferred, not confirmed.

## Verdicts

| Package | Verdict | Upgrade compatibility from 0.1.213 |
|---|---|---|
| rightkit-db | right shape with listed gaps | Same file, key, key form and schema; opens both ways by reading |
| rightkit-credentials | right shape for HeardRight | Same service and account names, read in place |
| rightkit-fs | right shape with listed gaps | Same root for a normal account; two edge cases |
| rightkit-http | right shape with listed gaps | No on-disk state; wire identity kept |
| rightkit-sync | right shape with listed gaps | Same container, zone and record fields; one-way ledger migration |
| rightkit-crypto | right shape for HeardRight | Trivial use (base64 only) |

---

## 1. rightkit-db

### 1.1 What the live app does

- One database only: `<app data>/heardright.sqlite`, with a one-time import of `history.json` (`LIVE/src-tauri/src/history/sqlcipher_store.rs:77-82`). Vocabulary, settings and sync ledgers are JSON files, not SQLite. A search for `rusqlite` and `sqlcipher` finds nothing outside `src-tauri/src/history/`.
- Engine: `rusqlite 0.40`. macOS and Linux use `bundled-sqlcipher` (CommonCrypto on macOS); Windows uses `bundled-sqlcipher-vendored-openssl` (`LIVE/src-tauri/Cargo.toml:122,168,186`).
- Key: `PRAGMA key = '<base64 of 32 random bytes>'`, a passphrase-style key that goes through SQLCipher's KDF. No `cipher_*` pragma is set, so SQLCipher 4 defaults apply (`LIVE/.../history/sqlcipher_conn.rs:14-27`).
- Connection contract, set and read back: WAL, `busy_timeout=5000`, `synchronous=NORMAL`, `journal_size_limit=64 MiB`, `temp_store=MEMORY`, `secure_delete=ON`, `optimize=0x10002` (`sqlcipher_conn.rs:29-74`).
- Schema: table `delivery_history(delivery_id PK, delivered_at_ms, record_json, app_name, outcome_kind)` and three indexes. Migration is a hand-written "add column if missing" step; there is no `user_version` and no migration ledger (`LIVE/.../history/migrations.rs`).
- Lost key: if the keychain item is absent, the live build mints a new key and then fails to open the existing file. History stays wedged (`LIVE/src-tauri/src/credential_store.rs:181-216`).

### 1.2 What RightKit provides

`RK/rightkit-db/src/lib.rs`:

- `DbOptions` with `open_writer`, read pools and `open_writer_recovering`.
- Keyed open that issues `PRAGMA key` first and then verifies with a read of `sqlite_master` (`lib.rs:378-386, 861-900`).
- The same pragma set as the live build (`lib.rs:70-71, 690-715, 1302-1304`).
- `WalStrict`, optional `secure_delete`, owner-only file modes, and refusal of a symlinked store file.
- `migrate` (versioned migrations, `add_column_if_missing`, `column_exists`), `rekey`, quarantine helpers, backup and restore, checkpoints.
- Feature `encrypted-lifecycle`: `EncryptedStore`, `rotate_key`, `recover_rotation`, relocation.
- `rightkit-credentials` feature `db-adapter` provides `CredentialsKeyStore`, including "legacy staging accounts" (`RK/rightkit-credentials/src/db_adapter.rs:98-128`).

### 1.3 Behaviour differences

| # | Difference | Status |
|---|---|---|
| D1 | **File, key and key form are identical.** Main still opens `heardright.sqlite` with `with_passphrase(key)`, which RightKit turns into `PRAGMA key = '<key>'` (`MAIN/.../history/sqlcipher_conn.rs:81-99`; `RK/rightkit-db/src/lib.rs:523-541, 861-866`). Neither side sets a `cipher_*` pragma. | Verified |
| D2 | **Schema is identical.** A diff of `migrations.rs` shows only helper swaps (`rightkit_db::column_exists`, `write_txn`). No `user_version` is written, so the live build can open a database that main has written. | Verified |
| D3 | **SQLCipher build changes.** rusqlite 0.40 to 0.41 on all platforms. Windows provider changes from vendored OpenSSL to LibTomCrypt (`MAIN/src-tauri/Cargo.toml:135`). The RightKit feature comment claims a real HeardRight database was proven to open (rightkit#1447). macOS keeps CommonCrypto (`sqlcipher-system`, `Cargo.toml:195`). | Suspicion (low). Exact SQLCipher versions and the Windows proof are not verified. |
| D4 | **Pragma contract is the same, but main reads back less.** Main reads back only `secure_delete`; WAL is enforced by `WalStrict` (`MAIN/.../sqlcipher_conn.rs:176-188`). The live build read back all six values. | Verified |
| D5 | **New: a symlinked database file is refused.** RightKit opens with `SQLITE_OPEN_NOFOLLOW` (`RK/rightkit-db/src/lib.rs:565-575`). The live build followed symlinks. Parent-directory symlinks are still followed, because the parent is canonicalised first. | Verified in code. Whether any user has a symlinked `heardright.sqlite` is a suspicion; the 0.1.57 field crash involved a symlinked directory, not a file. |
| D6 | **New: journal-mode environment switch.** `RIGHTKIT_SQLITE_JOURNAL_MODE=delete|truncate` overrides `WalStrict` in a release build (`lib.rs:805-837`). The live build had no such switch. | Verified |
| D7 | **Owner-only modes are off.** Main sets `options.owner_only = false`, so file modes are unchanged (`sqlcipher_conn.rs:91`). | Verified |
| D8 | **Lost-key handling changed.** Main quarantines an unreadable or keyless database and rebuilds it, with a staging-key retry first (`MAIN/.../sqlcipher_conn.rs:114-170`, `recovery.rs`). The live build wedged. | Verified |
| D9 | **Clear-history is stricter.** Main fails if the TRUNCATE checkpoint was blocked (`MAIN/.../sqlcipher_store.rs:437-455`). The live build ignored the result. | Verified |
| D10 | **Prepared-statement cache dropped in one place.** `push_batch` uses `prepare` with a comment saying rusqlite is built without the statement cache (`MAIN/.../sqlcipher_store.rs:206-209`), but `Cargo.toml:135,195` enables `statement-cache`. The comment is stale; the cost is one prepare per batch. | Verified |
| D11 | **Downgrade after key rotation.** If a rotation is interrupted, the live key sits only in the `SQLCipher/DBKey.staging` account, which 0.1.213 never reads. | Suspicion (edge case) |

### 1.4 What main calls

Both RightKit and its own code.

- RightKit: `DbOptions::open_writer` (`sqlcipher_conn.rs:84-97`), `rightkit_db::rekey` (`key_rotation.rs:20`), `write_txn` (`sqlcipher_store.rs` four sites, `migrations.rs`, `legacy_json_migration.rs`), `column_exists` (`migrations.rs:53`), `checkpoint_truncate` (`sqlcipher_store.rs:439`).
- Own code: key rotation, quarantine and recovery, the migration step, and the key-candidate logic in `open_and_key_resolved`.

### 1.5 Main code that duplicates RightKit

| Main file | Lines | RightKit equivalent |
|---|---|---|
| `src-tauri/src/history/key_rotation.rs` | 87 | `encrypted_lifecycle::rotate_key` and `recover_rotation`, with `CredentialsKeyStore` |
| `src-tauri/src/history/sqlcipher_conn.rs:114-223` | about 110 | `EncryptedStore::open`, `open_writer_with_key_candidates` |
| `src-tauri/src/history/recovery.rs` (quarantine half) | most of 978 | `quarantine_database_opts`, `open_writer_recovering`, `RecoveryPolicy` |
| `recovery.rs` helpers `sync_dir`, `move_no_clobber`, `same_file_identity` | about 150 | rightkit-fs `rename_exclusive` and the transaction module |
| `src-tauri/src/history/migrations.rs:51-80` | 30 | `add_column_if_missing` or `migrate` |
| `KeyVault` trait in `src-tauri/src/credential_store.rs:226-292` | about 65 | `ProtectedKeyStore` |

The CloudKit replay half of `recovery.rs` (scope-bound marker, cursor reset) is app policy and stays.

### 1.6 Post-live changes

- Deliberate: lost-key quarantine and recovery, user-initiated key rotation, strict checkpoint on clear, LibTomCrypt on Windows, removal of the test-only export.
- Looks accidental: the stale `prepare` comment (D10).

### 1.7 Verdict: right shape with listed gaps

The open path reproduces the live file, key form and pragmas, so an upgrade and a downgrade both work by reading. Main has not adopted the `encrypted-lifecycle` and quarantine owners, so about 1,200 lines of rotation and recovery code duplicate RightKit. Before deleting them, check that the RightKit lifecycle can express HeardRight's rule that the quarantined file is kept until a CloudKit replay receipt arrives.

---

## 2. rightkit-credentials

### 2.1 What the live app does

Two mechanisms.

- **Database key, direct OS calls** (`LIVE/src-tauri/src/credential_store.rs`):
  - macOS: `security_framework` `get_generic_password` / `set_generic_password`, service `app.heardright.next`, account `SQLCipher/DBKey` (`:26-28, 148-165`).
  - Windows: `CredReadW` / `CredWriteW`, target `HeardRight/SQLCipher/DBKey`, raw UTF-8 blob, `CRED_PERSIST_LOCAL_MACHINE` (`:14, 71-137`).
- **Everything else, `keyring-core`** with the Apple keychain store or Windows native store (`LIVE/src-tauri/src/license/keychain.rs:99-135`):

  | Service | Account |
  |---|---|
  | `app.heardright.next.license` | `activation` |
  | `app.heardright.next.license-clock` | `highest-seen-unix` |
  | `app.heardright.next.license-removals` | `pending` |
  | `app.heardright.next.device` | `device-id` |
  | `app.heardright.next.account-session` | `session` |
  | `app.heardright.next.l3` | `groq`, `openrouter`, `nvidia` |

- `pro.bridge` and `sync.consent` go through a Swift helper with keychain access groups (`pro_bridge.rs`), not through either mechanism.

### 2.2 What RightKit provides

- `Store` and `Namespace(service)` with a selectable Windows target shape: `Slash{prefix}` (raw blob) or `KeyringCompat` (`{account}.{service}`, UTF-16LE blob) (`RK/rightkit-credentials/src/namespace.rs:10-36, 224-234`).
- macOS backend on the legacy file-keychain generic-password API, bound to the user's default keychain (`src/macos.rs:1-46`).
- `get_or_create_key` returning standard base64, created add-if-absent (`src/store.rs:691-715`).
- `GuardMode::Deny`, a credential hand-off snapshot (`handoff`), `db_adapter`, rotation, OS authentication.

### 2.3 Behaviour differences

| # | Difference | Status |
|---|---|---|
| C1 | **Database key coordinates are identical.** macOS service `app.heardright.next`, account `SQLCipher/DBKey`. Windows `Slash{prefix:"HeardRight"}` gives target `HeardRight/SQLCipher/DBKey`, raw codec, `CRED_PERSIST_LOCAL_MACHINE` (`MAIN/src-tauri/src/credential_store.rs:23-37`; `RK/.../namespace.rs:227-229`; `RK/.../windows.rs:326-332`). | Verified |
| C2 | **Other items keep the same names.** The service and account constants are unchanged, and main uses `KeyringCompat` (`MAIN/src-tauri/src/secret_store.rs:34-38`, `license.rs:40-43`, `license/device.rs:17-18`, `l3_keys.rs:13-16`, `account_session.rs:24-25`, `license/removal.rs:25-26`). | Names verified. That `keyring-core`'s stores use exactly this target and blob layout rests on the RightKit doc comments; the keyring source is not in the tree, so not verified. |
| C3 | **macOS reads only the user default keychain.** RightKit never searches the keychain list (`RK/.../macos.rs:1-9`). The live `get_generic_password` searched the whole list. An item that lives only in a non-default keychain would now read as absent, and for the database key that leads to mint plus quarantine (D8). | Verified in code. User impact is a suspicion; rare. |
| C4 | **Minted key format is unchanged.** Standard base64 of 32 random bytes (`RK/.../store.rs:26-27, 684`), which passes HeardRight's stricter `validate_sqlcipher_key_for_pragma`. | Verified |
| C5 | **RNG failure no longer panics.** The live build panicked (`LIVE/.../credential_store.rs:226-230`); main returns an error. | Verified |
| C6 | **Developer keychain bypass removed.** `HR_DEV_NO_KEYCHAIN` (debug builds only) is gone. `RIGHTKIT_NO_KEYCHAIN` now fails closed through `GuardMode::Deny` (`MAIN/.../credential_store.rs:43-50`). No release effect. | Verified |
| C7 | **Linux gains a credential store** (Secret Service). The live build had none for the database key. No Linux product ships. | Verified |
| C8 | **BYOK provider keys reach the sidecar differently.** Main sends a `rightkit_credentials::handoff` snapshot over stdin (`MAIN/heardright-text-pipeline/src/provider_keys.rs`). How the live build passed them was not read. | Main side verified; live side not verified |
| C9 | **Windows persist type may change on rewrite.** RightKit writes every item as `CRED_PERSIST_LOCAL_MACHINE`. If the keyring store used a different persist type, a rewritten item changes it. Reads are unaffected. | Suspicion; not verified |

### 2.4 What main calls

RightKit only. No `keyring`, `security_framework` or `CredReadW` use remains in `src-tauri`.

- `credential_store.rs:30-51, 90, 110, 182`
- `secret_store.rs:56-79` (the single adapter)
- `host_services.rs:270-273`, `license/device.rs:57`, `l3_keys.rs:275-288`, `license/removal.rs:59-81`, `license/entitlement.rs:62`
- `heardright-text-pipeline/src/provider_keys.rs` (hand-off)
- `heardright-runtime/Cargo.toml:46` still depends on `security-framework` for `speech_service/peer_trust.rs`. That is code-signing peer trust, not credential storage.

### 2.5 Main code that duplicates RightKit

- The `KeyVault` trait and `OsKeyVault` in `credential_store.rs:226-292`, once key rotation moves to the RightKit lifecycle (section 1.5).
- The macOS in-process key cache (`credential_store.rs:53-75`) is app policy that avoids repeated keychain prompts. Keep it unless RightKit gains a cache.
- `secret_store.rs` (132 lines) is a thin adapter, not a duplicate.

### 2.6 Post-live changes

All look deliberate: one adapter for all items, add-if-absent minting, the tri-state key read (`db_key_state`), a staging account for rotation.

### 2.7 Verdict: right shape for HeardRight

Every live keychain item is addressed by the same service and account and read in place, and the Windows shapes for both the direct-call item and the keyring items are modelled explicitly. The only open point is the default-keychain-only read on macOS (C3), which is a deliberate hardening.

---

## 3. rightkit-fs

### 3.1 What the live app does

- **Root:** `dirs::data_local_dir()/HeardRightNext`, with an `HR_APP_DATA_DIR` environment override honoured in release builds and a fallback to `./HeardRightNext` if the OS gives no folder (`LIVE/heardright-telemetry/src/app_paths.rs:3-13`). The supervisor passed the root to the engine sidecar through that variable (`LIVE/heardright-app-runtime/src/engine_supervisor/supervisor.rs:307`).
- **Atomic writes:** three hand-written copies of temp file, fsync, replace. Windows used `ReplaceFileW`; Unix used rename plus a directory fsync (`LIVE/src-tauri/src/settings.rs:120-212`, `vocabulary.rs:192-239`, `heardright-telemetry/src/telemetry_outbox.rs:230-297`).
- **Hashing:** direct `sha2` in model fetch, download, diagnostics and sync.

### 3.2 What RightKit provides

- `app_dirs` (`AppId`, `AppDirs::resolve_with_options`, `without_org` for a flat folder), with bases taken from the account database and known-folder API, never from the environment (`RK/rightkit-fs/src/app_dirs.rs:1-50, 340-363`).
- `atomic` (`atomic_write`, `atomic_write_with_options`, `publish_file`, `rename_exclusive`), `replaced::read_replaced`, `digest`, `publish_dir`, `copy_tree`, locks, transactions, `restrict_owner_only`.

### 3.3 Behaviour differences

| # | Difference | Status |
|---|---|---|
| F1 | **Root is the same for a normal account.** `AppId::new("HeardRightNext").without_org()`, `dev(false)`, `.local` (`MAIN/heardright_core/src/app_paths.rs:25-71`). macOS `~/Library/Application Support/HeardRightNext`; Windows Local AppData known folder. | Verified |
| F2 | **`$HOME` is ignored on macOS and Linux.** RightKit takes home from `getpwuid_r` (`RK/.../app_dirs.rs:13-15, 352-355`). `dirs` used `$HOME` first. A user who launches with a redirected `HOME` resolves a different root after upgrade. | Verified in code. User impact is a suspicion; rare. |
| F3 | **`HR_APP_DATA_DIR` is no longer honoured.** Anyone relying on that override in a shipped build loses it. Each process now resolves the root itself (`MAIN/src-tauri/src/main.rs:17-18`, `MAIN/heardright-runtime/src/entry.rs:21-23`). | Verified |
| F4 | **Uninjected processes use a temp folder.** A process that never calls `set_app_data_root` gets `<temp>/heardright-data-<pid>/HeardRightNext` (`app_paths.rs:46-50, 74-80`). The desktop binary and the engine both inject. No injection was found in `src-tauri/src/lib.rs` or `mobile_run.rs`. | Desktop verified. Mobile entry is a suspicion; not verified, and it does not affect the Mac upgrade. |
| F5 | **No-folder fallback changed** from `./HeardRightNext` to a panic (`app_paths.rs:63-71`). | Verified |
| F6 | **Windows replace primitive changed** from `ReplaceFileW` to rename. The new file takes the temp file's inherited security descriptor, and the rename is not write-through (stated in `MAIN/src-tauri/src/atomic_replace.rs:9-18`). Unix is the same. | Verified from the wrapper comment and options; the Windows internals of rightkit-fs were not read. |
| F7 | **Windows readers retry a replace refusal.** Vocabulary loads through `read_replaced` (`MAIN/src-tauri/src/vocabulary.rs:89`). | Verified |
| F8 | **Legacy history backup.** `history.json.bak` is now written with an atomic private write and the source is then removed, instead of a rename (`MAIN/.../history/legacy_json_migration.rs:103-115`). The comment refers to "rightkit-fs 0.4.0 has no publish_file", but 0.4.12 has it. | Verified; stale workaround |
| F9 | **File names under the root.** `heardright.sqlite`, `history.json`, `cloud-sync-state.json` and the legacy sync ledger names are unchanged. Settings and vocabulary file formats were not compared (settings moved to `rightkit-app-settings`, outside this review). | Names verified; formats not verified |

### 3.4 What main calls

Mostly RightKit.

- `rightkit_fs::app_dirs` (`heardright_core/src/app_paths.rs:23, 64-65`)
- `atomic::atomic_write*` (about 20 sites; `src-tauri/src/atomic_replace.rs:22-26` is the single wrapper with 15 callers)
- `digest::sha256*` and `Sha256Hasher` (13 sites)
- `publish_file`, `publish_dir`, `copy_tree`, `rename_exclusive`, `replaced::read_replaced`

### 3.5 Main code that duplicates RightKit

| Main location | What it is |
|---|---|
| `heardright-runtime/src/cli.rs:415-424` | Hand-written `write_atomic` (temp, `sync_all`, rename) |
| `heardright-model-store/src/shared.rs:180` | Temp write plus rename |
| `src-tauri/src/history/recovery.rs:184-212, 296-315, 385-458` | `sync_dir`, `sync_parent_dir`, `move_no_clobber`, `same_file_identity` |
| `src-tauri/src/history/legacy_json_migration.rs:103-115` | Read, atomic private write, remove; replaceable by `publish_file` |
| `heardright-inference/src/coreml_asr/fused_probe.rs:561` | Own `sha256_path` |
| `heardright-model-store/src/ane_cache_prune.rs:36,147` | Still uses `dirs::cache_dir()`. `dirs = "7"` remains in the engine, model-store and telemetry manifests; only model-store was seen using it. |

The three live copies of `atomic_replace_file` and the live `sha256_file` and `hex_digest` are already gone.

### 3.6 Post-live changes

- Deliberate: the injected root (gate GC-A09), panic instead of the current directory, dropping the environment override, one atomic wrapper.
- Looks like a leftover: the `dirs` dependency and the stale `publish_file` workaround (F8).

### 3.7 Verdict: right shape with listed gaps

The resolver reproduces the live root for every normal account and removes three copies of atomic-write code. Gaps: redirected `HOME` and `HR_APP_DATA_DIR` installs move (F2, F3), the Windows replace semantics changed from `ReplaceFileW` (F6), and a few hand-written atomic and hash helpers and the `dirs` crate remain.

---

## 4. rightkit-http

### 4.1 What the live app does

- **`ureq 3.4.2`, blocking**, with default agents and only `timeout_global` set:

  | Call | Timeout | Location |
  |---|---|---|
  | Licence lease, account session, telemetry, feedback | 10 s | `LIVE/src-tauri/src/license/lease.rs:146-148, 391`, `account_session.rs:141-143`, `heardright-telemetry/src/telemetry_upload/schedule.rs:258, 316`, `feedback.rs:44` |
  | BYOK key validation | 5 s | `l3_keys.rs:76-77` |
  | Diagnostics upload | 30 s | `commands/diagnostics.rs:675` |
  | Model download command | 3600 s | `commands/models.rs:323` |
  | L3 cleanup | bucketed per request | `heardright-text-pipeline/src/l3_cleanup/transport.rs:189-206` |

- ureq defaults otherwise: environment proxy, bundled roots, 10 redirects, no retries, user agent `ureq/3.4.2`.
- **Model downloads:** own resumable engine with Range and If-Range, three attempts, 250 ms backoff, `.part` plus metadata, SHA-256 check (`LIVE/heardright-model-store/src/resumable_download.rs`, 508 lines).
- **Renderer:** `tauri-plugin-http` for `https://heardright.app/*` (`LIVE/src-tauri/capabilities/default.json:17-18`, `src/lib/license/core.ts`).
- **Update check:** `tauri-plugin-updater` against `https://api.spoares.com/...`.

### 4.2 What RightKit provides

`RK/rightkit-http/src/client.rs`:

- `Client` and `ClientConfig`. Defaults: 10 s connect, 60 s request, no redirects, platform trust roots, no proxy, retry policy on.
- `ClientConfig::legacy_ureq_parity(user_agent)`: environment proxy, bundled roots, 10 redirects, no retries (`client.rs:854-868`).
- `AddressPolicy`, public-web by default, which refuses private and loopback destinations (`netpolicy.rs:520-524`).
- `ProxyMode` None, Environment, Explicit, System. Secret headers, redaction, SSE, multipart, optional async client.

### 4.3 Behaviour differences

| # | Difference | Status |
|---|---|---|
| H1 | **Timeouts, proxy, TLS roots, redirects, retries and user agent are reproduced.** Main builds on `legacy_ureq_parity("ureq/3.4.2")` with one global timeout and the per-phase limits cleared (`MAIN/heardright-telemetry/src/http_client.rs:39-59`). Timeouts kept: 10 s removal, 5 s key validation, 30 s diagnostics (`license/removal.rs:138`, `l3_keys.rs:76`, `commands/diagnostics.rs:729`). | Verified |
| H2 | **New address policy.** Hosts that resolve to private or loopback addresses are refused unless explicitly allowed (`http_client.rs:40-43`). Through a proxy, single-label, internal and mDNS names are refused (`RK/.../netpolicy.rs:866-893`). ureq allowed all of these. Production hosts are public. | Verified in code. An effect on a user behind split DNS that maps a HeardRight host to a private address is a suspicion. |
| H3 | **Body cap.** Main sets a 10 MiB cap to match ureq's `read_to_string` limit (`http_client.rs:24, 50`). | Verified |
| H4 | **Unusable proxy variable.** Main retries with no proxy if the client cannot be built, matching ureq (`http_client.rs:54-58`). | Verified |
| H5 | **Model downloads moved to `rightkit-models 0.5.3`**, not rightkit-http directly (`MAIN/heardright-model-store/src/resumable_download.rs:10-13, 163-175`). The wrapper sets `retries: 0` on the transport and adds a "verified archive survives restart" step. Whether rightkit-models keeps the live three attempts, 250 ms backoff, and 412/416 restart rules was not read. | Wrapper verified; engine parity not verified |
| H6 | **Model fetch timeout.** `model_fetch.rs:147` uses a 900 s per-request timeout. The live value for that exact path was not compared (the live download command used 3600 s). | Not verified |
| H7 | **Renderer HTTP plugin removed on desktop.** Licence calls from TypeScript now go through native code and `rightkit-license` (`MAIN/src-tauri/Cargo.toml:209-229`). | Verified from the manifest; the TypeScript side was not read |
| H8 | **Update endpoint host changed** from `api.spoares.com` to `api.orthiclabs.com` (`tauri.conf.json`, both trees). Installed 0.1.213 builds keep polling the old host. This is not rightkit-http, but it decides whether live users receive the upgrade at all. | Verified |
| H9 | **Error text is reproduced by app code.** `ureq_style_error` maps RightKit errors back to ureq's strings (`http_client.rs:80-93`). | Verified |

### 4.4 What main calls

RightKit only for HTTP. No `ureq` or `reqwest` remains in any main manifest.

- Direct: `rightkit_http::Request` in `license/removal.rs:139`, `license/trial.rs:47`, `l3_keys.rs:81`, `commands/diagnostics.rs:730`, `heardright-telemetry/src/feedback.rs:92`, `heardright-model-store/src/model_fetch.rs:147`, `model_download_grant.rs:61`.
- Client construction: `heardright-telemetry/src/http_client.rs`.
- Through other RightKit crates: `rightkit-license` client (lease, account session), `rightkit-models` (downloads), `rightkit-llm::http` (L3 transport).

### 4.5 Main code that duplicates RightKit or itself

| Main location | Issue |
|---|---|
| `heardright-telemetry/src/http_client.rs:39-59`, `heardright-text-pipeline/src/l3_cleanup/transport.rs:340-361`, `heardright-model-store/src/resumable_download.rs:56-86` | Three copies of the same "ureq parity" client set-up, including the `"ureq/3.4.2"` constant three times. One shared constructor, or a RightKit preset that takes a single timeout, would remove two. |
| `http_client.rs:61-111` | `ureq_style_error`, `snake_case`, `status_display`: an app-owned text contract. Delete once logs and receipts stop depending on ureq wording. |
| `heardright-model-store/src/resumable_download.rs` | Now a 225-line shim; the engine duplicate is already gone. |

### 4.6 Post-live changes

- Deliberate: the public-web address policy, native-only desktop HTTP, kept user agent and error text, moving downloads to rightkit-models.
- Needs an owner decision: the update host change (H8).

### 4.7 Verdict: right shape with listed gaps

`legacy_ureq_parity` exists for exactly this move and main uses it to keep timeouts, proxy, roots, redirects and user agent. Gaps: the parity client is built three times in HeardRight, the ureq error-text shim remains, and the retry and resume parity of the download engine in rightkit-models is not verified here.

---

## 5. rightkit-sync and rightkit-crypto

### 5.1 What the live app does

- **CloudKit, not an own server.** Private database, container `iCloud.app.heardright.ios.native`, zone `HeardRightTranscripts`, record type `Transcript`.
- The app drives its own Swift helper `HeardRightCloudSync` over line-delimited JSON on stdin and stdout (`LIVE/src-tauri/src/cloud_sync.rs:1-28`; `LIVE/macos-native/HeardRightPillMac/Sources/HeardRightCloudSync/CloudSyncMain.swift`, 1,124 lines).
- Record fields: `id`, `text`, `rawText`, `source`, `tags`, `folder`, `createdAt`, `updatedAt`, `deletedAt` (`CloudSyncMain.swift:501-511, 683-722, 771-774`).
- Rust side: about 3,400 lines for merge, tombstone, alias and acknowledgement ledgers, backoff and a scheduler (`cloud_sync.rs` 1,464, `cloud_sync_ack.rs` 538, `cloud_sync_backoff.rs` 101, `cloud_sync_pending.rs` 82, `cloud_sync_scheduler.rs` 360).
- State files: `cloud-sync-state.json`, `cloud-sync-tombstones.json`, `cloud-sync-tombstones.pending.json`, `cloud-sync-aliases.json`.
- Consent is off by default (`cloud_sync.rs:50-70`).
- No client-side encryption of synced records.
- Crypto: own `base64_encode` (`LIVE/heardright_core/src/credential.rs:10`), `sha2`, and `ed25519-dalek` for lease envelopes.

### 5.2 What RightKit provides

- **rightkit-sync:** `SyncEngine<Mapper>` (change cursor, tombstones, aliases, last-writer-wins, conditional saves, backoff in the ledger), `helper::CloudKitHelper` speaking `rightkit-sync-helper/1` to the Swift `rightkit-cloudkit-helper`, `schedule` (`sync_once`, `next_delay`), `FileStorage` and `Ledger`, and optional `http`, `drive`, `cipher` and `vault` features. Field names `id`, `createdAt`, `updatedAt`, `deletedAt` (`RK/rightkit-sync/src/record.rs:9-12`).
- **rightkit-crypto:** `sha256`, `hmac_sha256`, `ct_eq`, hex, base64 and base64url, `canonical_json`, AEAD, KDF, signing, `Secret`.

### 5.3 Behaviour differences

| # | Difference | Status |
|---|---|---|
| S1 | **Same container, zone, record type and field names** (`MAIN/src-tauri/src/cloud_sync_core.rs:16-21, 82-125`). A record written by 0.1.213 or by the iOS app is readable by main and the reverse. | Field names verified. The helper's save policy and system-field handling in the RightKit Swift helper were not read. |
| S2 | **Helper binary replaced.** `HeardRightCloudSync` is replaced by `rightkit-cloudkit-helper` in a nested app bundle; the 1,124-line Swift file is gone from main. Timeout raised from 45 s to 120 s (`MAIN/.../cloud_sync_cloudkit.rs:25`). Main also checks the helper's signed CloudKit environment with `codesign` before use. | Verified |
| S3 | **Ledger migration is one-way.** On first run main imports the live tombstone, pending, alias and ack files into `cloud-sync-ledger.json` and `cloud-sync-local-versions.json`; the ledger's existence marks completion (`cloud_sync_core.rs:27-33, 494-517`; `cloud_sync_cloudkit.rs` `migrate_legacy`). The legacy files are not deleted. A downgrade to 0.1.213 resumes from the stale legacy ledgers. | Migration verified. Whether a stale ledger after downgrade resurrects deleted items is a suspicion. |
| S4 | **Consent file unchanged.** `cloud-sync-state.json` has the same name and stays off by default (`MAIN/src-tauri/src/cloud_sync.rs:25`). | Name verified; field compatibility of the scope binding not verified |
| S5 | **Provider seam added.** Main introduces a `SyncProvider` trait and a `sync_provider` setting (`icloud`, `google`, `off`). Only CloudKit is implemented; `active_provider` always returns CloudKit (`cloud_sync.rs:27-60`). | Verified |
| S6 | **Still no encryption of synced data.** rightkit-sync's `cipher` feature is not enabled. | Verified |
| S7 | **rightkit-crypto use is base64 only.** Five call sites; the output is the same standard padded base64 the live helper produced. | Verified |
| S8 | **HEAD of rightkit-sync removed things after 0.2.2** (`MemoryBackend`, the fake helper). Main is pinned to 0.2.2. | Verified from the changelog; the effect of moving the pin is not verified |

### 5.4 What main calls

- **rightkit-sync:** `SyncEngine`, `SyncEngineBuilder`, `FileStorage`, `Ledger`, `LocalStore` and related types (`cloud_sync.rs:16-19`); `helper::{CloudKitHelper, HelperConfig}` (`cloud_sync_cloudkit.rs:19-20`); the `RecordMapper` types (`cloud_sync_core.rs:13`); `PassOutcome` (`cloud_sync_scheduler.rs:22`).
- **Own code kept:** ids and the mapper, consent, the account gate, the deletion outbox, legacy migration, the scheduler, the pending-work set, and the native Swift observer bridge.
- **rightkit-crypto:** `heardright_core/src/credential.rs:8` (re-export), `src-tauri/src/app_icon.rs:105`, `commands/system.rs:81,125`, `renderer_events.rs:149`.

### 5.5 Main code that duplicates RightKit

| Main location | Lines | RightKit equivalent |
|---|---|---|
| `src-tauri/src/cloud_sync_scheduler.rs` | 340 | Partly `rightkit_sync::schedule` (`next_delay`, `sync_once`, `SchedulePolicy`); main imports only `PassOutcome`. Wake coalescing may be app-specific. |
| `src-tauri/src/cloud_sync_pending.rs` | 36 | Same question as above |
| `macos-native/.../CloudSyncNotifications.swift` | not read | `swift/RightKitSwift/Sources/RightKitCloudSync/CloudKitWakeHost.swift`; not verified |
| `cloud_sync_core.rs` legacy migration | about 150 | None; delete once no 0.1.213-era ledgers remain |
| `src-tauri/Cargo.toml:122` `base64 = "0.23.1"` | 1 | No `base64::` use found in `src-tauri` or `heardright_core` sources; looks unused. Tests not checked. |
| `src-tauri/Cargo.toml:93` `getrandom = "0.4"` | 1 | No `getrandom::` use found; key minting moved to rightkit-credentials. Looks unused. |
| `src-tauri/Cargo.toml:72` `ed25519-dalek = "3"` | 1 | Still direct for lease-envelope verification; rightkit-crypto has `sign`. Not verified whether rightkit-license now covers it. |
| `rightkit_fs::digest::Sha256Hasher` in `cloud_sync_core.rs`, `diagnostics.rs`, `coreml_asr/model.rs` | | Fine, but note two RightKit homes for SHA-256 (fs digest and crypto). |

### 5.6 Post-live changes

- Deliberate: the engine swap (about 3,400 lines down to about 2,000), the shared helper, the environment pin, the provider seam, legacy migration.
- Unfinished: the Google provider is declared in settings but not implemented (rightkit#162).

### 5.7 Verdicts

- **rightkit-sync: right shape with listed gaps.** Main really runs on the engine and the shared CloudKit helper, with the same container, zone and record fields, so cross-device compatibility is kept by reading. Gaps: the scheduler is still HeardRight's own, the ledger migration is one-way, and the Swift helper's save semantics against the live helper were not read.
- **rightkit-crypto: right shape for HeardRight.** HeardRight needs only base64 from it today and the output is identical. Two crate dependencies (`base64`, `getrandom`) look unused, and the direct `ed25519-dalek` use could move.

---

## Top gaps, in priority order

1. **Update host changed** from `api.spoares.com` to `api.orthiclabs.com` (H8). Installed 0.1.213 builds still poll the old host, so they only receive the upgrade if that host keeps serving. Outside these packages, but it gates everything else here.
2. **Database rotation and recovery are still HeardRight code** (about 1,200 lines in `history/key_rotation.rs`, `history/recovery.rs`, `history/sqlcipher_conn.rs`) while rightkit-db `encrypted-lifecycle` and the quarantine helpers exist. This is the largest remaining duplicate.
3. **Three copies of the ureq-parity HTTP client** inside HeardRight, plus the ureq error-text shim.
4. **App data root edge cases:** `HR_APP_DATA_DIR` and a redirected `$HOME` no longer move the root (F2, F3). Decide whether any shipped install relied on either.
5. **Windows atomic replace** changed from `ReplaceFileW` to rename, with different ACL inheritance (F6).
6. **macOS keychain read is default-keychain only** (C3). For the database key, an item elsewhere would lead to quarantine and an empty history.
7. **Sync ledger migration is one-way** (S3); a downgrade after upgrade is not covered.
8. **Leftovers:** `dirs`, `base64`, `getrandom` dependencies; the stale `publish_file` workaround; the stale `prepare` comment; stale `rightkit-mandatory.json` versions.

## Not verified

- Pinned RightKit releases versus HEAD, for all six packages.
- Exact SQLCipher versions on each side, and the Windows OpenSSL-to-LibTomCrypt open (claimed proven in rightkit#1447).
- `keyring-core` store layouts on macOS and Windows (source not in the trees).
- The download engine in `rightkit-models` against the live retry and resume rules.
- The Swift `rightkit-cloudkit-helper` save policy against the live `CloudSyncMain.swift`.
- Settings and vocabulary file formats, and the mobile entry's data root.
- Nothing was compiled or run; no database or keychain item was opened.
