VERDICT: DO NOT PUBLISH — new macOS ACL parent inspection can silently release caller-held record locks, allowing concurrent writers (`C:acl.rs:230`).

`B` = `fdad2f20`; `C` = `3c917803`. Unqualified source paths mean `crates/rightkit-fs/src/`.

**Earlier findings**

| Finding | Status & classification |
|---|---|
| Refused-lock unlink blocker | Closed; N fixed. Neither B nor C unlinks (`B:lock.rs:125`; `C:lock.rs:307`). |
| 1, 5, 13: Windows replacement/recovery | Closed by removal (`C:../CHANGELOG.md:22`). |
| 2: Alias registry | Improved, I; error-path exception below (`B:lock.rs:292`; `C:lock.rs:585`). |
| 3: Retry barriers | Open, disclosed I (`B:state.rs:434`; `C:state.rs:839`; `C:../README.md:38`). |
| 4: Atomic/settings parents | Open, disclosed I (`B:atomic.rs:88`; `C:atomic.rs:905`; `C:../README.md:62`). |
| 6: ACL fallback | Closed (`C:private.rs:259`). |
| 7: Windows creation window | Closed (`C:private.rs:552`). |
| 8: Copy staging | Closed (`C:clone.rs:488`). |
| 9: Reserved namespace | Open bypass, disclosed I (`B:atomic.rs:267`; `B/C:swap.rs:27`; `C:../README.md:68`). |
| 10: Settings assertion | Not applicable to fs publication. |
| 11: Non-private ACL exposure | Open, disclosed I (`B:atomic.rs:116`; `C:atomic.rs:495`; `C:../README.md:72`). |
| 12: Source permissions | Closed (`C:clone.rs:478`). |
| 14: Durability cache | Removed; underlying gap remains I, as finding 3 (`C:../CHANGELOG.md:23`). |
| 15: ACL scrubbing | Closed by refusal (`C:acl.rs:241`). |
| 16: Raw native clone privacy | Open, disclosed P (`B:clone.rs:100`; `C:clone.rs:128`; `C:../README.md:72`). |
| 17: Lifecycle coverage | Partial, I; ancestor tests exist, ACL race coverage remains absent (`C:../tests/state_ancestors.rs:104`; `C:../tests/acl_macos.rs:110`). |
| Subsequent SQLite lock regression | Closed for both functions (`B:private.rs:36`; `C:private.rs:53,81`; `C:../tests/private_keeps_locks.rs:48`). |

**Blocker — N: directory ACL checks open regular files**

- **Baseline:** `create_private_new(p.join("child"))` attempts exclusive creation directly; when `p` is regular, it fails without opening `p` (`B:private.rs:128–138`).
- **Candidate:** first calls `guard_parent_path`, whose unrestricted `File::open(parent)` opens & closes `p` before creation fails (`C:private.rs:204`; [acl.rs:230](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/acl.rs:230)). Directory postcreation inspection has equivalent unrestricted opening (`C:acl.rs:254`).
- **Reachable consumer:** inside `with_lock(p, …, closure)`, call `create_private_new(p.join("child"))`, handle its error, then continue protected updates (`C:lock.rs:779`). Closing ACL descriptor releases lock while guard remains alive. This also affects live SQLite files passed as parents; repository database connections remain live after hardening (`crates/rightkit-db/src/lib.rs:432–446`).
- **Invariant:** failed filesystem operations must preserve unrelated exclusion; otherwise concurrent writers can corrupt protected data.
- **Smallest fix:** require `O_DIRECTORY` atomically on both directory-only ACL opens. Add child-process contention regression for regular-file parents. Checking metadata *after* opening is insufficient.

**Every existing-path open**

“Yes” means closing can release same-process POSIX locks on that inode.

| Open site / public route | Lock consequence |
|---|---|
| ACL parent & created-directory inspection, `C:acl.rs:230,254` | **Yes**, if regular file supplied/substituted; blocker above. |
| `sync_dir`, `B:atomic.rs:226` → `C:atomic.rs:850` | **Yes** for regular-file argument/substitution; unchanged P. |
| `StateDir::read/read_opt`, `B:state.rs:495` → `C:state.rs:926` | **Yes**; unchanged P. |
| `FileKey::of_path`, `Fingerprint::of_path`, ledger read, `B/C:identity.rs:45,140,245` | **Yes** if same locked inode; unchanged P. Ledger normally uses separate lock (`identity.rs:240`). |
| File digests, `B:digest.rs:39` → `C:digest.rs:78` | **Yes**; unchanged P. Jobs forwards artifact paths here (`crates/rightkit-jobs/src/store.rs:2288,2590`). |
| Owner-record read, `B:lock.rs:425` → `C:lock.rs:800` | **Yes** if sidecar aliases locked inode; normally separate (`C:lock.rs:250`). |
| Fcntl acquisition, `C:lock.rs:299,585` | Successful aliases parked until final release (`591,677`); early-error exception below. |
| Flock acquisition, `C:lock.rs:705` | **Yes** for coexisting Fcntl locks; mixed kinds expressly discouraged (`C:../README.md:80`). |
| Clone ladder source, `C:clone.rs:380,465,529` | **Yes**; newly added source reads. Live-locked repository caller not established. |
| Digest commit, `C:atomic.rs:641` | Reopens own staging file; original descriptor already closed (`638`). |
| State directory opens/listings, `C:state.rs:708,1022`; directory enumerations `atomic.rs:1475`, `private.rs:229`, `identity.rs:419` | No regular-file open; directory operations. |
| Randomness, `B/C:identity.rs:473` | `/dev/urandom`, not regular state file. |
| Windows state opens, `C:state/windows.rs:237,317` | Pin/child/stat/read/rename/delete; no POSIX record-lock consequence. |
| Other-platform read, `C:state.rs:1187` | Excluded from Unix builds (`1098`). |

Exclusive-new opens cannot open existing destination inodes (`C:atomic.rs:707`; `private.rs:193,552`; `state.rs:952`; `state/windows.rs:469`).

**ACL correctness & consumers**

FFI matches SDK `sys/acl.h:102,142,199`: pointer result, path pointer, integer-compatible ACL type `0x100` (`C:acl.rs:58–77`). Apple implementation uses `lstatx_np`, without opening or following final symlink ([acl_file.c:87–115](https://github.com/apple-oss-distributions/Libc/blob/main/posix1e/acl_file.c#L87)). `ENOENT` also means absent ACL property ([filesec.c:141–144](https://github.com/apple-oss-distributions/Libc/blob/main/gen/filesec.c#L141)). Every non-null returned ACL reaches exactly one free (`C:acl.rs:134,163,193`).

Concurrent `restrict_owner_only`/`is_owner_only` now preserves FileLock descriptors & SQLite locks (`C:private.rs:53,81`). Database backup hardens after closing destination connection (`rightkit-db/src/backup.rs:66`); cache hardening & PDF inspection use corrected functions (`rightkit-cache/src/disk.rs:999`; `rightkit-pdf/src/commit.rs:210`). Settings & HTTP use separate std locks (`rightkit-settings/src/backend.rs:1130`; `rightkit-http/src/download.rs:972`); secrets migration reads directly (`rightkit-secrets/src/migrate.rs:169`).

**Notes, API & tests**

I: raced alias acquisition can fail ACL validation before parking, dropping existing exclusion (`B:lock.rs:292`; `C:lock.rs:311,585`). Public `FileLock::try_acquire` reaches this if another process links held inode into missing name. Baseline alias handling already failed; park before refusal.

Earlier durability, namespace & privacy residuals remain disclosed as table cites. No silent API break found; non-exhaustive `FsError` explicitly requires breaking-version publication despite manifest `0.3.4` (`C:../CHANGELOG.md:71`; `C:../Cargo.toml:3`).

Missing tests: competing-creator unlink refusal & postcreation ACL mutation (`README.md:72`; `tests/acl_macos.rs:179`). Lock-preservation test is sequential, not SQLite integration (`tests/private_keeps_locks.rs:48`).

Runtime reproduction, power-loss behavior & packaged-consumer equivalence: **not established**. Supplied CI treated as context. No builds, tests, or modifications performed.
