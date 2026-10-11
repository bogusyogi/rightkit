1. **P1 — Windows replacement failure can destroy both versions.** [atomic.rs:1058](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:1058) treats every `ReplaceFileW` failure as unchanged; [atomic.rs:512](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:512) deletes replacement. For error **1176**, without backup, Windows already deleted original: cleanup removes remaining new content. [Microsoft documents this state](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew). **Fix:** supply unique backup, preserve recovery files on partial failures & propagate typed partial-state errors through settings. Existing pre-call injection at `tests/atomic_testing.rs:130` cannot expose this failure.

2. **P1 — New inode-alias cache grants locks on unrelated files.** [lock.rs:534](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lock.rs:534): hold shared inode A; acquisition through X observes inode B, then X changes to hard-link A before opening. Code caches `B → A`. Subsequent shared acquisition through unchanged `B.lock` returns A’s guard without locking B; another process can exclusively lock B. **Fix:** remove cross-inode alias caching; retain raced descriptors until actual inode’s final holder releases.

3. **P1 — Creation retries bypass failed durability barriers.** [state.rs:785](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:785), [state.rs:832](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:832): create `new/deep`; flushing parent of `new` fails. Retry opens existing `new`, skips its barrier, creates `deep` & successfully writes. Power loss can still lose entire tree. Concurrent creators have equivalent interleaving. **Fix:** synchronize traversed parents before accepting create-or-open success, including existing components. `tests/state_durability.rs:78` explicitly expects unsafe retry behavior.

4. **P1 — Atomic/settings parent creation remains non-durable.** [atomic.rs:421](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:421), [settings/atomic.rs:59](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/atomic.rs:59): write `new/deep/settings.json` with missing parents. Only leaf directory gets synchronized; success can precede loss of ancestor links. **Fix:** use durable component-wise parent creation, including retry handling from finding 3.

5. **P1 — Private Windows replacement still has exposure race.** [atomic.rs:749](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:749): harden destination; concurrent writer replaces it with broadly readable file; `ReplaceFileW` inherits that file’s DACL; reader opens new secret before post-hardening. **Fix:** reject private `ReplaceFile` combination or publish using rename/move retaining private temp’s DACL.

6. **P1 — ACL fallback falsely reports owner-only success.** [private.rs:181](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/private.rs:181): native hardening fails, fallback succeeds against destination containing explicit Everyone-read ACE. `/inheritance:r` removes inherited ACEs; `/grant:r` replaces grants for specified SID, leaving Everyone access. [Command semantics](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/icacls). **Fix:** fail closed on native error or replace complete DACL & verify it before publication.

7. **P1 — Public private-file creation retains original exposure window.** [private.rs:138](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/private.rs:138): `create_private_new` creates with inherited Windows ACL, then hardens. Another principal can retain readable handle before subsequent secret writes. **Fix:** route Windows creation through existing `create_new_owner_only`.

8. **P1 — Unix copy fallback exposes private source bytes.** [clone.rs:405](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/clone.rs:405): copying `0600` source with umask `022` into traversable directory writes secrets into `0644` temp before applying source permissions. Reader can retain handle. **Fix:** create temp `0600` initially, then apply intended permissions before publication. Existing permission test checks only final mode.

9. **P2 — Reserved namespace still bypassable.** [private.rs:128](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/private.rs:128): create & close committed `.record.rktmp-123-1-0`; zero-age sweep deletes it. Clone/link paths also bypass reservation. **Fix:** validate reserved names across public creation/publication APIs.

10. **P2 — New settings assertion always fails on Windows move modes.** [writer.rs:135](/Volumes/D/claude/rightkit/crates/rightkit-settings/tests/writer.rs:135) applies `REPLACEFILE_WRITE_THROUGH` mask to every API. Its value `1` also means `MOVEFILE_REPLACE_EXISTING`; valid flags `9` fail assertion. **Fix:** filter assertions to `ReplaceApi::ReplaceFile`.

Previous-read dispositions, retaining original numbering:

| Previous | Status & current evidence |
|---|---|
| 1: Linux lock types | **FIXED** — [lock.rs:568](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lock.rs:568), explicit casts. |
| 2: Lock aliases | **PARTLY FIXED** — inode coordination added; finding 2 defeats exclusion. |
| 3: State ancestors | **PARTLY FIXED** — [state.rs:795](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:795), finding 3. |
| 4: ReplaceFile durability | **FIXED** — [atomic.rs:914](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:914), unsupported flag removed; weaker guarantee reported at `:974`. |
| 5: Windows privacy | **PARTLY FIXED** — [atomic.rs:595](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:595), creation secured; findings 5–6 remain. |
| 6: Publication errors | **PARTLY FIXED** — [atomic.rs:550](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:550), cleanup/ACL errors typed; finding 1 remains. |
| 7: Sweeping records | **PARTLY FIXED** — [state.rs:378](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:378), original writes protected; finding 9 remains. |
| 8: Concurrent listings | **FIXED** — [state.rs:957](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:957), independent open description; concurrent regression test added. |
| 9: Unlimited reads | **FIXED** — [state.rs:329](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:329), saturating addition & boundary tests. |
| 10: Feature gates | **FIXED** — [Cargo.toml:110](/Volumes/D/claude/rightkit/crates/rightkit-fs/Cargo.toml:110), required features; `lock_cross_process.rs:207` gates JSON assertion. |

Prior README claims: durability & reclamation remain **PARTLY FIXED**; Apple fallback/strict/weakest-level reporting remains **FIXED** (`atomic.rs:1231`, `state.rs:404`). Windows metadata persistence remains unestablished (`state/windows.rs:573`).

**DO NOT PUBLISH — blocking P1 findings: 1–8. Reviewed main `48f8fa01`; source/test inspection only.**