VERDICT: PUBLISH — legacy mutex restores mixed-version provisioning exclusion; no remaining finding meets your blocker definition (`C/src/windows.rs:239`, `C/src/locked.rs:62`).

References: **B** = published `21b58829`; **C** = candidate `3c917803`; **F** = requested `fdad2f20`, which identifies itself as **0.1.2**, not 0.1.3 (`F/Cargo.toml:3`). Paths below are relative to `crates/rightkit-secrets/`. Relevant Windows, namespace & migration baseline code is identical at B/F.

| Earlier finding | Status; classification | Candidate evidence |
|---|---|---|
| Mixed-version provisioning blocker | Closed; former **N** repaired | `src/windows.rs:239`; `src/locked.rs:62` |
| Timeout excludes resolution | Still open; **N**, unchanged | `src/store.rs:412` |
| Migration destination deletion | Still open; **I**, unchanged | `src/migrate.rs:103` |
| Undocumented settings adapter break | Closed by documentation | `CHANGELOG.md:13` |
| Undocumented CLI replacement break | Closed by documentation | `CHANGELOG.md:15` |
| 1 Explicit-keychain fallback | Remains closed by refusal/gating | `src/macos.rs:190,348` |
| 2 Reopened-store exclusion | Remains closed for reported alias | `src/macos.rs:584` |
| 3 Session-local locking | Candidate/candidate fixed; mixed-version limitation **I** | `src/windows.rs:119,239` |
| 4 Plaintext retirement | Remains closed by removal | `src/migrate.rs:198` |
| 5 Decoded prefixes | Remains closed | `src/keyring_compat.rs:48` |
| 6 Allocator-probe UB | Remains closed by replacement | `src/apple_helper.rs:70` |
| 8 Nondurable migration | Remains closed | `src/migrate.rs:45` |
| 9 Create-only import | Remains closed | `src/migrate.rs:302` |
| 10 macOS prefix alias | Remains closed | `src/macos.rs:576` |
| 11 Double resolution | Remains closed | `src/macos.rs:598` |
| 15 Overbroad write-path claim | Remains closed by narrowing | `CHANGELOG.md:54` |
| 7, 12–14; inherited other-crate findings | Not applicable | Prior review: `docs/audits/2026-10-12-astra-secrets-comparative.md:13,18,22` |

Closed findings have no remaining N/P/I issue unless specified.

**Blockers: none.**

**Legacy-name equivalence & exclusion**

Byte-for-byte match is established for identical accepted Namespace/account inputs:

- B/F `Namespace::new` retains supplied service unchanged; `service()` returns that stored string. Selecting Windows layout changes only layout, never service (`B/F/src/namespace.rs:51,59,63`; `C/src/namespace.rs:151,163,179`).
- Both hash UTF-8 bytes of `service + "\0" + account`, with BCrypt SHA-256, then encode first 16 digest bytes using lowercase, zero-padded `{b:02x}`. Both prepend `Local\rightkit-secrets-` & append one UTF-16 NUL (`B/F/src/windows.rs:22,45,63`; `C/src/windows.rs:43,125,131,150,267`). Prefix selection, SID & case normalization do not enter legacy hash.

**I — mixed-version exclusion remains limited.** Baseline holds legacy mutex **only inside `add_if_absent`**, across presence check & its internal `set`; standalone `set` & `delete` take no mutex (`B/F/src/windows.rs:152,181,191`). Candidate holds both across all four mutations (`C/src/locked.rs:46,56,70,75`).

Consequently, same-session, exact-service/account old `add_if_absent` cannot interleave with candidate `set`, `add_if_absent`, `delete` or `delete_if_equals`. This closes overlapping `Store::get_or_create_key("db", 32)` provisioning race (`B/src/store.rs:318`; `C/src/store.rs:532`).

Old standalone `set`/`delete` still interleave with every candidate operation. Old creators in another session, or using case/service aliases producing different legacy hashes, also remain unexcluded. Invariant: provisioning/conditional deletion must not overwrite or remove concurrent credentials. These gaps existed in baseline; candidate narrows them. Disclosure exists at `C/CHANGELOG.md:11,170`; README’s blanket old/new exclusion wording should specify **old `add_if_absent`, same session, exact names** (`C/README.md:39`).

**Lock order:** no cycle arises from these implementations. Candidate takes Global then Local; baseline takes process-local `ADD_LOCK` then Local, never Global; candidate never requests baseline `ADD_LOCK` (`B/F/src/windows.rs:181`; `C/src/windows.rs:239`). `WAIT_ABANDONED` marks ownership acquired, so ordinary guard cleanup applies (`C/src/windows.rs:213,244`). If second acquisition times out/errors, `?` drops first guard, releasing Global; unsuccessful Local guard closes its handle (`C/src/windows.rs:206,240,244`). Each wait is bounded separately—up to roughly 20 seconds combined, excluding setup (`C/src/windows.rs:41,239`).

**Other non-blocking notes**

- **N, unchanged timeout boundary:** B executes backend work inside timed worker (`B/src/store.rs:255`; `F/src/store.rs:232`). C resolves/pins synchronously before spawning worker (`C/src/store.rs:412,424`; `C/src/macos.rs:215,234,600`). Reachable through `Store::set_str` with `Config::timeout`; slow resolution violates whole-call deadline expectation. Indefinite native lookup hang is **not established**.
- **I, unchanged migration race:** B/F verifies destination then deletes source (`src/migrate.rs:47`). C adds durability, re-verification & conditional source deletion (`src/migrate.rs:100`). Windows `migrate_between` racing `to.delete(account)` after final verification can still lose both copies: source lock does not protect destination (`C/src/locked.rs:81`). Invariant: retirement preserves copied value. Candidate improves existing defect.

**Silent API breaks & tests**

No remaining silent break identified. Settings/CLI omissions are now documented; migration enums/reports, typed imports, fixture gating, validation/codec changes & `Error::InProgress` are documented (`C/CHANGELOG.md:13,32,67,75,89,153,163,175`). Changed `CredentialOps` remains crate-private (`C/src/locked.rs:24`).

Tests cover legacy-name vector & contention/timeout retry (`C/src/windows.rs:469,482`), provisioning (`tests/windows_lock.rs:38`), passive presence (`tests/e2e.rs:85`), migration refusal (`tests/store.rs:1346`), retained files (`src/migrate.rs:345`) & helper wiping/deadlines (`src/apple_helper.rs:680,796`).

Uncovered: actual old/new executable provisioning—legacy test emulates old holder; process test launches candidate only (`src/windows.rs:486`; `tests/windows_lock.rs:56`). Cross-session/DACL denial remain explicitly untested (`CHANGELOG.md:149`); CLI existing-value/`--replace`, stalled pin resolution & forced abandonment lack targeted coverage (`tests/e2e.rs:99`; `tests/store.rs:1153`; `src/windows.rs:482`).

**Not established:** independent CI results or real default-switch interleaving beyond injected fixtures (`C/src/macos.rs:982`). Supplied CI was context only. Review completed read-only, without builds, test execution or modifications.
