VERDICT: DO NOT PUBLISH — changed Windows mutex protocol introduces a mixed-version race that can overwrite newly provisioned database keys.

References: **B** = `21b58829`; **C** = `0f0395ee`; paths below are relative to `crates/rightkit-secrets/`. No builds, tests or modifications performed.

| Earlier finding | Status; N/P/I | Candidate evidence |
|---|---|---|
| 1 Explicit-keychain fallback/fixture exemption | Closed for production by refusal & feature gating | `C/src/macos.rs:190,312,348` |
| 2 Reopened-store exclusion | Closed for reported alias | `C/src/store.rs:232,416`; `C/tests/store.rs:572` |
| 3 Windows session-local mutex | Original finding closed; **N** transition regression below | `C/src/windows.rs:114,151` |
| 4 Plaintext retirement race | Closed by removal | `C/src/migrate.rs:198,228` |
| 5 Unwiped decoded prefixes | Closed | `C/src/keyring_compat.rs:48` |
| 6 Allocator-probe UB | Closed by replacement | `C/src/apple_helper.rs:70` |
| 7 License test weaknesses | Not applicable to this crate | Earlier review: `docs/audits/2026-10-11-astra-secrets-fifth-read.md:13` |
| 8 Migration into nondurable destination | Closed for reported loss path | `C/src/migrate.rs:45,100`; `C/tests/store.rs:1346` |
| 9 Create-only environment import | Closed | `C/src/migrate.rs:302`; `C/tests/store.rs:1552` |
| 10 macOS Windows-prefix alias | Closed | `C/src/macos.rs:576`; `C/tests/store.rs:1005` |
| 11 Default resolved separately for lock/write | Closed for reported double-resolution path | `C/src/store.rs:412,425`; `C/src/macos.rs:598`; `C/tests/store.rs:1259,1295` |
| 12 PDF replacement | Not applicable | Earlier review: line 43 |
| 13 Directory durability cache | Not applicable | Earlier review: line 46 |
| 14 Proxy bypass | Not applicable | Earlier review: line 49 |
| 15 Overbroad regression-check claim | Closed by narrowing scope; coverage limits below | `C/CHANGELOG.md:43`; `C/tests/store.rs:1458` |
| Inherited Process/HTTP/FS findings | Not applicable to this crate review | Earlier review: lines 18–36 |

**Blocker — N: mixed-version create-only writes lose exclusion**

- **Baseline:** identical service/account callers within one Windows session share `Local\rightkit-secrets-<hash>`, held across presence check & write (`B/src/windows.rs:45,64,184`). Two baseline processes therefore serialize creation.
- **Candidate:** uses exclusively `Global\rightkit-secrets-t-<different hash>` (`C/src/windows.rs:116,136,148`), with separate check/write under that mutex (`C/src/locked.rs:61`). It never acquires baseline mutex.
- **Reachable path:** during overlapping old/new app processes, both call `Store::get_or_create_key("db", 32)` for identical namespace, user & session. Both read absence; each acquires its different mutex; both presence checks succeed; old process writes/returns key A; candidate writes/returns key B. Return paths trust successful creation (`B/src/store.rs:318`; `C/src/store.rs:532`).
- **Invariant:** concurrent provisioning must return one persistent key. Data encrypted using A becomes unrecoverable after A disappears from memory because credential now holds B.
- **Smallest repair:** retain new mutex & additionally acquire exact legacy mutex, consistently ordered, across candidate mutations. Add mixed-version creation regression coverage.

This limitation is disclosed at `C/CHANGELOG.md:120`, but disclosure does not exempt an **N** with irreversible harm under your rule.

**Notes**

- **N, timeout boundary:** baseline performs backend operation inside timed worker (`B/src/store.rs:255`). Candidate calls `pinned()` & identity resolution synchronously before worker (`C/src/store.rs:412,424`), including macOS native/path lookups (`C/src/macos.rs:215,234,600`). Consumer path: `Store::set_str` with `Config::timeout`. Invariant: operation cap should cover resolution too. Slow resolution exceeds configured cap; an actual indefinitely blocked native lookup is **not established**, so this remains non-blocking.
- **I, migration transaction remains incomplete:** baseline verifies destination then deletes source (`B/src/migrate.rs:47`). Candidate adds durability, another verification & conditional source deletion (`C/src/migrate.rs:100`). During Windows `migrate_between`, concurrent `to.delete(account)` after final verification can still leave neither copy; source-only lock does not protect destination (`C/src/locked.rs:80`). Invariant: source retirement preserves copied value. Candidate narrows existing race; it does not introduce it.

**Silent API breaks & test coverage**

Documented changes include `FileMigration`, migration reports/durability, typed imports, fixture gating, encoding/validation & `Error::InProgress` (`C/CHANGELOG.md:21,56,64,78,142,152,164`).

Two omissions from CHANGELOG:

- **N, adapter compatibility:** `settings-probe` switches from settings 0.1 to 0.2 (`B/Cargo.toml:28`; `C/Cargo.toml:33`). Consumers requiring `rightkit_settings` 0.1’s `SecretProbe` no longer receive that trait implementation (`B/src/store.rs:332`; `C/src/store.rs:546`). Invariant: public adapter type compatibility.
- **N, CLI behavior:** repeated `set` previously replaced (`B/src/bin/rightkit-secrets.rs:74`); now requires `--replace` (`C/src/bin/rightkit-secrets.rs:78`). Existing scripts silently leave old credentials. README documents this (`C/README.md:51`); CHANGELOG does not.

Tests cover passive presence, provisioning, timeout exclusion, guarded migration, retained files & helper decoding/deadlines (`C/tests/e2e.rs:85,150`; `C/tests/store.rs:423,1346`; `C/src/migrate.rs:345`; `C/src/apple_helper.rs:680,796`).

Untested promises/branches: mixed-version exclusion—existing test launches only itself (`C/tests/windows_lock.rs:56`); cross-session locking/access-denied squatting explicitly untested (`C/CHANGELOG.md:138`); CLI existing-value/`--replace` behavior—test only creates fresh item (`C/tests/e2e.rs:99`); stalled pin resolution—fixture returns immediately (`C/tests/store.rs:1153`).

**Not established:** native lookup hang, real default-switch interleaving beyond injected tests (`C/src/macos.rs:982`), or independent CI receipt verification. Supplied successful CI results were accepted as review premises.

Publication requires clearing mixed-version key-loss blocker.
