VERDICT: PUBLISH — source comparison establishes no remaining blocker under supplied definition; directory-open regression is fixed & remaining lock-loss limitations are disclosed (`C:acl.rs:255`; `C:atomic.rs:856`; `C:CHANGELOG.md:6`).

`B` = `fdad2f20`; `C` = `bdf35c60`. Source paths mean `crates/rightkit-fs/src/`; documentation/tests are crate-relative.

| Earlier finding | Status & N/P/I |
|---|---|
| Refused-lock unlink | Closed, N fixed: refusal preserves pathname (`B:lock.rs:125`; `C:lock.rs:307`). |
| Directory ACL opens dropping locks | Closed, N fixed (`B:private.rs:138`; `C:acl.rs:230,255,294`). |
| `sync_dir` opening regular files | Closed, P fixed (`B:atomic.rs:226`; `C:atomic.rs:856`). |
| Existing-file ACL/SQLite regression | Closed: path-based inspection opens nothing (`B:private.rs:36,55`; `C:private.rs:53,81`). |
| 1, 5, 13: Windows replacement/recovery | Closed by removal (`C:CHANGELOG.md:32`; `C:atomic.rs:1073`). |
| 2: Alias registry | Improved, I; refusal exception remains (`B:lock.rs:292`; `C:lock.rs:585`). |
| 3: Retry barriers | Open, disclosed I (`B:state.rs:434`; `C:state.rs:839`; `C:README.md:38`). |
| 4: Atomic/settings parents | Filesystem residual open, disclosed I (`B:atomic.rs:88`; `C:atomic.rs:943`; `C:README.md:62`). |
| 6: Weak ACL fallback | Closed (`C:private.rs:259`). |
| 7: Windows creation window | Closed (`C:private.rs:552`). |
| 8: Copy staging | Closed (`C:clone.rs:488`). |
| 9: Reserved namespace | Bypass open, disclosed I (`B:atomic.rs:267`; `B/C:swap.rs:27`; `C:README.md:68`). |
| 10: Settings assertion | Not applicable; separate crate (`docs/audits/2026-10-12-astra-fs-comparative.md`, finding 10). |
| 11: Non-private ACL exposure | Open, disclosed I (`B:atomic.rs:116`; `C:atomic.rs:495`; `C:README.md:72`). |
| 12: Stale source permissions | Closed (`C:clone.rs:478`). |
| 14: Durability cache | Removed; residual I matches finding 3 (`C:CHANGELOG.md:33`). |
| 15: ACL scrubbing | Closed by refusal (`C:acl.rs:280`; `C:CHANGELOG.md:34`). |
| 16: Raw clone privacy | Open, disclosed P (`B:clone.rs:100`; `C:clone.rs:128`; `C:README.md:72`). |
| 17: Lifecycle coverage | Partial, I; ancestor tests exist, ACL interleavings remain untested (`C:tests/state_ancestors.rs:104`; `C:tests/acl_macos.rs:179`). |

**Blockers:** None established.

**Directory-open check.** Both ACL calls reach one helper that supplies `O_DIRECTORY | O_CLOEXEC` atomically under `cfg(unix)`; directory synchronization does likewise. State pinning, children & listings already use directory-only `openat` (`C:acl.rs:249`; `C:atomic.rs:843`; `C:state.rs:705,1022`).

For `create_private_new(file.join("child"))`, parent inspection accepts `ENOTDIR`, then exclusive creation reports its own failure; postcreation inspection propagates errors instead (`B:private.rs:138`; `C:acl.rs:235,267,294`; `C:private.rs:210`). This preserves exclusion without hiding creation failure.

No `O_NOFOLLOW` was added to ACL/sync opens, so symlinked directory parents remain followed; `StateDir` retains its separate symlink refusal. Unsupported ACL filesystems retain `ENOTSUP`/`EOPNOTSUPP` acceptance. Linux/other Unix ACL checks remain no-ops, while synchronization receives `O_DIRECTORY` (`C:acl.rs:94,227,255`; `C:atomic.rs:856`; `C:state.rs:711`). No source-level compilation defect identified in this change.

**Remaining existing-path opens.** Each row concerns exclusion when supplied inode is already record-locked by this process.

| Public route | Baseline → candidate; disposition |
|---|---|
| `StateDir::read/read_opt` | `B:state.rs:495` → `C:state.rs:926`; P, disclosed reads. |
| `FileKey::of_path`, `Fingerprint::of_path` | `B/C:identity.rs:45,140`; P, disclosed handle-based identity/content inspection. |
| `IdentityStore` ledger operations | `B/C:identity.rs:245`; P, actual read; normally separate lock (`:240`). |
| File digests | `B:digest.rs:39` → `C:digest.rs:78`; P, disclosed reads. |
| `current_owner`/`probe_owner` | `B:lock.rs:425` → `C:lock.rs:800`; P, sidecar read; aliasing can release locks. |
| Clone ladder source | Absent from B’s raw-only API (`B:clone.rs:81`); `C:clone.rs:380,465`; new reads explicitly disclosed. |
| Fcntl acquisition | `B:lock.rs:306` → `C:lock.rs:585,591`; aliases parked except refusal below. |
| Flock acquisition | Absent at `B:lock.rs:113`; `C:lock.rs:705`; mixing kinds violates documented one-kind usage (`C:README.md:80`; `C:CHANGELOG.md:104`). |
| Digest commit | B already closes staging (`B:atomic.rs:178`); C additionally reads it (`C:atomic.rs:638,641`); own staging, disclosed digest read. |

General read disclosure covers ledger/owner reads too (`C:CHANGELOG.md:6`). Directory enumeration remains through `read_dir`; exclusive-new opens cannot open existing destination inodes (`C:atomic.rs:1483,707`; `C:private.rs:229,193`; `C:identity.rs:419`; `C:state.rs:949`).

**Non-blocking notes.**

- **I, raced alias:** `FileLock::try_acquire(missing_alias)` can close held inode on ACL refusal before parking (`C:lock.rs:311,585`). B’s path-keyed registry already allowed alias acquisition/downgrade/release (`B:lock.rs:292,306`). Exclusion remains incomplete, explicitly disclosed (`C:CHANGELOG.md:6`); this fails neither blocker limb. Smallest improvement: park held-inode descriptors before refusal.
- **I durability:** retries through existing parents can acknowledge paths whose ancestor links remain unsynced; B synced none, C syncs newly created links only. Consumer paths: `StateDir::create` & durable `AtomicWriter`; invariant: crash-surviving pathname (table findings 3–4).
- **I/P preservation/privacy:** `swap_paths(record,temp)` followed by sweeping can lose records; non-private atomic writes/raw native clones under inheriting ACLs can expose bytes. Baseline shares these mechanisms; disclosures remain explicit (table findings 9, 11, 16).

**API & tests.** No silent API break found. Exhaustive `FsError` matching deliberately breaks; CHANGELOG requires next breaking version, although manifest remains `0.3.4` (`B:error.rs:7`; `C:error.rs:30`; `C:CHANGELOG.md:81`; `C:Cargo.toml:3`).

README’s new directory-lock promise lacks targeted regular-parent contention, substitution & `ENOTDIR` tests. Existing lock-preservation test covers only hardening/inspection. Competing-creator refusal & postcreation ACL mutation also lack deterministic coverage (`C:README.md:72`; `C:tests/private_keeps_locks.rs:48`; `C:tests/acl_macos.rs:110,179`).

**Not established:** candidate compilation/runtime, every Unix standard-library enumeration implementation, power-loss behavior & packaged-consumer equivalence. Supplied `3c917803` CI is context only.

Review completed read-only; no builds, tests or edits performed.
