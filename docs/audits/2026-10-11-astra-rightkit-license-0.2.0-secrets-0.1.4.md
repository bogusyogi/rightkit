# Astra read of rightkit-license 0.2.0 and rightkit-secrets 0.1.4 (working tree), 2026-10-11

Read-only review before publication. Verdict for both: do not publish. Findings verbatim:

license 0.2.0**

- **P1 — Throttling forgets trusted time.** [clock.rs:435](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:435). With persisted mark `1000` & monotonic time lagging after sleep, evaluation at wall `1050` expires token with `exp=1040`; commit skips persistence because advancement is below 60 seconds. Move wall to `1020` while monotonic remains below it: next evaluation returns Active. Keep every observation in memory; throttle only durable writes.

- **P1 — Stale commits can undo expiry & replay protection.** [clock.rs:431](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:431). Reader A prepares `next=1000` from prior `900`; reader B commits `1100`; A subsequently saves `1000` without checking current state. Restart at wall `1001` then accepts token expiring at `1050`, despite previously trusting `1100`. Interleaving with `reanchor` also overwrites newer `last_server_iat`. Load/check/save needs serialization or compare-and-swap across shared ledger users.

- **P2 — Clock repair leaves poisoned process anchor intact.** [clock.rs:457](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:457), [clock.rs:190](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:190). Start process with clock far ahead, correct wall time, then successfully reanchor using fresh server time. Next reconciliation takes maximum with original, still-ahead monotonic anchor, immediately restoring inflated time & expiring valid licenses. Reanchor must also repair process time reference.

Lease canonicalization covers exactly seven requested fields in requested order; base64url uses no-pad decoding. Token signatures cover raw payload bytes. No canonicalization bypass, ordinary malformed-input panic, or expiry/grace integer wrap identified. Grace uses saturating addition & excludes trials/provisional lifetime.

Nested entitlement remains unsigned by envelope, explicitly named `unverified_entitlement`; lease expiry, device binding & generation ordering remain caller responsibilities. Plain Ed25519 verification permits weak public keys by design; no bypass identified assuming correctly provisioned, non-weak key. [Dalek documentation](https://docs.rs/ed25519-dalek/latest/ed25519_dalek/struct.VerifyingKey.html).

**Verdict: do not publish.**

**B — rightkit-secrets 0.1.4**

Inherited findings below already exist at HEAD; they are publication risks, not regressions introduced by keychain scoping.

- **P1 — Failed replacement can destroy existing credential — inherited.** [macos.rs:309](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:309). Interactive update gets `AUTH_FAILED`/`READ_ONLY`, deletes existing item, then attempts add. If add fails or process exits between operations, previous credential is permanently lost. Return original update failure instead of destructive replacement.

- **P1 — Public backend permits wildcard-account deletion — inherited.** [macos.rs:212](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:212), [macos.rs:345](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:345). Call exported backend directly with empty account: zero account length omits account filtering in Apple’s implementation, so delete can remove first matching service item. `Store` validates accounts, but public `Backend` path bypasses it. [Apple source](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/SecKeychain.cpp#L761).

- **P1 — Migration equality check accepts different-length secrets — inherited.** [secret.rs:30](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/secret.rs:30), [migrate.rs:48](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:48). Length XOR is truncated to `u8`: matching-prefix secrets of lengths 32 & 288 compare equal. If destination readback loses 256 bytes or concurrent replacement produces that prefix, migration accepts verification & deletes original. Compare full lengths before byte comparison.

- **P1 — Explicit keychain does not eliminate native add fallback — inherited API behavior.** [macos.rs:236](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:236). Open explicit keychain A, make A unavailable, then add while default B remains available: Apple’s published implementation catches missing explicit keychain & retries through `defaultKeychainUI`, potentially storing secret in B. Same risk exists if resolved default disappears before add. This is source-derived, not runtime reproduced. [Apple add implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/SecKeychain.cpp#L739), [fallback implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/StorageManager.cpp#L2141).

- **P2 — Helper JSON copies remain unwiped — inherited.** [apple_helper.rs:67](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:67), [apple_helper.rs:127](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:127). Write creates ordinary `Value::String` containing secret; read parses another, then copies into `Secret`. Those JSON allocations are freed without zeroization, despite transport strings using `Zeroizing`.

- **P3 — Resolver failure type violates stated contract — new.** [macos.rs:137](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:137). Resolver returning any status outside two missing-keychain codes produces `Backend`, `Denied`, or `InteractionRequired`, rather than guaranteed `Unavailable`; success-with-null produces `Backend { code: 0 }`. All fail closed, but typed unavailable handling misses these cases.

Normal find/update/delete paths retain one explicit keychain handle throughout operation; no remaining null-search-list call found. CF releases & copied-buffer lifetimes show no evident use-after-free. Apple’s content-free implementation uses sensitive allocator, so absence of explicit Rust wiping alone does not establish macOS buffer leakage. [Apple cleanup source](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/Item.cpp#L1300).

Windows remains unchanged: `CredWriteW` replaces directly, `CredDeleteW` targets named credential, & presence calls `CredReadW`, fetching native blob before freeing it. Rust copies are zeroized; native allocation gets no explicit wipe, & [CredFree documentation](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credfree) promises freeing without specifying scrubbing. Linux OS backend consistently returns `Unavailable`.

Reading alone cannot establish deployed Security.framework behavior, native buffer scrubbing, ACL/prompt outcomes, concurrent runtime behavior, signer interoperability beyond inspected test source, or consumer enforcement. No builds/tests were run.

**Verdict: do not publish.**
tokens used
123,658
Compared `git diff HEAD -- <crate>` plus untracked license modules. No edits, execution, or real credential/key/token access.

**A — rightkit-license 0.2.0**

- **P1 — Throttling forgets trusted time.** [clock.rs:435](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:435). With persisted mark `1000` & monotonic time lagging after sleep, evaluation at wall `1050` expires token with `exp=1040`; commit skips persistence because advancement is below 60 seconds. Move wall to `1020` while monotonic remains below it: next evaluation returns Active. Keep every observation in memory; throttle only durable writes.

- **P1 — Stale commits can undo expiry & replay protection.** [clock.rs:431](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:431). Reader A prepares `next=1000` from prior `900`; reader B commits `1100`; A subsequently saves `1000` without checking current state. Restart at wall `1001` then accepts token expiring at `1050`, despite previously trusting `1100`. Interleaving with `reanchor` also overwrites newer `last_server_iat`. Load/check/save needs serialization or compare-and-swap across shared ledger users.

- **P2 — Clock repair leaves poisoned process anchor intact.** [clock.rs:457](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:457), [clock.rs:190](/Volumes/D/claude/rightkit/crates/rightkit-license/src/clock.rs:190). Start process with clock far ahead, correct wall time, then successfully reanchor using fresh server time. Next reconciliation takes maximum with original, still-ahead monotonic anchor, immediately restoring inflated time & expiring valid licenses. Reanchor must also repair process time reference.

Lease canonicalization covers exactly seven requested fields in requested order; base64url uses no-pad decoding. Token signatures cover raw payload bytes. No canonicalization bypass, ordinary malformed-input panic, or expiry/grace integer wrap identified. Grace uses saturating addition & excludes trials/provisional lifetime.

Nested entitlement remains unsigned by envelope, explicitly named `unverified_entitlement`; lease expiry, device binding & generation ordering remain caller responsibilities. Plain Ed25519 verification permits weak public keys by design; no bypass identified assuming correctly provisioned, non-weak key. [Dalek documentation](https://docs.rs/ed25519-dalek/latest/ed25519_dalek/struct.VerifyingKey.html).

**Verdict: do not publish.**

**B — rightkit-secrets 0.1.4**

Inherited findings below already exist at HEAD; they are publication risks, not regressions introduced by keychain scoping.

- **P1 — Failed replacement can destroy existing credential — inherited.** [macos.rs:309](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:309). Interactive update gets `AUTH_FAILED`/`READ_ONLY`, deletes existing item, then attempts add. If add fails or process exits between operations, previous credential is permanently lost. Return original update failure instead of destructive replacement.

- **P1 — Public backend permits wildcard-account deletion — inherited.** [macos.rs:212](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:212), [macos.rs:345](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:345). Call exported backend directly with empty account: zero account length omits account filtering in Apple’s implementation, so delete can remove first matching service item. `Store` validates accounts, but public `Backend` path bypasses it. [Apple source](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/SecKeychain.cpp#L761).

- **P1 — Migration equality check accepts different-length secrets — inherited.** [secret.rs:30](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/secret.rs:30), [migrate.rs:48](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:48). Length XOR is truncated to `u8`: matching-prefix secrets of lengths 32 & 288 compare equal. If destination readback loses 256 bytes or concurrent replacement produces that prefix, migration accepts verification & deletes original. Compare full lengths before byte comparison.

- **P1 — Explicit keychain does not eliminate native add fallback — inherited API behavior.** [macos.rs:236](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:236). Open explicit keychain A, make A unavailable, then add while default B remains available: Apple’s published implementation catches missing explicit keychain & retries through `defaultKeychainUI`, potentially storing secret in B. Same risk exists if resolved default disappears before add. This is source-derived, not runtime reproduced. [Apple add implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/SecKeychain.cpp#L739), [fallback implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/StorageManager.cpp#L2141).

- **P2 — Helper JSON copies remain unwiped — inherited.** [apple_helper.rs:67](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:67), [apple_helper.rs:127](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:127). Write creates ordinary `Value::String` containing secret; read parses another, then copies into `Secret`. Those JSON allocations are freed without zeroization, despite transport strings using `Zeroizing`.

- **P3 — Resolver failure type violates stated contract — new.** [macos.rs:137](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:137). Resolver returning any status outside two missing-keychain codes produces `Backend`, `Denied`, or `InteractionRequired`, rather than guaranteed `Unavailable`; success-with-null produces `Backend { code: 0 }`. All fail closed, but typed unavailable handling misses these cases.

Normal find/update/delete paths retain one explicit keychain handle throughout operation; no remaining null-search-list call found. CF releases & copied-buffer lifetimes show no evident use-after-free. Apple’s content-free implementation uses sensitive allocator, so absence of explicit Rust wiping alone does not establish macOS buffer leakage. [Apple cleanup source](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_keychain/lib/Item.cpp#L1300).

Windows remains unchanged: `CredWriteW` replaces directly, `CredDeleteW` targets named credential, & presence calls `CredReadW`, fetching native blob before freeing it. Rust copies are zeroized; native allocation gets no explicit wipe, & [CredFree documentation](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credfree) promises freeing without specifying scrubbing. Linux OS backend consistently returns `Unavailable`.

Reading alone cannot establish deployed Security.framework behavior, native buffer scrubbing, ACL/prompt outcomes, concurrent runtime behavior, signer interoperability beyond inspected test source, or consumer enforcement. No builds/tests were run.

**Verdict: do not publish.**

