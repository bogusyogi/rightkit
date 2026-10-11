1. **P1 — Linux cannot compile with `lock`.** [src/lock.rs:504](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lock.rs:504) passes `libc::F_WRLCK`/`F_RDLCK` to `set_record_lock(..., c_short)` at line 512. Locked libc 0.2.186 defines these as `c_int` on Linux, unlike macOS. Default Linux compilation therefore has mismatched types. **Fix:** cast constants to `libc::c_short` before calling.

2. **P1 — Path aliases break POSIX exclusion.** [src/lock.rs:464](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lock.rs:464), line 546: registry keys canonicalize only parents. Acquire exclusive through `A.lock`, then shared through hard-link `B.lock`—or case alias on case-insensitive APFS. Second acquisition succeeds & downgrades process-wide lock; dropping either descriptor releases both. **Fix:** coordinate by `(dev, ino)` & retain duplicate descriptors until final release; immediately closing rejected aliases also unlocks existing holders.

3. **P1 — Newly created state trees are not durable.** [src/state.rs:734](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:734), [src/state/windows.rs:433](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state/windows.rs:433): `create("new/deep")` publishes missing directories without parent barriers/write-through. Subsequent successful `write` flushes only inside `deep`; power loss can lose ancestor links & entire record. **Fix:** durably publish every newly created directory before descending, propagating failures.

4. **P1 — Windows `ReplaceFile` durability promise is invalid.** [src/atomic.rs:676](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:676) relies on `REPLACEFILE_WRITE_THROUGH`, an unsupported flag, while line 550 skips directory synchronization. Replacing existing state can return success without guaranteed durable publication; power loss may restore old namespace state. **Fix:** use supported durable publication semantics or reject this mode’s durability request. Flag assertions in `tests/atomic_testing.rs:85` cannot establish persistence.

5. **P1 — Windows private atomic writes have exposure windows.** [src/atomic.rs:389](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:389) creates temp before restricting ACL. Another principal allowed by inherited ACL can open it first & retain read access when secrets arrive. Line 567 also ignores failed destination hardening before `ReplaceFileW` preserves destination ACL. **Fix:** create temp with owner-only security descriptor, as StateDir does; publish with that ACL & fail closed on required hardening.

6. **P1 — Published content can be reported as unchanged.** [src/atomic.rs:317](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:317), lines 479 & 575: fallback hard-link publication may succeed before temp unlink fails; Windows replacement may succeed before ACL repair fails. Both become ordinary `FsError::Io`, so `target_replaced()` returns false & directory synchronization is skipped. **Fix:** track publication separately from cleanup/hardening, complete required synchronization & return typed post-publication errors.

7. **P2 — Sweeping can delete committed records.** [src/state.rs:463](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:463), [src/atomic.rs:1023](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:1023): public writes accept `invoice.rktmp-1-final`; sweeping with zero age deletes it as temporary data. **Fix:** reserve temporary namespace in public target validation & recognize complete generated temp names, preferably within separate staging storage.

8. **P2 — Concurrent Unix listings corrupt enumeration results.** [src/state.rs:889](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:889): cloned descriptors share directory offset. Thread A consumes first buffer; B rewinds & exhausts directory; A then sees EOF, omitting remaining entries. **Fix:** obtain independent open description via descriptor-relative `openat(".", ...)`, or serialize complete enumerations. Existing listing tests are sequential.

9. **P2 — Unlimited reads silently return empty data.** [src/state.rs:319](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:319): `read("record", u64::MAX)` overflows `max_bytes + 1`; debug panics, ordinary release wraps to zero & returns empty bytes. **Fix:** use `saturating_add(1)` & cover this boundary.

10. **P2 — Feature-specific tests are incorrectly gated.** [tests/lock_reparse.rs:3](/Volumes/D/claude/rightkit/crates/rightkit-fs/tests/lock_reparse.rs:3) imports locks even with only `fs`; [tests/state_durability.rs:4](/Volumes/D/claude/rightkit/crates/rightkit-fs/tests/state_durability.rs:4) imports filesystem APIs with digest-only configuration. `tests/lock_cross_process.rs:210` requires owner JSON even under lock-only configuration. **Fix:** add required-feature declarations & gate owner assertions on `json`.

README durability claims, assessed against current implementation:

| Claim | Status & evidence |
|---|---|
| Atomic file flush → publication → directory flush; typed sync failure | **PARTLY FIXED:** `atomic.rs:461–494` implements Unix sequence; findings 3–6 defeat broader guarantees. |
| StateDir `write`/`write_new` durability | **PARTLY FIXED:** `state.rs:370–397` flushes file & directory; newly created ancestors remain unprotected. |
| Durable removal & creating `child`; failures surfaced | **PARTLY FIXED:** Unix implements barriers at `state.rs:430` & `:767`; fault assertions exist at `tests/state_durability.rs:53–81`. Windows sets write-through flags but `windows.rs:604` performs no barrier; source/tests do not establish metadata persistence. |
| Apple fallback, strict refusal & weakest-level reporting | **FIXED:** `atomic.rs:861–910`, `state.rs:392`; assertions at `atomic.rs:944–981` & `tests/state_durability.rs:181–210`. |
| Crash-temp reclamation | **PARTLY FIXED:** implemented, but namespace collision causes finding 7. |

Reviewed main at `c9774ac592fece702e1a93cad9d46e0f8b41b367`; source only, no builds or tests executed.

**DO NOT PUBLISH — blockers: 1, 2, 3, 4, 5 & 6.**