1. **P1 — macOS explicit-keychain isolation still races.** [macos.rs:282](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:282): `open_file(K)` passes because K is currently default; another process switches default to L & deletes K before line 293. Native creation can then store secret in L, consistent with [Apple’s fallback implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/SecKeychainItem.cpp#L98). Public fixture mode also bypasses destination matching. **Smallest fix:** refuse explicit-file creation unconditionally until creation is destination-bound; remove production fixture bypass. Mock adds always succeed at line 963, so cannot detect this race.

2. **P1 — Reopening Store bypasses timeout protection.** [store.rs:370](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/store.rs:370): exclusion uses `Arc` address; [backend.rs:215](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/backend.rs:215) creates fresh instances. Windows interleaving: A’s worker pauses before acquiring native lock; caller times out; reopened Store writes B successfully; A resumes & overwrites B. Current regression shares one backend. **Smallest fix:** wait for mutation completion instead of abandoning it; alternatively introduce stable backend/credential identity & observable completion.

3. **P1 — Windows locks cover less than credential scope.** [windows.rs:67](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/windows.rs:67) uses session-local mutexes, while line 188 writes machine-persistent credentials. [Microsoft documents](https://learn.microsoft.com/en-us/windows/win32/api/wincred/ns-wincred-credentialw) visibility across same-user logon sessions; [`Local\` mutexes remain session-specific](https://learn.microsoft.com/en-us/windows/win32/termserv/kernel-object-namespaces). Two sessions can both observe absence, then overwrite each other; conditional migration deletion can similarly delete replacement B after comparing A. **Smallest fix:** global mutex keyed by user SID & native target, with appropriate access control.

4. **P1 — Plaintext migration still destroys unpreserved data.** [migrate.rs:124](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:124): read file A; another writer replaces it with B; line 149 wipes/deletes B after verifying only destination A. Separately, `GuardMode::Memory` permits copying into ephemeral memory & deleting the sole persistent source. **Smallest fix:** reject destructive migration when guarded; retain source until migration & file writers share serialization. Existing test exercises only uncontended migration.

5. **P2 — Windows decoding leaves unwiped secret prefixes.** [keyring_compat.rs:43](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/keyring_compat.rs:43): UTF-16LE `"secret"` followed by unpaired `0xD800` allocates decoded prefix in ordinary `String`, then frees it on error. [Rust implementation](https://doc.rust-lang.org/src/alloc/string.rs.html#723) also grows this buffer without wiping replaced allocations. **Smallest fix:** decode into preallocated `Zeroizing<String>` sized for worst-case UTF-8 expansion.

6. **P2 — New zeroization probe invokes undefined behavior.** [apple_helper.rs:669](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:669): allocator scans entire allocation, including uninitialized capacity/padding. Dropping `Vec::<u8>::with_capacity(64)` while armed suffices. [`from_raw_parts` requires initialized elements](https://doc.rust-lang.org/std/slice/fn.from_raw_parts.html). **Smallest fix:** inspect explicitly initialized secret buffers through controlled drop hooks, rather than arbitrary heap allocations.

7. **P3 — Clock concurrency tests overstate coverage.** [tests/clock.rs:710](/Volumes/D/claude/rightkit/crates/rightkit-license/tests/clock.rs:710) ignores every failed `trusted_now`; returning errors universally still passes final inequality. [tests/file_store.rs:226](/Volumes/D/claude/rightkit/crates/rightkit-license/tests/file_store.rs:226) names two processes but creates two objects in one process. **Smallest fix:** assert successful observations & use actual child processes.

Previous read’s numbered findings:

| Previous finding | Status | Current source evidence |
|---|---|---|
| 1. Nonatomic clock CAS default | **FIXED** | [clock.rs:321](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:321) requires implementation; [file_store.rs:197](/Volumes/D/claude/rightkit/crates/rightkit-license/src/file_store.rs:197) locks comparison/write. |
| 2. macOS fallback | **PARTLY FIXED** | [macos.rs:282](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:282); finding 1. |
| 3. Windows competing mutators | **PARTLY FIXED** | [locked.rs:50](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/locked.rs:50); finding 3. |
| 4. Ambiguous Unicode codec | **FIXED** | [namespace.rs:142](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/namespace.rs:142); explicit codec, U+4142 regression. |
| 5. Migration deletes replacement | **PARTLY FIXED** | [migrate.rs:65](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:65); conditional deletion, Windows scope defect remains. |
| 6. Timed-out mutations | **PARTLY FIXED** | [store.rs:368](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/store.rs:368); finding 2. |
| 7. NUL target prefix | **FIXED** | [namespace.rs:153](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/namespace.rs:153); backend validation & victim-preservation regression. |
| 8. Helper decoding copies | **FIXED** | [apple_helper.rs:184](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:184); preallocated zeroizing decoder. |
| 9. Helper timeout gaps | **FIXED** | [apple_helper.rs:459](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:459); deadline covers pipe operations, child killed/reaped. |

First-read findings carried forward:

| Finding | Status | Current source |
|---|---|---|
| Throttled time forgotten | **FIXED** | [clock.rs:586](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:586) |
| Stale commits undo expiry/replay | **FIXED** | [clock.rs:628](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:628) |
| Poisoned process anchor | **FIXED** | [clock.rs:726](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:726) |
| Failed replacement destroys credential | **FIXED** | [macos.rs:464](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:464) |
| Wildcard-account deletion | **FIXED** | [macos.rs:578](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:578) |
| Different-length equality | **FIXED** | [secret.rs:32](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/secret.rs:32) |
| Native add fallback | **PARTLY FIXED** | [macos.rs:293](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:293) |
| Helper JSON copies | **FIXED** | [apple_helper.rs:184](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:184) |
| Resolver failure type | **FIXED** | [macos.rs:180](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:180) |

Source/test review at HEAD `48f8fa01`; no builds, tests, repository scripts or modifications.

**DO NOT PUBLISH this batch. Blocking findings: 1–4, all in rightkit-secrets. Findings 5–6 should fix; 7 is coverage debt.**