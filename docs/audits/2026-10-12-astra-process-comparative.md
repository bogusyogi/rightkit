VERDICT: DO NOT PUBLISH — candidate introduces an unbounded hang in `StderrTail::finish_within` when callback teardown blocks.

B = `21099448`; C = `0f0395ee`. Paths below are relative to `crates/rightkit-process`.

| Earlier finding | Status & classification |
|---|---|
| 1, 8: media completion / retained stdin | Not applicable to this crate-only review; consumer findings ([earlier review:5](/Volumes/D/claude/rightkit/docs/audits/2026-10-11-astra-process-fifth-read.md:5), [:13](/Volumes/D/claude/rightkit/docs/audits/2026-10-11-astra-process-fifth-read.md:13)). |
| 2: Windows early leader exit | Closed by fix: job termination precedes return ([C owned.rs:554](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:554)); regression test [windows_process_tree.rs:164](/Volumes/D/claude/rightkit/crates/rightkit-process/tests/windows_process_tree.rs:164). |
| 3: macOS descriptor bound | Reopened **P**, disclosed; details below. |
| 4: helper steals status | Closed by removal, formerly N: unsupported hosts refuse before spawn ([C owned.rs:270](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:270)). |
| 5: unbounded stderr | Crate primitive fixed: capped retention & oversized-line discard ([C stderr_tail.rs:117](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:117), [:215](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:215)); media integration outside scope. |
| HTTP 4: OAuth echoes | Not applicable; separate crate ([earlier review:10](/Volumes/D/claude/rightkit/docs/audits/2026-10-11-astra-process-fifth-read.md:10)). |
| 6: Windows inherited handles | Still open **I**, disclosed; details below. |
| 7: replacement-group termination | Still open **I**, disclosed; details below. |
| 9: premature exit readiness | Closed by removal of blocking reap: watch only pauses; subsequent look decides ([C exit_watch.rs:136](/Volumes/D/claude/rightkit/crates/rightkit-process/src/exit_watch.rs:136)). |
| 10: watch failure blocks termination | Closed by polling fallback & released lock ([C exit_watch.rs:128](/Volumes/D/claude/rightkit/crates/rightkit-process/src/exit_watch.rs:128), [unix_child.rs:598](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:598)). |
| 11: abandoned helper zombie | Closed by helper removal ([CHANGELOG.md:29](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:29)). |
| Additional class findings | Spawn provenance remains **I**, disclosed ([C unix_child.rs:181](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:181)); raw-PID fallback removed. Sandbox descendant sweep & PTY serialization are outside scope ([earlier review:17–20](/Volumes/D/claude/rightkit/docs/audits/2026-10-11-astra-process-fifth-read.md:17)). |

**Blocker — N: bounded stderr finish can deadlock.**

- **Baseline:** [B stderr_tail.rs:84](https://github.com/Orthic-Labs/rightkit/blob/21099448/crates/rightkit-process/src/stderr_tail.rs#L84) polls `JoinHandle::is_finished` until deadline, joining only after completion.
- **Candidate:** [C stderr_tail.rs:101](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:101) introduces local `DoneGuard`; its destructor sets completion at [:34](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:34), before remaining callback captures finish dropping. `finish_within` treats that flag as permission for unconditional `join` at [:175](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:175). This follows Rust’s [drop scopes](https://doc.rust-lang.org/reference/destructors.html#drop-scopes) & [join semantics](https://doc.rust-lang.org/std/thread/struct.JoinHandle.html#method.join).
- **Reachable consumer:** call `StderrTail::capture` with empty `Cursor` input & `on_line` capturing an object whose destructor locks an application mutex. Hold that mutex while calling `finish_within(100ms)`. EOF drops `DoneGuard`, then callback destruction waits for caller’s mutex; caller joins while holding it. Baseline times out, allowing caller to release mutex.
- **Invariant:** finite `finish_within` must return despite unfinished drain-thread teardown.
- **Smallest fix:** restore completion-checked joining, returning snapshot when thread remains unfinished; add this destructor interleaving regression test.

**Non-blocking notes.**

- **P — descriptor exposure:** `inherit_handles_only(...).spawn()` with concurrent descriptor creation above captured bound can leak that descriptor. [B process_unix.rs:20](https://github.com/Orthic-Labs/rightkit/blob/21099448/crates/rightkit-process/src/process_unix.rs#L20) & [C process_unix.rs:20](/Volumes/D/claude/rightkit/crates/rightkit-process/src/process_unix.rs:20) capture identical bounds; exclusion stops at [:78](/Volumes/D/claude/rightkit/crates/rightkit-process/src/process_unix.rs:78). Invariant: allowlist isolation. Disclosed at [README.md:189](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:189).
- **I — Windows exposure:** [B spawn_windows.rs:173](https://github.com/Orthic-Labs/rightkit/blob/21099448/crates/rightkit-process/src/spawn_windows.rs#L173) temporarily raises inheritance; [B owned.rs:278](https://github.com/Orthic-Labs/rightkit/blob/21099448/crates/rightkit-process/src/owned.rs#L278) bypasses serialization. Candidate locks ordinary spawns ([C owned.rs:333](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:333)), but concurrent external `Command::spawn` can inherit raised handles. Invariant: private-handle isolation. Disclosed at [README.md:182](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:182).
- **I — foreign reaping:** `terminate_tree()` alongside foreign reaping/PID reuse can still signal replacement work. [B owned.rs:511](https://github.com/Orthic-Labs/rightkit/blob/21099448/crates/rightkit-process/src/owned.rs#L511) signals remembered groups without identity proof; [C unix_child.rs:353](/Volumes/D/claude/rightkit/crates/rightkit-process/src/unix_child.rs:353) proves before signaling, leaving syscall gap. Invariant: signal only owned processes. Spawn provenance shares this limitation; disclosed at [README.md:174](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:174).

**Silent API breaks & test coverage.**

No additional silent signature break found. Unix `child()` changes from std `Child` ([B owned.rs:399](https://github.com/Orthic-Labs/rightkit/blob/21099448/crates/rightkit-process/src/owned.rs#L399)) to `UnixChild`; deliberate break & post-reap behavior change are documented at [CHANGELOG.md:173](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:173) & [:134](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:134). Additions are recorded at [:82](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:82) & [:189](/Volumes/D/claude/rightkit/crates/rightkit-process/CHANGELOG.md:189).

Tests cover retirement ([tree_identity.rs:104](/Volumes/D/claude/rightkit/crates/rightkit-process/tests/tree_identity.rs:104)), non-reaping observation ([exit_observation.rs:54](/Volumes/D/claude/rightkit/crates/rightkit-process/tests/exit_observation.rs:54)), concurrent termination ([identity_tests.rs:882](/Volumes/D/claude/rightkit/crates/rightkit-process/src/identity_tests.rs:882)), Windows locking ([spawn_windows.rs:917](/Volumes/D/claude/rightkit/crates/rightkit-process/src/spawn_windows.rs:917)) & stderr redaction/caps ([stderr_tail.rs:299](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:299)).

README’s failure-mode wait promise ([README.md:83](/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:83)) lacks forced early-notification/allocation-failure integration coverage: fallback tests exercise isolated watch loops ([exit_watch.rs:335](/Volumes/D/claude/rightkit/crates/rightkit-process/src/exit_watch.rs:335)). No crate test calls `finish_within`; stderr helpers use `finish` ([stderr_tail.rs:267](/Volumes/D/claude/rightkit/crates/rightkit-process/src/stderr_tail.rs:267)).

**Not established:** runtime reproduction of identified deadlock, registry-byte equivalence, or consumer integration. Supplied CI result accepted. Review stayed read-only; no builds, tests or modifications performed.
