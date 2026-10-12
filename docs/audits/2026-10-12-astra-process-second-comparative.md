VERDICT: DO NOT PUBLISH — callback-capture deadlock is fixed, but `finish_within` retains an undisclosed **P** unbounded teardown hang.

B = published `21099448`; C = `3c917803`. Requested `fdad2f20` baseline citations have identical crate source to B; only repository metadata differs. Paths below identify C unless marked B.

| Earlier finding | Status & N/P/I |
|---|---|
| Callback-capture destructor blocks bounded finish | **Closed, formerly N**: completion check precedes join; regression test added ([stderr_tail.rs:185](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:185), [:287](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:287)). |
| 2: Windows early leader exit | **Closed, P fixed**: retained job terminated before return ([owned.rs:558](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:558)). |
| 3: macOS descriptor bound | **Still open P**, disclosed; see notes. |
| 4: helper steals status | **Closed, formerly N**, helper removed; unsupported option refused ([owned.rs:270](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:270)). |
| 5: stderr retention/redaction | **Closed, I completed** through oversized-line discard & retention cap ([stderr_tail.rs:117](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:117), [:225](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:225)). |
| 6: Windows inherited handles | **Still open I**, disclosed; see notes. |
| 7: replacement-group signals; spawn provenance | **Still open I**, disclosed; see notes. |
| 9: premature readiness | **Closed, formerly N**: notification cannot authorize blocking reap ([exit_watch.rs:136](/Volumes/D/claude/rightkit/crates/rightkit-process/src/exit_watch.rs:136)). |
| 10: watch failure blocks termination | **Closed, formerly N**: polling fallback & released child lock ([exit_watch.rs:128](/Volumes/D/claude/rightkit/crates/rightkit-process/src/exit_watch.rs:128), [unix_child.rs:598](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:598)). |
| 11: helper zombie | **Closed, formerly N**, removal documented ([CHANGELOG.md:34](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:34)). |
| Media completion/stdin, OAuth, sandbox sweep, PTY serialization | **Not applicable**, consumer/separate-crate findings ([first review:7](/Volumes/D/claude/rightkit/docs/audits/2026-10-12-astra-process-comparative.md:7), [:18](/Volumes/D/claude/rightkit/docs/audits/2026-10-12-astra-process-comparative.md:18)). |

**Blocker — P: thread completion check does not bound teardown**

- **Baseline:** [B stderr_tail.rs:91](https://github.com/Orthic-Labs/rightkit/blob/fdad2f20/crates/rightkit-process/src/stderr_tail.rs#L91) joins whenever `is_finished()` returns true.
- **Candidate:** [stderr_tail.rs:185](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:185) does likewise. Rust marks completion by dropping its result-packet reference before native thread exit; `is_finished` checks that reference count ([std lifecycle.rs:80, 211](https://doc.rust-lang.org/src/std/thread/lifecycle.rs.html#80)). `join` explicitly waits for thread-local destructors ([std join_handle.rs:101](https://doc.rust-lang.org/src/std/thread/join_handle.rs.html#101)).
- **Reachable consumer:** call `StderrTail::capture` with finite input & `on_line` initializing thread-local state whose destructor acquires application mutex M. Caller holds M, waits until that destructor starts, then calls `finish_within(100ms)`. Body has ended, `is_finished()` is true, but join waits forever for destructor blocked on M. **Zero timeout also reaches this join.** Separately, ignoring `join()`’s returned panic payload can run its caller-supplied destructor synchronously at line 187.
- **Invariant:** finite `finish_within` must return despite arbitrary drain-thread teardown. This is **P**, not N. Candidate instead asserts unconditional boundedness ([CHANGELOG.md:5](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:5)); known limits omit this exception ([README.md:169](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:169)).
- **Smallest clearance:** disclose this inherited limitation & qualify unconditional claim. Smallest code repair: omit joining from bounded finish, retain handle for explicit `finish`, & add teardown regression coverage.

**Specific checks & non-blocking notes**

Before joining, never-ending reads/callbacks remain timeout-controlled; poisoned guards recover, & zero timeout skips polling sleeps ([stderr_tail.rs:168](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:168)). Full-call boundedness fails through blocker above.

Snapshot ordering is correct: retention finishes before `DoneGuard` signals ([stderr_tail.rs:130](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:130), [:34](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:34)). Returning after signal without joining preserves every retained line. Timeout before signal yields only capture-so-far, as documented at [:160](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:160).

- **P, descriptor isolation:** `inherit_handles_only(...).spawn()` racing higher descriptor creation retains identical pre-fork bound in [B process_unix.rs:20](https://github.com/Orthic-Labs/rightkit/blob/fdad2f20/crates/rightkit-process/src/process_unix.rs#L20) & [C:20](/Volumes/D/claude/rightkit/crates/rightkit-process/src/process_unix.rs:20). Allowlist isolation remains incomplete; disclosed at [README.md:189](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:189).
- **I, private-handle isolation:** baseline raises inheritance ([B spawn_windows.rs:173](https://github.com/Orthic-Labs/rightkit/blob/fdad2f20/crates/rightkit-process/src/spawn_windows.rs#L173)); candidate serializes internal spawns ([owned.rs:333](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:333)). Concurrent external `Command::spawn` still inherits raised handles; disclosed at [README.md:182](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:182).
- **I, ownership:** baseline signals remembered groups ([B owned.rs:511](https://github.com/Orthic-Labs/rightkit/blob/fdad2f20/crates/rightkit-process/src/owned.rs#L511)); candidate proves identity first ([unix_child.rs:353](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:353)). `terminate_tree()` racing foreign reap/PID reuse still risks replacement processes; disclosed at [README.md:174](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:174).

Other callback interference exists: `while_unreaped` holds child lock through closure execution/destruction, delaying clone `wait_timeout`; baseline already permitted arbitrary work under `child()` guard ([B owned.rs:399](https://github.com/Orthic-Labs/rightkit/blob/fdad2f20/crates/rightkit-process/src/owned.rs#L399), [C:480](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:480)). **P lock-bound limitation**, explicitly disclosed at [README.md:114](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:114). `StallWatchdog::stop` also joins callback-bearing threads, but promises no timeout ([stall.rs:151](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stall.rs:151)).

**Silent API breaks & untested promises**

No additional silent API break found. Unix `child()` type change, signaling changes & additions are deliberate/documented ([CHANGELOG.md:122](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:122), [:178](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:178), [:194](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:194)).

Tests cover retention/redaction, ordinary callback teardown, ownership retirement & Windows descendant cleanup ([stderr_tail.rs:287](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:287), [:343](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:343), [tree_identity.rs:104](/Volumes/D/claude/rightkit/crates/rightkit-process/tests/tree_identity.rs:104), [windows_process_tree.rs:164](/Volumes/D/claude/rightkit/crates/rightkit-process/tests/windows_process_tree.rs:164)). Missing: bounded-finish TLS/panic-payload, poison, zero-timeout & never-ending-reader tests; forced early-event/allocation-failure integration coverage remains absent—fallback tests exercise isolated loops ([exit_watch.rs:335](/Volumes/D/claude/rightkit/crates/rightkit-process/src/exit_watch.rs:335)).

**Not established:** runtime reproduction, registry-byte equivalence, consumer integration, or independent CI verification. Supplied CI remains context. Review remained read-only; no builds, tests, or edits.
