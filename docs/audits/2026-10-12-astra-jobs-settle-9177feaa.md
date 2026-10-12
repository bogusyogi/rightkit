# Review of rightkit-jobs store/settle.rs at RightKit main 9177feaa

Requested by the RightKit lead, 2026-10-12. Read-only (files exported with `git show`), Codex Astra high. File references are to the crate's `src/` at that commit; `settle.rs` means `store/settle.rs`.

Found hook deadlocks & remaining settlement/journal races. References below use `settle.rs` for store/settle.rs, alongside store.rs, runner.rs & pool.rs.

**(a) Lock inventory & ordering.** F=`settle.flush`; P=`settle.pending`; U=`settle.unread`; D=`settle.delivering`; C=`conn`; G=`prog`; K=`cancels`. Entries retain per-job `FileLock` claims. No separate journal, prune or sink mutex appears.

Nested acquisitions:

- F→P: owner `settle.rs:504`→599/613/798; flush 828/830→839/798/864/878.
- F→U→P: forced flush 837→971→948→744.
- F→D: flush 851→1023; pin destruction→261.
- F→C: owner 524/543, flush 852→398 or `store.rs:1218`.
- F→K, then separately F→G: `settle.rs:423`→`store.rs:1037/1044`.
- C→G: `store.rs:2156/2185`, through `settle.rs:406` & `store.rs:1531/1602/1877/1929/2036/2090/2125/2222`. Owner/flush retain F too.
- C→D: submit/prune `store.rs:785/805` or 1063/1065→1086→`settle.rs:1034`.

Other acquisitions are standalone; retained job claims span runner/store operations. Pending guards end before database acquisition; delivery pins retain membership, not D’s mutex. No callback-free lock-order cycle found.

**Findings:**

1. **Blocker — hook re-entry creates C↔F deadlock.** Thread A: owner `settle.rs:504` holds F→398 waits C. Thread B: submit `store.rs:785` holds C→921 `on_create`→owner settle→`settle.rs:504` waits F. Additionally, owner/flush→`settle.rs:398/407`→`store.rs:1340` hook→owner settle self-deadlocks on F. Ordinary hook re-entry without another F holder instead reacquires its own C. `JobControl::settle` reaches identical machinery (`runner.rs:266–272`). Smallest fix: reject same-store hook re-entry before acquiring F; queue requested settlements until transaction completion.

2. **Major — runner bypasses F.** `settle.rs:504/517/524`: runner observes no pending entry; owner then defers B through 580; runner subsequently commits A through 524. Its `after_write` flush finds terminal A & removes B through 855. Owner’s accepted newer outcome disappears. Another ordering—runner database failure, owner commits, runner finally inserts at 582—leaves stale pending/journal state after terminal commit. Fix: serialize runner requests through F from adoption through commit/insertion, releasing before callbacks/`after_write`.

3. **Major — discard races snapshots & journal replacement.** `settle.rs:719–734`: discard removes A, releases P; owner defers B & writes its journal; discard then deletes B’s file while B remains marked durable. Separately, flush snapshots A at 839; discard succeeds; flush still commits discarded A at 852. Fix: hold F across discard’s removal & journal deletion, coordinated with runner serialization.

4. **Major — replay lacks cross-store coordination.** `store.rs:696`; `settle.rs:938/948/852/804`: second store reads journal A; first store replaces it with B; second commits cached A & deletes B’s journal. F/P belong to individual stores. Fix: coordinate settlement/replay across stores/processes with shared locking & journal revisions.

5. **Major — ignored unlink failures preserve stale outcomes.** `settle.rs:626–630/791`: replacing A with B fails journaling; deleting A also fails; crash replays A. Successful commits likewise forget cleanup when unlink fails (`804`). Fix: reject replacement unless stale journal is invalidated; retain retryable cleanup state after commit.

6. **Minor — lifecycle attachment races replacement.** Runner adopts A (`runner.rs:783`); owner replaces it with B (`settle.rs:616–617`); runner attaches A’s event at `runner.rs:788`/`settle.rs:705`. B later commits with A’s announcement. Fix: adopt & derive/attach event atomically under F/P.

**(b,d)** Normal sinks run after F/P/C release (`settle.rs:530/879`; runner callbacks follow store returns). No sink-lock defect found. **Major:** F+C span database waits & hooks (`398–420`), stalling unrelated submit/cancel; P spans durable journal I/O (`626/762`). Use bounded/nonblocking settlement database attempts & keep hooks bounded.

**(e)** Poisoned mutexes recover via `into_inner`; no poison-induced abort found. Hook panics propagate; settlement sink panics are caught. No duplicate terminal-row commit or additional shutdown race found under documented quiescence.