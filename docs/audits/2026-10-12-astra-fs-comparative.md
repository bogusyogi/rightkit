VERDICT: DO NOT PUBLISH — new lock-cleanup race can delete another process’s active lock, permitting concurrent writers & silent corruption (`C:src/lock.rs:270–310`).

`B` = `fdad2f20`; `C` = `0f0395ee`. Paths below are relative to `crates/rightkit-fs`. “—” means finding closed or outside this crate; N/P/I classify remaining defects.

| Earlier finding | Status | N/P/I & evidence |
|---|---|---|
| 1. Partial Windows replacement | Closed by removal | —; B used rename (`src/atomic.rs:179`); C selects rename/move/exclusive rename (`src/atomic.rs:1056`). |
| 2. Cross-inode alias cache | Closed | —; B registry was path-keyed (`src/lock.rs:292`); C keys opened inode & parks descriptors (`src/lock.rs:586`). |
| 3. Retry barriers | Still open, disclosed | I; B created without ancestor sync (`src/state.rs:434`); C syncs newly created links only (`src/state.rs:839`). |
| 4. Atomic/settings parents | Filesystem portion still open, disclosed | I; B `src/atomic.rs:88`; C `src/atomic.rs:901`. Settings itself outside scope. |
| 5. Private ReplaceFile exposure | Closed by removal | —; `C:CHANGELOG.md:7`, `C:src/atomic.rs:1056`. |
| 6. Weak ACL fallback | Closed | —; B fallback `src/private.rs:172`; C propagates native failure (`src/private.rs:265`). |
| 7. Windows creation window | Closed | —; B created then hardened (`src/private.rs:138`); C creates with security descriptor (`src/private.rs:553`). |
| 8. Permissive copy temp | Closed | —; ladder absent from B (`src/clone.rs:81`); C selects private Unix staging (`src/clone.rs:488`). |
| 9. Reserved namespace | Alias defect closed; bypass remains disclosed | I; B swept marker matches (`src/atomic.rs:267`); C normalizes names (`src/atomic.rs:806`), but swap bypass remains (`README.md:66`). |
| 10. Settings flag assertion | Not applicable | —; separate crate, identified in fifth review at line 14. |
| 11. macOS ACL exposure | Improved; non-private path remains | I; B mode-only creation (`src/atomic.rs:116`); C checks explicit private options (`src/atomic.rs:491`). |
| 12. Stale source permissions | Closed | —; ladder absent from B; C derives mode from opened source (`src/clone.rs:478`); regression test `tests/clone_fallback.rs:454`. |
| 13. Swept recovery content | Closed by removal | —; `C:CHANGELOG.md:7`, `C:src/atomic.rs:1056`. |
| 14. Positive durability cache | Cache removed; underlying gap remains disclosed | I; B `src/state.rs:434`; C `src/state.rs:839`; `C:README.md:38`. |
| 15. ACL scrub/open handles | Original defect closed by refusal | —; B lacked checks (`src/atomic.rs:116`); C refuses before filling (`src/atomic.rs:515`). Separate cleanup regression below. |
| 16. Native clone ACL bypass | Raw API still open, disclosed; ladder improved | P; B `src/clone.rs:100`, C `src/clone.rs:128`; disclosure `README.md:70`; ladder guard `src/clone.rs:346`. |
| 17. Lifecycle coverage | Partly closed | I; moved-directory tests now exist (`tests/state_ancestors.rs:104`); ACL interleaving coverage remains missing below. |

**Blocker — N: ACL rejection can unlink somebody else’s lock**

- **Baseline:** `open_lock_file` uses non-truncating `create(true)` & never removes lock path (`B:src/lock.rs:125–135`). Contention reaches record-lock acquisition (`B:src/lock.rs:306–310`).
- **Candidate:** [lock.rs:270](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lock.rs:270) remembers whether path was missing; [line 283](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lock.rs:283) still uses nonexclusive `create(true)`. If ACL inspection fails, [line 309](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lock.rs:309) removes that pathname without establishing this call created it.
- **Reachable path:** On macOS, process A calls `FileLock::try_acquire(p)` (`C:src/lock.rs:352`), observes absence & passes parent check. Before A opens, B creates `p` with explicit ACL `allow` entry & acquires it; existing files bypass creation ACL checks (`C:src/lock.rs:305`). A opens B’s inode, rejects its ACL & unlinks it. C can then acquire newly created `p` while B still holds original inode (`C:src/lock.rs:585–607`).
- **Invariant:** Failed acquisition must preserve another holder’s coordination file; otherwise consumers using `with_lock` for read-modify-write can silently lose updates (`C:src/lock.rs:779`). README explicitly promises lock files are never deleted (`C:README.md:81`).

**Smallest clearing change:** Remove pathname deletion from lock ACL-error cleanup; retain refusal. Any later cleanup needs proven exclusive creation. Add deterministic competing-creator regression.

**Non-blocking notes**

- **I durability:** `StateDir::create` retry or durable `AtomicWriter` with existing/symlinked parents cannot establish every ancestor link’s durability. B synced none; C syncs newly created links. Invariant: acknowledged pathname survives crash. Explicitly disclosed (`B:src/state.rs:434`; `B:src/atomic.rs:88`; `C:src/atomic.rs:901–934`; `C:README.md:38,62`).
- **P privacy:** `clone_file_nofollow(private_source, acl_parent/dst)` retains inherited-ACL exposure; both versions invoke native clone directly. Invariant: private bytes remain private. README discloses exception (`B:src/clone.rs:100`; `C:src/clone.rs:128`; `C:README.md:70`).
- **I namespace:** `swap_paths(record, generated_temp)` followed by sweeping can delete moved record in both versions; invariant is committed-record preservation. Swap remains unchanged & exception disclosed (`B/C:src/swap.rs:27`; `B:src/atomic.rs:267`; `C:README.md:66`).

**Silent API breaks & untested promises**

No silent source-signature break found. Documented **N** compatibility break: exhaustive `FsError` matches accepted by B fail against C’s non-exhaustive enum (`B:src/error.rs:7`; `C:src/error.rs:30`). CHANGELOG explicitly requires breaking-version publication, while manifest remains `0.3.4` (`C:CHANGELOG.md:56`; `C:Cargo.toml:3`).

Tests cover publication ordering, checked commits, namespace rejection, aliases, ancestor creation & private-source fallback (`tests/atomic_publish.rs:39`; `atomic_checked_sync.rs:15`; `atomic_namespace.rs:156`; `lock_alias.rs:238`; `state_ancestors.rs:31`; `clone_fallback.rs:419`). README’s postcreation ACL-race promise lacks deterministic mutation/retained-reader coverage; existing ACL tests establish ACLs before calls (`README.md:70`; `tests/acl_macos.rs:110,235`).

**What could not be established**

Runtime reproduction of identified race, power-loss behavior & registry-package equivalence were not established. Supplied CI result was accepted; no builds, tests or modifications performed.
