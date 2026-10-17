**Publish reduced candidates, preserving proven fixes while removing newly added failure paths.** Main improves several protections, but PDF replacement & process waiting introduce concrete regressions.

Compared `7db868c4` with version-entry snapshots found through `git log -G '^version = '` & inspected using `git show`. Registry archive provenance was unavailable, so historical evidence below identifies Git snapshots explicitly. No builds, tests or edits performed.

**Classification:** **P** = published defect persists; **N** = mechanism absent from published snapshot; **R** = existing operation becomes less safe. These overlap: a new guard can inadequately address an old defect without causing a regression.

**rightkit-fs — published 0.3.3, baseline `fdad2f20`**

| Fifth-read blockers | Classification & evidence |
|---|---|
| **1, 13: interrupted replacement/recovery sweeping** | **N/R for callers selecting `ReplaceFile`.** Published [`atomic.rs:179`](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-fs/src/atomic.rs#L179) used `fs::rename`; today replacement precedes recovery protection at [atomic.rs:1410](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:1410). Failed protection leaves new bytes sweepable. Default still selects `Rename` at [atomic.rs:306](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:306). |
| **3, 4, 14: directory durability/cache** | **N mechanism; P underlying durability gap.** Positive inode cache at [atomic.rs:925](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:925) cannot establish durability of subsequently moved directory links. Published [`atomic.rs:88`](https://github.com/bogusyogi/rightkit/blob/fdadad2f20/crates/rightkit-fs/src/atomic.rs#L88) already created ancestors without synchronizing their parents; its final directory sync ignored errors. New cache defeats stronger guarantees, rather than removing an old guarantee. |
| **9: reserved-name aliases** | **P.** Published writer accepted arbitrary destination names; [`atomic.rs:267`](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-fs/src/atomic.rs#L267) swept names containing its marker. Current [normalization:848](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:848) narrows exposure but misses leading-space aliases & bypassing publishers. |
| **11, 15: ACL exposure/scrub** | **P exposure, N incomplete repair.** Published [`atomic.rs:116`](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-fs/src/atomic.rs#L116) relied on mode bits without ACL inspection. New [scrub:202](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/acl.rs:202) cannot revoke already-open handles. Removing scrub alone would restore broader exposure. |
| **16: native clone ACL bypass** | **P.** Published [`clone.rs:100`](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-fs/src/clone.rs#L100) already called `clonefile` directly; current [clone.rs:115](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/clone.rs:115) retains that path. |

**Safety:** common atomic/private-write paths are improved; whole expanded API is mixed. Secret exposure is reduced, not eliminated. New Windows replacement mode adds recovery/data-loss risk; directory durability remains incomplete.

**Smallest path:** remove `ReplaceFile` machinery from this release; retain ordinary rename/private creation. Delete positive durability cache; synchronize required parent links each time. Replace creation-time ACL scrubbing with rejection & disposal of unsafe empty inodes. Disable native cloning where private creation cannot be established. Consolidate settings callback publication & namespace validation into fs.

**Publish when:** interruption preserves old or recoverable new content; sweeper cannot delete legitimate/recovery files; private bytes never enter exposed inodes; acknowledged directory durability survives moved-directory & retry cases.

**rightkit-http — published 0.2.5, baseline `14b7c74f`**

| Fifth-read blockers | Classification & evidence |
|---|---|
| **9: Unicode alias replaces active lock** | **N.** Published [`lib.rs:13`](https://github.com/bogusyogi/rightkit/blob/14b7c74f/crates/rightkit-http/src/lib.rs#L13) had no download module. Current [download.rs:805](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:805) uses ASCII folding for filesystem identity; publication can replace another download’s lock. |
| **10: contradictory `416` accepted** | **N.** Resumable publication did not exist in 0.2.5. Current [`416` branch:483](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:483) lacks validator comparison already present in `206`. |
| **11: scoped bypass becomes proxy disclosure** | **N; R within today’s implementation.** `ProxyMode::System` did not exist in published API. Current [system_proxy.rs:121](/Volumes/D/claude/rightkit/crates/rightkit-http/src/system_proxy.rs:121) silently discards bypass rules while still selecting proxy. That can expose plaintext credentials/body to proxy; Microsoft documents [scheme-scoped bypass semantics](https://learn.microsoft.com/en-us/deployedge/configure-microsoft-edge-proxy-support). |

**Safety:** shared request paths are safer through address-policy, cancellation & redaction work. Added download/system-proxy paths are unsafe. These blockers do **not** establish regression in ordinary default requests: [default proxy is `None`:395](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:395).

**Smallest path:** defer resume, or make `416` restart unconditionally; reject unsupported system-proxy configurations before sending. Remove bespoke locking/part-file publication by using qualified fs primitives with internal files isolated from destination namespace. Merely substituting `FileLock` while retaining aliasable `.lock` names does not solve #9.

**Publish when:** concurrent downloads cannot corrupt or replace each other’s coordination files; contradictory responses never publish stale bytes; unsupported proxy settings emit zero requests. If download cannot meet this promptly, exclude it from this candidate & publish request-path improvements.

**rightkit-pdf — published 0.2.0, baseline `ca41ba38`**

| Fifth-read blockers | Classification & evidence |
|---|---|
| **7: private PDF exposure** | **P.** Published [`commit.rs:37`](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/commit.rs#L37) created ordinary temporary files without security preservation. Current [commit.rs:118](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:118) still provides no Windows creation protection; Unix preservation copies mode bits, not ACLs. |
| **13: replacement failure deletes both versions** | **N/R, unambiguous.** Published [`commit.rs:40`](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/commit.rs#L40) used rename. New [commit.rs:156](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:156) calls `ReplaceFileW` without backup, then [error cleanup:92](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:92) deletes surviving replacement. [Microsoft’s 1176 contract](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew) explicitly permits original disappearance. |
| **14: token estimate misses allocation amplification** | **P exhaustion, N ineffective guard.** Published [`inspect.rs:101`](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/inspect.rs#L101) decoded without this bound. New [estimate:204](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:204) undercounts adjacent operands/operators; it does not introduce parsing amplification. |
| **15: inherited-resource cloning amplification** | **P.** Published [`merge.rs:60`](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/merge.rs#L60) recursively cloned inherited values onto pages. Current [merge.rs:76](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/merge.rs:76) adds shallow accounting before that same recursive clone. |

**Safety:** **less safe for Windows saving because of #13**, despite better conflict handling & editing checks. macOS saving improves ordinary mode preservation; private-document exposure remains. Parsing is better bounded in several dimensions, but known exhaustion paths survive.

**Smallest path:** delete PDF’s bespoke platform commit implementation; use qualified fs `AtomicWriter`, retaining PDF’s conflict locking, source validation & token invalidation. Until preservation is supported, refuse affected saves without touching originals. Remove false token-bound heuristic, retain other limits, & require conservative allocation bounds before decode. Share inherited resources indirectly instead of cloning them per page.

**Publish when:** save failures preserve originals; staging never broadens access; default inspect/merge reject known amplification inputs before excessive allocation. Shared-stream locator refusals & advertised editability mismatches can follow separately.

**rightkit-process — published 0.3.3, baseline `21099448`**

| Fifth-read blockers | Classification & evidence |
|---|---|
| **4: helper steals another child’s status** | **N.** Published [`process_unix.rs`](https://github.com/bogusyogi/rightkit/blob/21099448/crates/rightkit-process/src/process_unix.rs) contained inheritance handling, not today’s parent-death helper. Current [identity comparison:488](/Volumes/D/claude/rightkit/crates/rightkit-process/src/process_unix.rs:488) remains separate from reap. |
| **6: Windows inherited-handle escape** | **P.** Published [`spawn_windows.rs:173`](https://github.com/bogusyogi/rightkit/blob/21099448/crates/rightkit-process/src/spawn_windows.rs#L173) already changed caller-handle inheritance while ordinary spawn bypassed its lock. Current [owned.rs:368](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:368) retains that separation. |
| **7: termination reaches replacement group** | **P; narrower exposure today.** Published [`owned.rs:511`](https://github.com/bogusyogi/rightkit/blob/21099448/crates/rightkit-process/src/owned.rs#L511) signaled remembered numeric groups even after reaping. Current [unix_child.rs:385](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:385) adds proof, but proof & signal remain separate. |
| **9, 10: premature readiness/resource-failure hangs** | **N/R.** Published [`process_handle.rs:209`](https://github.com/bogusyogi/rightkit/blob/21099448/crates/rightkit-process/src/process_handle.rs#L209) bounded waits used `try_wait` polling; macOS unbounded wait released shared mutex. New [process_handle.rs:163](/Volumes/D/claude/rightkit/crates/rightkit-process/src/process_handle.rs:163) can enter blocking reap under that mutex. |

**Safety:** default Unix waiting is less reliable; newly possible deadline/cancellation hangs outweigh an unconditional upgrade recommendation. Stale-group data-loss risk is reduced but persists; Windows secret-handle exposure is pre-existing & unresolved.

**Smallest path:** remove exit-watch-driven blocking reap & its raw-PID fallback thread. Restore bounded nonblocking reap attempts, releasing mutex between attempts. Defer macOS parent-death helper if ownership cannot be established simply. Keep improved descendant cleanup, Windows job handling & identity rejection. Land verified Windows handle isolation or refuse affected handle-transfer operation.

**Publish when:** deadlines remain bounded under early notification & allocation failure; concurrent termination always progresses; supported consumers share exclusive reap ownership. No signaling after ownership is lost. Apps with foreign reapers need isolation or refusal—not another identity check.

**rightkit-secrets — published 0.1.3, baseline `21b58829`**

There is **no secrets fifth-read** at this SHA. Latest report is fourth-read; `e922ec99` subsequently changes its findings.

| Latest blockers | Classification & current disposition |
|---|---|
| **1: explicit-keychain fallback/fixture exemption** | **P fallback, N exemption.** Published [`macos.rs:192`](https://github.com/bogusyogi/rightkit/blob/21b58829/crates/rightkit-secrets/src/macos.rs#L192) already used fallback-capable native add. Main refuses ordinary explicit-file creation; exemption is now behind [test feature:314](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:314). |
| **2, 10: late writes bypass exclusion** | **P late-write hazard, N incomplete exclusion mechanism.** Published [`store.rs:255`](https://github.com/bogusyogi/rightkit/blob/21b58829/crates/rightkit-secrets/src/store.rs#L255) abandoned timed-out workers without exclusion. Main uses [backend-defined identity:393](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/store.rs:393), including [macOS service/account:542](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/macos.rs:542). Named alias defect addressed in source. |
| **4, 8: migration destroys persistent source** | **P.** Published [`migrate.rs:49`](https://github.com/bogusyogi/rightkit/blob/21b58829/crates/rightkit-secrets/src/migrate.rs#L49) deleted credentials after copying; plaintext wipe was pathname-based. Main [refuses nondurable destinations:45](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:45) & offers explicit retention. Opt-in [retirement:279](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:279) still requires quiescent writers. |
| **9: create-only import overwrites concurrent save** | **P.** Published [`migrate.rs:207`](https://github.com/bogusyogi/rightkit/blob/21b58829/crates/rightkit-secrets/src/migrate.rs#L207) separated presence check from write. Main routes through [typed `put`:454](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/migrate.rs:454). |
| **6: allocator probe UB** | **N, test-only; removed.** Current [apple_helper.rs:89](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:89) uses controlled buffer-drop hooks. This was qualification-code failure, not shipped-app secret exposure. |

**Safety:** main is materially safer for credential preservation & secret handling. Blanket rollback would restore unconditional migration deletion, late-write races & destructive replacement fallback.

**Smallest path:** retain these fixes; remove optional plaintext retirement machinery from this candidate & leave sources intact. Keep fixture feature absent from shipped dependency graphs.

**Publish when:** native tests demonstrate create-only concurrency, alias/reopened-store timeout exclusion, guarded-destination preservation & failed-update preservation; qualification confirms no production fixture feature. This crate is closest to publication.

**Recommendations**

1. **Freeze scope, then CI before another adversarial read.** One mechanism per change; exact candidate SHA must compile & pass focused native Windows/macOS tests before review. Apply CI formatting/lock patches through existing workflow.
2. **Make review comparative.** Every blocker must identify published behavior, changed behavior, reachable consumer path & required invariant. “Guard incomplete” must not automatically become “regression.”
3. **Prioritize irreversible harm:** PDF replacement loss; fs recovery/namespace deletion & private-file exposure; process hangs/handle leakage/unsafe signaling; HTTP corruption/proxy disclosure. Close each through repair **or removal/refusal**.
4. **Test properties across interleavings:** old/new content survives interruption; private bytes never become readable; create-only never overwrites; deadlines bound completion; abandoned ownership cannot target replacement processes. Require tests to prove affected branches executed.
5. **Qualify packaged bytes & actual consumers.** Test registry-shaped packages outside workspace, with shipped feature sets; run seven consuming apps against frozen candidates. Dependency declarations alone prove neither integration nor safety.
6. **Publish independently once each gate passes.** Record residual noncritical debt; do not demand hypothetical perfection. Publish through sanctioned CI, pin released versions in consumers & remove marked path overrides.

**Release smaller, evidenced improvements now; keep speculative platform mechanisms out until their contracts are demonstrated.**
