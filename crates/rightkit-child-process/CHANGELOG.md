# Changelog

## 0.4.3

Issue #271. Written without compiling or running Cargo here; CI builds and runs the tests.

- **New: `PowerAssertion::acquire(reason, PowerKind)`**, a keep-awake guard released on drop. `PowerKind::System`
  prevents idle system sleep; `SystemAndDisplay` also keeps the display on. Windows: `SetThreadExecutionState` on a
  holder thread that lives as long as the guard (the state is per thread). macOS: an IOKit
  `IOPMAssertionCreateWithName` assertion (`PreventUserIdleSystemSleep` or `PreventUserIdleDisplaySleep`), visible in
  `pmset -g assertions`. Other hosts return `ErrorKind::Unsupported`. New Windows feature `Win32_System_Power`.
- **New: `ProcessPriority::Efficient`** (the enum was already `#[non_exhaustive]`). Windows: below-normal priority class
  plus EcoQoS (`PROCESS_POWER_THROTTLING_EXECUTION_SPEED` via `SetProcessInformation`, applied while the child is still
  suspended, best effort). macOS: nice 10 plus `QOS_CLASS_UTILITY` set between fork and exec (advisory; whether the
  class survives exec is to be confirmed on a Mac). Other Unix: nice 10.
- **Item 4 of #271 (macOS parent-death tie) is already delivered** by 0.4.2: `kill_on_parent_exit(true)` arms a kqueue
  `NOTE_EXIT` watchdog on macOS. Nothing further was added for it.

## 0.4.2

Issue #171. Written without compiling or running Cargo here; CI builds and runs the tests.

- **New: `terminate_all_owned(TerminationOptions) -> TerminateAllReport` and `owned_count()`.** A process-wide list of
  owned children (weak references; every `OwnedCommand::spawn`, never detached ones) lets a shutdown path stop them
  all without an app-kept list. Unix: SIGTERM to every group, one shared grace, SIGKILL to every group, bounded reap
  (2 s total); every signal goes through `UnixChild::signal`, so a reaped or recycled pid is never signalled
  (reported `already_gone`). Windows: each retained job and process is terminated (no graceful stage; the job is now
  an `Arc` inside `OwnedChild` so the list can hold a `Weak`, with unchanged close-with-the-child behaviour). The
  `OwnedChild` values stay valid. Dropping an `OwnedChild` removes it from the list.
- **New: `OwnedChild::terminate_tree_deep(options) -> TreeTermination { status, descendants }`.** On-demand tree
  termination without terminate-on-drop, for `spawn_uncontained_detached` children (which `terminate_tree` only
  signals directly) and ordinary ones: descendants' identities are recorded while the leader is proven, the leader's
  whole group gets TERM, the grace, KILL (Unix; a detached child leads its own session), the leader is reaped, then
  the recorded descendants are swept by identity, leaves first (the only path for a detached child's descendants on
  Windows). `Drop` behaviour is unchanged.
- **New: `kill_on_parent_exit` works on macOS.** A watchdog process (double fork, `setsid`, `kqueue` with
  `EVFILT_PROC | NOTE_EXIT` on the supervisor and the child) sends SIGTERM to the child's group when the supervisor
  exits however it exits, and SIGKILL after 2 s; it exits when the child does. `spawn` returns after the watchdog is
  armed and kills the child and fails otherwise. Differences from Linux: whole-process trigger (not the spawning
  thread), group-wide with KILL escalation, one helper process per tied child, a short untied window between spawn and
  arming, and a final signal that names the pid after the owner died. The helper never waits on or reaps a process
  (the problem that removed the earlier helper). Other Unix hosts still return `Unsupported`.
- **Tests.** `tests/owned_tree_control.rs` (list, shared grace, detached untouched, drop leaves the list, deep
  termination of detached and ordinary trees, repeat call); `tests/parent_death.rs` now runs on macOS too, plus a
  SIGTERM-ignoring child (macOS) and 20 tied children that exit at once (arming race).
- **New: sweep by executable path and command line (item 4 of #171).** `ProcessQuery` (`exe(path)`, `arg(token)`,
  `arg_containing(needle)`; all set criteria must hold, at least one is required, empty values are `InvalidInput`),
  `find_processes(&query) -> Vec<MatchedProcess>` (no signal; newest first) and
  `sweep_matching(&query, &MatchSweepOptions { grace, dry_run }) -> MatchSweepReport { matched, dry_run, sweep }`, which
  stops the matches through the existing identity-checked `sweep`. `MatchedProcess` carries the `AdoptedIdentity`, the
  executable and, when the query had an argument criterion, the command line. Sources: Linux `/proc/<pid>/exe` and
  `cmdline`; macOS `proc_pidpath` and the `KERN_PROCARGS2` sysctl; Windows `QueryFullProcessImageNameW` and
  `NtQueryInformationProcess(ProcessCommandLineInformation)` (Windows 8.1+; the raw line is split with the C runtime's
  rules). Safety: only processes of the current user (uid, or token user SID); never this process or its ancestors;
  each candidate is described before and after its command line is read and kept only if the start time agrees; an
  unreadable executable or command line never matches; a pid that changed hands before the signal is refused
  by the sweep. Limits: a process that `exec`s another program between the scan and the signal keeps its identity; a
  program that rewrites its argument area is matched on the rewritten text; Linux `... (deleted)` executables do not
  match. Tests: `tests/sweep_matching.rs` (marked child found, dry run, exe AND marker, exact token, sweep stops only the
  marked child, no match, refused empty query, self never matched) and unit tests in `src/foreign/scan.rs` (argument
  matching, ancestor walk, Windows command-line splitting, `KERN_PROCARGS2` parsing).

## 0.4.1

- **New: `OwnedChild::wait_then_kill(grace)`.** Like `wait_or_kill` but without the opening SIGTERM on Unix: the leader gets the whole grace period to exit on its own (for example on stdin EOF), then the tree is killed. On Windows it is the same as `wait_or_kill`. `rightkit-agents` `Session::shutdown` uses it, so a CLI that exits cleanly on EOF is no longer raced by SIGTERM.

- **Caller-settable process priority (additive, issue #129).** `OwnedCommand::process_priority(ProcessPriority)` selects
  `ProcessPriority::Normal` (the default, which adds nothing to the spawn), `BelowNormal` or `Idle`; the enum is
  `#[non_exhaustive]` and re-exported from the crate root. On Windows, `BelowNormal` adds `BELOW_NORMAL_PRIORITY_CLASS` and
  `Idle` adds `IDLE_PRIORITY_CLASS` to the creation flags, composed with the suspension or breakaway flag and `CREATE_NO_WINDOW`,
  which are never dropped; the allowlisted spawn path gets the same class. On Unix, the child raises its nice value to at
  least 10 (`BelowNormal`) or 19 (`Idle`) between fork and exec with `setpriority`; a value already at or above the floor is
  left alone, so no privilege is needed. Applies to detached spawns too. Tests: flag composition (`process_windows.rs`,
  Windows only: default unchanged, class added without dropping console, suspension or breakaway flags, distinct classes),
  nice floors and the default and setter (`owned.rs`, both platforms). Written without compiling or running Cargo here;
  CI builds and runs the tests.

- **Setuid children spawn on macOS (fix, issue #105).** `OwnedCommand::spawn` no longer refuses, and kills, a child the
  kernel will not describe because it is setuid or setgid (`/bin/ps`, `login`, `sudo`): `proc_pidinfo` returns zero bytes
  with errno 0 for those, which was read as "the child cannot be identified". The spawn now checks that the pid exists
  (`kill(pid, 0)`; `EPERM` counts) and, if so, records the child as opaque instead of missing. Actions by pid on an opaque
  child rest on the unreaped-child rule (the kernel is asked whether the pid is still our child before every signal and
  wait, as before) plus a parent check if the process becomes readable. A pid that does not exist at spawn is still refused,
  and every other identity check is unchanged. Limit, unchanged by the OS: an unprivileged parent cannot signal a
  setuid-root child (`EPERM`), so `terminate_tree` and `kill` on such a child report that error; waiting for it works.
  Tests (`identity_tests.rs`): opaque check against the parent, missing still refuses, `/bin/ps` spawn and reap on macOS.
  Follow-up: the GitHub macOS runner answers `proc_pidinfo` for a setuid child with an error (not zero bytes), which still
  read as "unidentified". An error from the description now also yields an opaque child when the pid is proven to exist
  (`kill(pid, 0)`), and the opaque proof treats a refused read as still unreadable. A pid that does not exist is still refused.
- **Byte-exact bounded output capture (additive, issue #120).** `OutputCapture::capture(reader, max_bytes)` drains a
  child's stdout (or any stream) on a thread into `CapturedOutput { bytes, truncated, total_bytes }`: the first
  `max_bytes` bytes unmodified (trailing newline and non-UTF-8 bytes kept), the rest read and discarded so the child never
  blocks on a full pipe. `try_capture`, `snapshot`, `finish`, `finish_within(timeout)`, `is_finished`, `take_error`;
  `read_bounded(reader, max_bytes)` is the blocking form. `StderrTail` is unchanged. Tests: unit tests in `capture.rs`,
  real-child tests in `tests/output_capture.rs`.
- **tokio helpers behind the optional `tokio` feature (additive, issue #121).** `NoWindow::no_window()` (and the public
  `CREATE_NO_WINDOW` constant) hides the console on Windows for `std::process::Command` always and for
  `tokio::process::Command` with the feature; a no-op elsewhere. With `--features tokio`: `tokio_support::terminate_tree(&mut
  tokio::process::Child, TerminationOptions)` is an async staged kill (Unix: `SIGTERM` to the group, grace, `SIGKILL`; the
  group is signalled only when the child leads it, otherwise descendants are swept by identity; Windows: descendants then
  the child, no graceful stage) and `tokio_support::terminate_process_group(pgid, options)` (Unix) does the same for a
  group id with no `Child`. Not built in: a session sweep by `getsid`. The feature adds `tokio` (`process`, `time`) as an
  optional dependency only. Tests: `tests/tokio_support.rs` (run with `--features tokio`), `no_window.rs` unit tests.
- **Priority tests and docs (issue #129 follow-up).** One real-spawn test per platform (Windows `GetPriorityClass`, Unix
  `ps -o ni=` at or above the floor); `ProcessPriority::Normal` is documented as "inherit the parent's class or nice
  value", not a reset to normal.

## 0.4.0

Renamed from rightkit-process; 0.3.3 remains published under the old name.

- **M3 environment policy (additive).** `OwnedCommand::env_allowlist_with_defaults(extra)` clears the inherited environment and keeps `DEFAULT_ENV_ALLOWLIST` plus the consumer's own names (a model-cache location, for instance); `OwnedCommand::env_is_cleared()` reports whether an allowlist cleared it (on every platform; the field was Windows-only). Gotcha now documented: `env_allowlist` starts from an empty environment, so call it once. Tests: `tests/env_policy.rs`. Not compiled when written (CI-only).
- **M3 restart policy: consecutive-failure cap and reset-on-ready (additive).** `RestartPolicy::with_consecutive_failures(cap, cooldown)`
  limits restarts granted in a row without a ready: at the cap the next restart waits `cooldown` instead of the exponential backoff and a
  new count starts; a zero cooldown returns `Exhausted`. The rolling-window budget is still checked first. `RestartTracker::ready(generation)`
  clears the consecutive count (a stale generation changes nothing); `RestartPolicy::with_reset_budget_on_ready()` makes it also clear the
  rolling budget (HeardRight's reset-on-ready). Defaults are unchanged, so existing policies behave as before. New `ConsecutiveFailures`,
  `RestartTracker::consecutive_failures`. Tests: `tests/restart_health.rs` (cooldown, zero-cooldown end, ready reset, stale-generation
  ready, zero cap refused). Not compiled when written (CI-only).

Additive; no version bump. Owner-gap lane M5 (items M5-O1, M5-O4, M5-O5, M5-O7). Written without running Cargo
(CI-only); the new code and tests were read for macOS and Windows cfgs but have not been compiled or run.

- **`reveal` module (M5-O1): reveal or open a file or folder through `OwnedCommand`, inside caller-supplied roots.**
  `reveal::reveal` / `reveal::open` (and `launch`, `launch_command`, the pure `plan`) start `open -R` (macOS),
  `explorer.exe /select,"<path>"` (Windows) or `xdg-open` on the parent folder (other Unix). The path is
  canonicalised and must lie inside a `PathScope` root (symlink, `..` and prefix-sibling escapes are refused with
  `PermissionDenied`; an empty scope is refused at construction). `open` also refuses files the system would run
  (executable bit, `.app`, `.bat`, `.lnk`, `.sh`, and similar). The helper is detached, with a null stdio and an
  allowlisted environment. Tests: scope and escape refusals (`tests/reveal_scope.rs`, unit tests).
- **`AdoptedIdentity` (M5-O4): identity for a pid this crate did not spawn.** Identity is pid plus start time
  (`StartToken`); the executable path is recorded for the caller but not compared, because a process that `exec`s keeps
  its pid and start time (commit b698ff37). `verify` and `signal(AdoptedSignal)` refuse a pid that now names another
  process (`SignalResult::IdentityMismatch`, nothing sent), report `Gone`, and never signal this process.
  `from_recorded` rebuilds a persisted identity; `AdoptedProcess::identity` bridges from the existing type, which still
  has no terminate method. Windows checks and terminates through one handle. Tests: identity-mismatch refusal by start
  time; a changed executable with the same start time is still the same process (`tests/adopted_identity.rs`).
- **Descendant enumeration and sweep (M5-O5).** `AdoptedIdentity::descendants`, `OwnedChild::descendant_identities`,
  `sweep(&[AdoptedIdentity], grace)` and `sweep_descendants(root, grace)` (graceful signal, bounded wait, forced
  signal; a target whose identity changed is `refused`). Descendants must be listed while their parent is alive.
- **Bounded termination on `Drop` (M5-O7).** Dropping an owned `OwnedChild` used to run the unbounded
  `terminate_tree`. It now kills at once and waits for the reap at most `drop_reap_timeout` (default
  `DEFAULT_DROP_REAP_TIMEOUT`, 2 s; `set_drop_reap_timeout`, `Duration::ZERO` for no wait); an unreaped leader is
  reaped by a background thread on Unix. Behaviour change only for a leader that does not die within the bound.
  Tests: `tests/drop_bounded.rs`.
- Not done, by decision: M5-O2 (Swift owner), M5-O6 (OS-tool wrappers), M5-O8 (release-time executable pinning); M5-O3
  (macOS parent-death) is a design note only, see the lane evidence file.

Published version number: this release changes or removes public items, so it is a breaking release under Cargo's rules for 0.x versions. It was developed as 0.3.4, which was never published; everything under the 0.3.4 heading below ships in 0.4.0. `PidProof` and the Windows tolerant-detach API are gone, `OwnedChild::child()` returns `UnixChild` on Unix, and `kill_on_parent_exit` is refused at spawn off Linux.

- **`StderrTail::finish_within` never joins the drain thread (two comparative reviews, 2026-10-12).** It waits, up to
  the timeout, for the thread's body to signal its end and returns what was captured; it does not join. Two ways a join
  could outlast the timeout were found: the first candidate joined as soon as the body signalled, while the caller's
  `map` and `on_line` are dropped after that signal; and 0.3.3 (and the first fix) joined once `JoinHandle::is_finished`
  was true, which can be true while thread-local destructors registered by those callbacks still run. Either way a
  destructor waiting on a lock the caller held made a bounded call wait for ever. The tail is complete without a join:
  every line is retained before the signal. `finish` (unbounded) still joins. Test:
  `finish_within_returns_while_the_callback_is_still_being_dropped`; the thread-local case has no test.

**Reduced candidate: mechanisms added after the published 0.3.3 were removed where a read found them
unsafe or their contract is not demonstrated by a test in this crate.** Five reads of this crate did
not converge because each fix added a platform mechanism that had not run, and the next defect was
inside it. What was removed is listed here with the reason; each item is restored to what 0.3.3 did.
None of the removed items was ever published, so removing them is not a break.

Removed:
- **Exit-event-driven reaping** (the `ExitWatch` that decided when to reap, its raw-pid
  `waitid(WNOWAIT)` fallback thread, `ExitWatch::wait_all`, and the blocking reap that followed an exit
  event). Reasons: a registration that answered `ESRCH` was read as "already exited" although XNU hides a
  process from registration before it is waitable, so a bounded wait entered a blocking reap holding the
  shared lock and outlived its deadline; and a failed pipe or thread allocation made an independent wait
  block `kill` and `terminate_tree` behind that lock. Replaced by the 0.3.3 contract: every wait on a shared
  child is a loop of non-blocking `try_wait` looks (the non-reaping `waitid(WNOHANG | WNOWAIT)` for
  `wait_exited*`), each under the lock and released at once (the exceptions are listed below), sleeping 5 ms
  between looks as 0.3.3 did. The kqueue `NOTE_EXIT` /
  pidfd descriptor remains **only as a wake-up hint**: a sleep on it ends early at the exit, never lasts
  past the caller's deadline or 1 s, and decides nothing; the look after it does. When it cannot be opened
  the wait polls at 5 ms. An unbounded `ProcessHandle::wait` releases the lock while it sleeps. A child
  still holds one extra descriptor for the hint. Waiting threads wake about once a second with the hint and
  about 200 times a second without it, for as long as the child lives.
- **Linux pidfd signalling and pidfd proof** (`pidfd_send_signal`, `Origin::Unavailable` on pidfd hosts).
  Linux now uses the same `/proc/<pid>/stat` identity proof as macOS, so there is one proof to reason
  about.
- **The macOS parent-death helper** (a forked helper process, its pipes, its kqueue, its descriptor sweep,
  its release protocol and its identity-checked reap) and `UnixChild`'s helper field. A reap of the helper
  could take another child's exit status once its pid was collected elsewhere and reused, and the
  ownership that would make it safe was not established simply. `OwnedCommand::kill_on_parent_exit` stays
  for Linux (`PR_SET_PDEATHSIG`, SIGTERM to the direct child, keyed to the spawning thread); on macOS and
  other Unix hosts it now fails the spawn with `ErrorKind::Unsupported` before anything is started.
  Nothing in this repository calls it.
- **`PidProof`** (public, Unix) and its use for a PTY child: it had no test in this crate, and its
  `wait_exit` reported a hint that its only caller took as "reap returns at once", the blocking-reap
  pattern above. `rightkit-sandbox`'s `PtySession` returns to polling `try_wait` as in 0.3.3 (an edit
  in that crate).
- **The macOS post-fork descriptor listing** in `inherit_handles_only` (`PROC_PIDLISTFDS` with `mmap` in
  the forked child). `Inheritance` is exactly the 0.3.3 code again, with its bound taken before the fork.
  See "Known limits" in the README.
- **Windows** `OwnedCommand::spawn_detached_tolerating_denied_breakaway` and `DetachedBreakaway`: the
  denied-breakaway branch that defines the feature has no test. `spawn_uncontained_detached` is unchanged
  and still fails closed.
- Tests of the removed mechanisms: `tests/atfork_handlers.rs`, `tests/spawn_ready.rs`,
  `tests/high_descriptor.rs`, the macOS cases of `tests/parent_death.rs`, the helper and descriptor-sweep
  tests in `src/process_unix.rs`, the exit-watch wake-up-count tests.

Changed (Unix) beyond the restorations:
- The macOS refusal of a group signal (`EPERM` while the last members are mid-exit) is retried for at
  most 300 ms **outside** the child lock (`OwnedChild` releases it between attempts), not inside it.
- The tree-wide grace of `terminate_tree*` lists the group every 5 ms (macOS `proc_listpids`, Linux
  `/proc`) until nothing but the exited, unreaped leader is left. It no longer opens a descriptor per
  group member.

Where the shared child lock is still held across a wait or a long action, stated precisely (README
"Where the shared lock is held", `unix_child` module docs) and not changed, because none is a bounded wait
that enters a blocking reap and only the first can hold a deadline-bearing caller back:
- `wait` through the `OwnedChild::child()` guard. The caller holds the lock by holding the guard, so a
  bounded wait on a clone, a `kill`, `terminate_tree` or `Drop` on another thread queues behind it. The
  caller chose that, and 0.3.3's `std::process::Child::wait` through the same accessor did the same. The
  wait itself reaps only with non-blocking looks. Use `OwnedChild::wait`, `wait_timeout` or a
  `ProcessHandle` where anything else must stay live.
- Failed-spawn cleanup with no timeout and the `rightkit-partial-reap` thread. Both wait under the lock, as
  in 0.3.3, on a child that only the spawning thread (then the reaper) can reach: the partial `OwnedChild`
  was never returned and no `ProcessHandle` was cloned from it.
- `OwnedChild::while_unreaped`, which holds the lock for as long as the caller's closure runs. New since
  0.3.3; `rightkit-sandbox` passes a descendant sweep with a 200 ms grace.
The `rightkit-detached-reap` thread (a detached child's eventual reap) makes non-blocking looks and releases
the lock between them. Waiting threads wake about once a second with the hint and about 200 times a second
without it for as long as the child lives.

Known limits, stated in the README and in `identity`, not closed here:
- Unix: the identity proof and the call it guards are two steps; foreign code that reaps our child in
  that gap, and a replacement that takes the pid there, defeats it for one call. The identity is read
  just after spawn, not by it.
- Windows: handles raised for inheritance by `inherit_handles_only` can reach a process created by code
  outside this crate (a plain `Command::spawn`). Not addressed and not verified on Windows.
- macOS: `inherit_handles_only` closes descriptors up to a bound taken before the fork (0.3.3).

Kept from earlier unreleased work, each covered by a test in this crate:
- Fixed (Windows, fourth read finding 6, partial): an ordinary spawn could run while an allowlisted spawn
  (`inherit_handles_only`) had the caller's handles raised for inheritance. Both spawn paths of this crate
  now take one lock that covers raising the flags, creating the process and restoring them. Added
  `with_windows_spawn_lock` so code that creates processes by other means can join the same
  serialisation (the lock is not re-entrant); a spawn made outside it is still exposed. A regression test
  stops an allowlisted spawn while its handle is inheritable and shows an ordinary spawn creates nothing
  until the handle is private again.
- Fixed (Windows): `wait_or_kill` terminates the retained job when the leader exits within `grace`, so
  descendants that outlived the leader no longer survive; the result is still `Exited` with the leader's
  status.
- Fixed (Unix), fourth read finding 7: a pid is no longer trusted because it is "one of my unreaped
  children". Foreign code can reap the leader and a replacement child can take its number. A new
  `identity` module reads parent and start time (macOS `proc_pidinfo`, Linux `/proc/<pid>/stat`) at
  spawn; `UnixChild::prove` compares it before every site that names the pid: group and leader
  signals, the `waitid` exit probe (proved again after a positive probe), the macOS `kill(-pgid, 0)`
  membership probe, `try_wait` and `wait`. A changed or missing description marks the leader lost: no
  signal, no wait, no reap, and the replacement keeps its status. A child the host cannot describe at
  spawn is killed and the spawn fails (`require_identity`). `UnixChild::wait` polls the proof and a
  non-blocking reap instead of one blocking `waitpid(pid)`, which would have waited on a replacement.
  Residual: foreign reap plus pid reuse inside the gap between proof and call (see above).
- Added `OwnedChild::while_unreaped(act)`: runs a pid-based action under the proof and the child lock,
  and skips it once the leader is reaped. `rightkit-sandbox`'s `SandboxedChild::terminate_tree` uses it,
  so its descendant sweep no longer signals a recycled number after the leader was reaped.
- `StderrTail::try_capture` returns `io::Result<StderrTail>` and surfaces a drain-thread spawn failure.
  `capture` keeps its signature and is documented as best effort: on a spawn failure it returns a tail
  that retains nothing.
- `process_usage`, `observe_idle` and their types (see 0.3.4 "Added"): unchanged, used by the runtime
  budget benches of other crates.

## 0.3.4

Safety release. Read "Fixed" first: 0.3.3 can signal an unrelated process group.

Fixed (Unix):
- **The 0.3.3 hazard.** After the leader of an `OwnedChild` was reaped, `terminate_tree`, `wait_or_kill`,
  the termination options and `Drop` could still send `SIGKILL`/`SIGTERM` to `-pgid`. Once the leader is reaped
  and its group is empty, the kernel may give that id to an unrelated group, so the signal could reach a program
  this process never started. An unreleased intermediate of this version checked group membership after the reap;
  that check and the signal were not atomic, other clones or the public `child()` accessor could reap in
  between, and the "sealed" state was kept in two atomics published in the wrong order.
- **The rule that replaces it.** A group signal is sent only while this process still holds the leader as its
  own unreaped child, because an unreaped (running or zombie) child pins its pid and group id and only this
  process can reap it. Every group signal is sent under the lock that guards reaping, after the leader is
  confirmed unreaped. After the first reap by any path (wait, try_wait, wait_timeout, a `ProcessHandle` or
  clone, the detached reaper, `child().wait()`/`try_wait()`), group signalling is retired for the `OwnedChild` and
  all its clones: no signal, no `kill(-pgid, 0)` probe. The state is one lock-guarded enum, not two atomics.
  Detached children follow the same rule for their direct pid.
- The terminating paths (`terminate_tree`, `terminate_tree_with_options`, `terminate_tree_bounded[_with_options]`,
  `wait_or_kill`, `Drop`) observe an exited leader with `waitid(WNOWAIT)`, signal the group while the zombie pins
  the id, and only then reap. During a tree-wide grace they stop waiting as soon as nothing but the exited,
  unreaped leader is left.
- **Behaviour change you will notice.** `wait()` (or any plain reap) followed by drop signals nothing:
  grandchildren that outlive a plainly waited leader are no longer killed. Use `terminate_tree`,
  `terminate_tree_with_options`, `terminate_tree_bounded` or `wait_or_kill` before the leader is reaped. A
  `ProcessHandle` clone that waits on another thread reaps as soon as the leader exits and retires signalling for
  the owner as well (a monitor that must not do that uses `wait_exited`). `wait_or_kill` whose leader exits inside the grace now kills descendants that are still
  running (before it reaps the leader) instead of leaving them for `Drop`.
- Windows: `OwnedCommand::from_command` again has no default spawn configuration, as in 0.3.3. An allowlisted
  spawn (`inherit_handles_only`) from a wrapped `Command` requires `windows_spawn_config`, so a wrapped command
  with `env_clear()` or piped stdio can never silently inherit the parent's environment or stdout. Only
  `OwnedCommand::new` supplies the default. (An unreleased intermediate had widened this.)
- `process_usage` on Unix targets other than Linux/Android/macOS returns `ErrorKind::Unsupported` (chosen at
  compile time) instead of reading a procfs that may not exist; Linux counters use checked arithmetic.

Fixed (Unix), second independent read of 0.3.4 (`docs/audits/2026-10-11-astra-rightkit-process-second-read.md`):
- **An exit monitor no longer defeats tree termination.** `ProcessHandle::wait` (and `try_wait`,
  `wait_timeout`) reap the leader, which retires group signalling, so a monitor thread that learned of the exit
  that way left a TERM-resistant grandchild that held the leader's pipes alive: the owner never escalated to
  KILL and a reader join blocked until the grandchild ended on its own. The new `ProcessHandle::wait_exited`
  and `wait_exited_timeout` observe the exit **without reaping**; the exited leader stays a zombie that pins the
  group id, the owner kills the group (`wait_or_kill`, `terminate_tree*`), and only then reaps. On hosts with
  no non-reaping probe (Unix other than Linux, Android, macOS and iOS) they reap exactly like `wait` and
  `wait_timeout`, so the gap stays open there. `rightkit-media`'s runners use them and keep the deadline and
  cancel live while the pipes drain (a pipe that stays held past the stop is abandoned after 2 s, not joined
  forever); `rightkit-sandbox::wait_with_output` reaps only after the drain.
- **Waits cannot follow a recycled pid.** `ProcessHandle::wait` (and `wait_exited`) used to read the shared
  state, release the lock and then block in `waitid(pid)`; a pause between the two let another reader reap, the
  kernel recycle the number, and the waiter block on an unrelated child. Waiters now look under the lock,
  release it, sleep and look again, so no wait system call names the pid outside the lock. (An unreleased
  intermediate did this with an exit-event-driven reap; that was removed, see "Unreleased": the waits are
  non-blocking looks, 5 ms apart where no wake-up hint is available, as in 0.3.3.)
- **`StderrTail` redaction and byte cap.** A line longer than four times `max_bytes` was cut and each piece
  handed to the line-based `map` as if it were a line, so a secret spanning the cut passed through unredacted,
  and a single retained line could exceed `max_bytes`. Such a line now never reaches `map`: it is read to its
  newline and discarded, and one marker line, `[stderr line over N bytes dropped]`, is forwarded and retained
  in its place. A retained line longer than the budget is cut to its last bytes (at a character boundary), so
  `snapshot()` is never longer than `max_bytes`; `max_bytes == 0` retains nothing.
- Test only: the sampling test no longer runs on Unix targets where sampling returns `Unsupported`, and a
  test asserts that error there.

Breaking (Unix, public items):
- `OwnedChild::child()` returns `MutexGuard<UnixChild>` instead of `MutexGuard<std::process::Child>`. The new
  public type `UnixChild` has the stdio fields `stdin`, `stdout`, `stderr` and the methods `id`, `try_wait`, `wait`
  and `kill`, mirroring `WindowsChild`. Code that named `std::process::Child`, or used any other `Child` member
  through the accessor, no longer compiles. The std `Child` is no longer reachable, because a reap through it
  could not be seen by the crate. (`PlatformChild`, the alias behind the accessor, follows.)
- Strictly this is a minor-version break (0.4.0); the version stays 0.3.4 only because every sibling crate in the
  repository pins `rightkit-process` at 0.3.x and that bump is a coordinated release step outside this crate.

Fixed (found by CutRight): with zero kill grace, a deadline kill that raced the child's own exit could fail.
A group signal answered `EPERM` (macOS, only zombies left) was an error unless the leader was still visibly
exited at the moment of the check; it is now "already gone" whenever the leader is no longer running, including
when a handle clone reaped it in between. `rightkit-media`'s runner additionally reports `TimedOut` or `Cancelled`
whenever the deadline or cancel caused the stop, whatever the kill returned. Repeated-run tests (250 and 200
rounds) cover both crates.

Added:
- `ProcessHandle::wait_exited()` and `ProcessHandle::wait_exited_timeout(Duration) -> io::Result<bool>`: block until
  the leader has exited without reaping it (see "Fixed", second read). Windows: they wait on the process
  handle; nothing is reaped there. Additive.
- `OwnedCommand::kill_on_parent_exit(bool)`, off by default; Linux only (`PR_SET_PDEATHSIG(SIGTERM)` on the
  direct child in `pre_exec`, then `getppid()` is checked so the spawn fails if the supervisor already died;
  direct child only, one catchable `SIGTERM`, keyed to the forking thread, no group kill and no `SIGKILL`
  escalation). Other Unix hosts, macOS included, fail the spawn with `ErrorKind::Unsupported` before anything
  starts rather than run untied (an unreleased macOS helper was removed, see "Unreleased"). Windows ignores
  it because kill-on-close job objects already cover parent death. Combining the option with
  `spawn_uncontained_detached` returns `InvalidInput`.
- `process_usage`, `observe_idle`, `ProcessUsage`, `UsageRates`, `IdleObservation` and `BudgetLabels`: resource
  usage of the current process (macOS, Windows, Linux/Android).
- Tests: `tests/tree_identity.rs`, `tests/parent_death.rs` (Linux), and unit tests that record every
  signal and reap in order, including forced-id-reuse and clone-interleaving cases.
- Crate metadata now includes `CHANGELOG.md`.

## 0.3.3 — 2026-10-09

Changed:
- Unix builds call `libc` directly in place of `nix`, and the `nix` dependency is removed (`21099448`). Public API is unchanged.

Fixed:
- `process_is_running` retries on `EINTR`. Before, the signal-0 probe returned `EINTR` as an error (`21099448`).

## 0.3.2 — 2026-10-09

Added:
- `windows_job` module (Windows): `WindowsJob`, a kill-on-close job object for children the caller spawned. `WindowsJob::new(Option<u32>)` sets an optional active-process cap; `assign_child`, `assign_process_id`, and `terminate` attach or stop the job. Dropping the job kills every process in it. `DEFAULT_ACTIVE_PROCESS_LIMIT` is 64. Both items are re-exported at the crate root. `rightkit-sandbox` 0.3.2 uses `WindowsJob` (`aef8cd6f`).

Changed:
- The Windows `windows` dependency moves from 0.61 to 0.62 (`a3c74d90`).
- The ownership declaration is revalidated (`7acf96a8`). README gains Windows sections (`653f488d`, `30e300e4`).

Fixed:
- Test only: the exec-failure test writes a non-executable file. On macOS `execvp` re-ran an invalid executable through `/bin/sh`, so the spawn succeeded (`ef139b7d`).
- Test only: argv-only Windows unit tests are annotated as windowless (`4cab4a66`).

## 0.3.1 — 2026-10-08

Breaking:
- `OwnedChild::child()` returns `MutexGuard<PlatformChild>` instead of `MutexGuard<std::process::Child>` (`41c517b0`). On Unix, `PlatformChild` is still `std::process::Child`, so nothing changes there. On Windows, `PlatformChild` is the new `WindowsChild`. Code that names `std::process::Child`, or calls a `std::process::Child` member that `WindowsChild` does not have, no longer compiles on Windows. `WindowsChild` provides `stdin`, `stdout`, `stderr`, `id`, `try_wait`, `wait`, and `kill`. The commit message does not mention this change.

Added:
- `OwnedCommand::inherit_handles_only(handles)` and the `RawHandleOrFd` alias (`41c517b0`). On Windows the child is created with `CreateProcessW` and `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` and stays suspended until job assignment. On Unix, `pre_exec` closes every descriptor at or above 3 that is not listed. Standard streams are always included. Listed handles stay caller-owned.
- `OwnedCommand::windows_spawn_config(WindowsSpawnConfig)` (Windows), with the `WindowsSpawnConfig`, `WindowsStdio`, and `WindowsChild` types (`41c517b0`). A command built with `from_command` or `command_mut` needs an explicit config before an allowlisted spawn. `OwnedCommand::new` supplies a default.
- `JobAssignmentFailure::raw_os_error()` (`41c517b0`).

Changed:
- `wait()` and `wait_timeout()` no longer poll on Windows. Windows blocks on the process handle with `WaitForSingleObject`. On Linux, macOS, and the BSDs, `wait()` uses `waitid` with `WNOWAIT`, so std remains the reap owner. Other Unix hosts use std's blocking `wait`. Unix `wait_timeout()` still polls every 5 ms (`41c517b0`).
- On Windows, `command_mut()` drops an automatically generated spawn config, because a mutable `Command` can change settings that have no getter (`41c517b0`).

Fixed:
- Windows job-assignment and other job errors keep their Win32 code. `HRESULT_FROM_WIN32` values map back to the raw OS error (`41c517b0`).
- Windows children no longer inherit every inheritable handle. The allowlist fixes a case where Membrane's tray daemon held pipes open and blocked EOF (`41c517b0`).
- macOS: `kill(-pgid)` returns `EPERM`, not `ESRCH`, when the group's remaining members are zombies. Once the leader has exited, that now counts as gone (`b345a796`). This fixes the `rightkit-sandbox` test `process_tree_kill_kills_grandchildren_including_group_escapers` on macOS.

## 0.3.0 — 2026-10-08

Public signatures changed only by addition. No public item was removed or re-signed compared with `2e63502b`. The minor bump is the crate's cascade marker for dependents (`ee6ec860`).

Added:
- `TerminationOptions` (`grace`, `windows_exit_code`), with `OwnedChild::terminate_tree_with_options` and `OwnedChild::terminate_tree_bounded_with_options` (`ee6ec860`).
- `JobAssignmentMode` (default `Strict`; `BestEffort` keeps every failure), `OwnedCommand::job_assignment_mode`, `JobAssignmentFailure`, and `OwnedChild::job_assignment_failures` (`ee6ec860`).
- `UnixContainment` (default `ProcessGroup`; `Session`), set with `OwnedCommand::unix_containment` (`ee6ec860`).
- `OwnedCommand::partial_spawn_cleanup_timeout`. Cleanup after a failed spawn is bounded by this timeout, and a child that does not exit in time goes to a background reaper. The default is unbounded, as before (`ee6ec860`).
- `OwnedCommand::spawn_uncontained_detached`: a launch with no owner job and no terminate-on-drop. Unix uses `setsid`; Windows uses breakaway. Combining it with `windows_job` returns `InvalidInput` (`ee6ec860`).
- `ProcessHandle`, an independent exit reader with `try_clone`, `try_wait`, `wait`, `wait_timeout`, and `creation_time`. Windows also has `creation_time_ticks`. Holding one does not keep the tree alive. Reach it through `OwnedChild::process_handle()` (`ee6ec860`).
- `OwnedChild::child()`, `creation_time()`, and (Windows) `creation_time_ticks()` are public (`ee6ec860`).

Changed:
- The `process-wrap` dependency is removed. Spawn, wait, and tree termination are implemented in the crate, in `owned.rs`, `process_handle.rs`, and `process_windows.rs` (`ee6ec860`).
- Unix spawn sets the process group directly with `pre_exec` `setpgid(0, 0)`, or `setsid` for session and detached launches. Tree termination signals the group: TERM, then KILL after the grace period (`ee6ec860`).
- Windows children start suspended and resume after job assignment (`ee6ec860`).
- The Windows `windows` dependency becomes a caret requirement, `0.61` (`ee6ec860`).

Fixed:
- Windows: when `TerminateProcess` returns `ACCESS_DENIED` for a process that is already exiting, the call succeeds if the process exits within 2 s of the error. Job termination is asynchronous, so the direct child can still be exiting (`ee6ec860`, `process_windows::terminate`).

## 0.2.1 — 2026-10-07

Added:
- `OwnedCommand::windows_job(BorrowedHandle)` (Windows). The child joins the caller's job object before RightKit's own kill-on-close job, so closing the caller's job kills the child and its tree (`d579cc1b`).

Changed:
- On Windows, `OwnedCommand` children are created with `CREATE_NO_WINDOW` by default. `windows_hide()` is kept as a no-op (`59abbe4e`).
- On Windows, a child that cannot be bound to the caller's job or to RightKit's job is killed and reaped before the spawn returns an error. Before, the spawn error was returned without that cleanup (`d579cc1b`).

Fixed:
- Test file: the supervision tests build again on macOS after a Windows `clippy --fix` (`b9e709d4`).

Publication: `2e63502b` records gate evidence for crates published from `0faf9aa9`, including process 0.2.0. Git holds no publication record for 0.2.1.

## 0.2.0 — 2026-10-07

Breaking:
- Minimum supported Rust version is raised from 1.81 to 1.98.0 (`343654bf`).

Added:
- `OwnedCommand::env_allowlist` clears the environment and copies only named variables. `OwnedCommand::env_strip` removes named variables. `DEFAULT_ENV_ALLOWLIST` is a starting list (`8230a302`).
- `OwnedChild::terminate_tree_bounded` and `ReapOutcome`. The tree is killed, and the wait is bounded by a timeout (`8230a302`).
- Stall watchdog module: `StallWatchdog`, `StallConfig`, `StallEvent`, `Ack`, and `Cooldown`. Each stall episode is reported once, and recovery re-arms the report (`8230a302`).
- `StderrTail`: bounded capture of child stderr, with an optional line filter and forwarding callback (`8230a302`).
- `RestartPolicy` gains accessors (`mode`, `max_restarts`, `window`, `base_backoff`, `max_backoff`) and a `Default` (automatic, three restarts per 60 s window, backoff from 250 ms to 5 s) (`8230a302`).
- `RestartTracker::restarts_in_window` and `RestartTracker::reset_budget` (`8230a302`).
- `LICENSE-MIT` and `LICENSE-APACHE` are bundled and listed in `include` (`60672efa`).

Changed:
- The Windows `windows` dependency gains the `Win32_Security` and `Win32_System_JobObjects` features (`d3655cbd`).

Fixed:
- Restart permits: the epoch advances on each scheduled restart, so a permit issued before a restart cannot authorize a duplicate child start (`60672efa`).
- Windows: each child is assigned to a second kill-on-close job. process-wrap's job never set `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so a host that dies without destructors (crash, `TerminateProcess`) left its tree running. The second job closes with the host and kills the tree (`d3655cbd`).
- Tests: the Unix process-group suite (`tests/unix_process_group.rs`) and a portable partial-spawn cleanup test cover macOS (`b2aa935d`).

## 0.1.0 — 2026-07-14

Added:
- Initial crate (`2a0becf1`). `OwnedCommand` and `OwnedChild` provide child-tree ownership with `wait_or_kill`, `terminate_tree`, and stdio takers. `AdoptedProcess` observes an existing process without owning it. `RestartTracker`, `RestartPolicy`, and `RestartPermit` manage bounded restarts. `HealthTracker`, `HealthPolicy`, and `HealthProbe` track health.
