Reviewed committed `25fe6c33` only; working-tree changes ignored. No builds or tests run. Source locations below refer to that commit.

**Fourth-read secrets findings**

| # | Status | Evidence |
|---|---|---|
| 1 | **CLOSED** for production configuration | Explicit-file writes refuse; fixture bypass now requires nondefault test feature: [macos.rs:310](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:310), [314](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:314). |
| 2 | **PARTLY** | Backend-defined identities fix reopened-store aliases, but resolving identity separately from mutation introduces #11 below: [store.rs:391](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/store.rs:391). |
| 3 | **CLOSED** | SID-qualified global mutex & owner DACL remain: [windows.rs:114](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/windows.rs:114), [151](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/windows.rs:151). |
| 4 | **PARTLY** | `Retain` is safe; retirement now moves aside & rechecks. In-place check/wipe race remains explicitly excluded by caller’s “no writers” precondition: [migrate.rs:168](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:168), [279](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:279). Nonblocking under that narrowed contract. |
| 5 | **CLOSED** | Worst-case UTF-8 capacity & zeroized scratch remain: [keyring_compat.rs:48](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/keyring_compat.rs:48). |
| 6 | **CLOSED** | Allocator-wide scanning replaced with initialized-byte drop hook: [apple_helper.rs:70](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:70). |
| 7 | **CLOSED** | Clock test requires successful observations; file-store test launches real children: [clock.rs:736](/Volumes/D/claude/rightkit/crates/rightkit-license/tests/clock.rs:736), [file_store.rs:294](/Volumes/D/claude/rightkit/crates/rightkit-license/tests/file_store.rs:294). |
| 8 | **CLOSED** | Durability checked before migration & again before conditional source removal: [migrate.rs:45](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:45), [100](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:100). |
| 9 | **CLOSED** | Environment import uses atomic `put(CreateOnly)` rather than presence-check/unconditional-write: [migrate.rs:454](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:454). |
| 10 | **CLOSED** for reported prefix alias | macOS identity now derives from service/account: [macos.rs:542](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:542). Backend identity remains vulnerable under #11. |

**Consumer checks & inherited blockers**

| Finding | Status | Evidence |
|---|---|---|
| Process 1 | **CLOSED** | Tree cleanup precedes writer join/final reap: [runner.rs:639](/Volumes/D/claude/rightkit/crates/rightkit-media/src/runner.rs:639), [spawn.rs:458](/Volumes/D/claude/rightkit/crates/rightkit-sandbox/src/spawn.rs:458). |
| Process 2 | **CLOSED** | Early Windows leader exit still terminates retained job: [owned.rs:609](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:609). |
| Process 5 | **CLOSED** | Media consumes bounded stderr primitive: [runner.rs:545](/Volumes/D/claude/rightkit/crates/rightkit-media/src/runner.rs:545). |
| HTTP 4 | **CLOSED** | OAuth scrubbing remains: [lib.rs:595](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:595), [device.rs:197](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/device.rs:197). |
| Process 6 | **OPEN-IN-PR** | Main still temporarily enables handle inheritance under private lock: [spawn_windows.rs:173](/Volumes/D/claude/rightkit/crates/rightkit-process/src/spawn_windows.rs:173), [384](/Volumes/D/claude/rightkit/crates/rightkit-process/src/spawn_windows.rs:384). Known `win/process-spawn-handles` work; not new. |
| HTTP 7 | **PARTLY** | Scoped bypass entries are silently discarded: [system_proxy.rs:121](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:121). See #14. |
| HTTP 8 | **CLOSED** | Different per-scheme endpoints return typed error: [client.rs:327](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:327). Both clients propagate it; deferred routing is safe. |
| Process 4 / 7 | **PARTLY — BLOCKING** | Identity comparison still precedes separate numeric-PID reap/signal: [process_unix.rs:488](/Volumes/D/claude/rightkit/crates/rightkit-process/src/process_unix.rs:488), [498](/Volumes/D/claude/rightkit/crates/rightkit-process/src/process_unix.rs:498), [unix_child.rs:385](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:385), [395](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:395). External reap/reuse can still redirect operations. Shared reaping authority or dedicated supervisor remains necessary. |

**Class-level verdict: incomplete.**

- Store adapters, imports & migrations use `Store::put`; credential retirement uses `remove_copied_source`. Crate-wide “every write” claim is false: public [AppleHelper::write:601](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:601) & [copy_synced_to_device_only:637](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:637) bypass that primitive.
- Process proof guard has direct bypasses: failed identity acquisition invokes unguarded kill/wait in [unix_child.rs:190](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:190) & [pty.rs:204](/Volumes/D/claude/rightkit/crates/rightkit-sandbox/src/pty.rs:204). PTY proof/action also lacks shared ownership exclusion: [pty.rs:461](/Volumes/D/claude/rightkit/crates/rightkit-sandbox/src/pty.rs:461).
- PDF replacement independently calls `ReplaceFileW`, bypassing filesystem partial-replacement handling: #12.
- Recovery-file protection remains conditional: failed recovery rename explicitly leaves sole new content sweepable at [atomic.rs:1478](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:1478). **FS-13 remains PARTLY/BLOCKING.** Protect recovery candidates before destructive replacement, including rename-failure & crash paths.

**NEW — BLOCKING**

**11. P1 — macOS lock identity can differ from mutated keychain.**  
[macos.rs:555](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:555) resolves default keychain for exclusion; [600](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:600) resolves it again for mutation. Pause A after locking default X; A times out; default changes to Y; B successfully writes Y; resume A, which resolves Y & overwrites B under X’s permit. Failed first resolution followed by successful second resolution similarly splits identities. [Apple’s API retrieves current domain default](https://developer.apple.com/documentation/security/seckeychaincopydomaindefault%28_%3A_%3A%29); separate calls do not bind one target. **Fix:** resolve once inside timed worker, derive permit from that retained keychain & perform mutation against exactly that object. Add default-switch & resolution-failure interleavings.

**12. P1 — Windows PDF save can delete both original & replacement.**  
[commit.rs:156](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:156) calls `ReplaceFileW` without backup; every error triggers replacement deletion at [93](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:93). Microsoft documents error **1176** without backup as original already absent, replacement still at original temporary name. Cleanup therefore deletes sole surviving content. This mechanism is wrong against [ReplaceFileW’s documented failure states](https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-replacefilew). **Fix:** use shared backup-aware replacement primitive; retain recovery content for partial failures. Cover every documented error state.

**13. P2 — Durable-directory record survives rename without persisting new link.**  
Cache keys contain directory device/inode/birth time at [atomic.rs:925](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:925); cache hits suppress parent synchronization at [937](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:937). Publish/cache `/A/d` & `/B`; rename `d` into `/B/d`; invoke durable creation there. Same directory identity skips synchronization of new `/B/d` entry, so acknowledged path can disappear after crash. Durability belongs to directory entry, not permanently to child inode; [fsync requires separate containing-directory synchronization](https://www.man7.org/linux/man-pages/man2/fsync.2.html). **Fix:** synchronize traversed parent entries; remove inode-only cache unless link identity & mutation invalidation are reliable.

**14. P2 — Unsupported Windows bypass rules route secrets instead of failing closed.**  
For one common proxy endpoint, [system_proxy.rs:144](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:144) accepts configuration after [121](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:121) drops `http://private.example` bypass. Requests intended to connect directly now send plaintext credentials/body through proxy. Dropping `<local>` has analogous exposure; [Microsoft defines it as intranet bypass](https://learn.microsoft.com/en-us/windows/win32/api/winhttp/ns-winhttp-winhttp_proxy_info). **Fix:** unsupported bypass semantics must return typed configuration error before networking. Per-scheme endpoint rejection already demonstrates suitable approach.

**NON-BLOCKING**

**15. P3 — Class regression checks cover only selected paths.**  
[tests/store.rs:1185](/Volumes/D/claude/rightkit/crates/rightkit-secrets/tests/store.rs:1185) scans only `migrate.rs` for spellings; it cannot establish crate-wide write coverage. **Fix:** enumerate helper/native/Store mutation paths explicitly; add case-table rows for default switching, resolution failure, retirement recovery failure & partial replacement.

Platform checks found no claimed Darwin ABI defect: [ACL iteration:86](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/acl.rs:86) correctly treats zero as success, matching [Apple libc](https://raw.githubusercontent.com/apple-oss-distributions/Libc/main/posix1e/acl_entry.c). Kqueue readiness is supported by [XNU](https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/kern/kern_event.c); exit observation does not make subsequent numeric-PID actions atomic. Page budgeting charges bounded decompression & parsing centrally at [inspect.rs:247](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:247), consistent with [lopdf’s bounded API](https://docs.rs/lopdf/0.44.0/lopdf/struct.Stream.html).

DO NOT PUBLISH: 11, 12, 13, 14, Process-4, Process-6, Process-7, FS-13
