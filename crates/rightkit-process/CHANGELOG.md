# Changelog

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
- `kill_on_parent_exit` on macOS: the helper is forked by the supervisor, not by a second `fork` inside the new
  process. The nested fork re-ran registered `pthread_atfork` handlers in the new process and could deadlock
  before any timeout started. The engine now blocks in `pre_exec`, before `exec`, until the helper has armed.
- `kill_on_parent_exit` on macOS: the helper closes every inherited descriptor except its three pipe ends by
  listing them with the kernel (`proc_pidinfo(PROC_PIDLISTFDS)`), instead of closing up to the highest known
  descriptor plus 1024. A pipe writer another thread duplicated higher than that bound stayed open in the helper
  (which never execs, so close-on-exec does not help) and kept readers from ever seeing end of file. If the
  kernel will not list the table, the helper no longer falls back to a walk bounded by a number taken before
  the fork (a writer duplicated above that number, after it was taken, stayed open): it cannot prove it closed
  everything, so it fails the spawn closed (`EIO`, or `EPIPE` if the engine wrote after the helper exited) and
  exits, which releases whatever it held.
- `kill_on_parent_exit` on macOS: the tie no longer ends when the engine exits. The helper lives as long as the
  `OwnedChild` (and its handle clones), so a supervisor that dies while the `OwnedChild` exists still takes down
  descendants that outlived the engine. After the last owner is dropped nothing is protected.
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
- **macOS helper: no pid is signalled.** After `ECHILD` (another reaper in the process took the helper) the
  `UnixChild` kept the pid and a later drop sent `SIGKILL` to it, by then possibly an unrelated process. The
  helper is now told to stand down over a private pipe (one byte; the helper exits by itself with status 0),
  its pid is forgotten as soon as `waitpid` says it is gone or `ECHILD`, and `waitpid(pid, WNOHANG)` is the
  only call that names it. End of file on that pipe is not a release: the supervisor's own death closes it
  too, so the helper keeps waiting for `NOTE_EXIT` and still kills the group.
- **Waits cannot follow a recycled pid.** `ProcessHandle::wait` (and `wait_exited`) used to read the shared
  state, release the lock and then block in `waitid(pid)`; a pause between the two let another reader reap, the
  kernel recycle the number, and the waiter block on an unrelated child. Waiters now look under the lock,
  release it, sleep and look again, so no wait system call names the pid outside the lock. **Behaviour
  change:** `wait` polls (0.25 ms for the first 20 ms, 2 ms to 1 s, 10 ms to 30 s, 50 ms after that) instead
  of blocking in the kernel, so an exit is noticed up to that long after it happens. Other Unix hosts no
  longer hold the child lock for the whole wait either.
- **`StderrTail` redaction and byte cap.** A line longer than four times `max_bytes` was cut and each piece
  handed to the line-based `map` as if it were a line, so a secret spanning the cut passed through unredacted,
  and a single retained line could exceed `max_bytes`. Such a line now never reaches `map`: it is read to its
  newline and discarded, and one marker line, `[stderr line over N bytes dropped]`, is forwarded and retained
  in its place. A retained line longer than the budget is cut to its last bytes (at a character boundary), so
  `snapshot()` is never longer than `max_bytes`; `max_bytes == 0` retains nothing.
- Test only: the sampling test no longer runs on Unix targets where sampling returns `Unsupported`, and a
  test asserts that error there. `tests/high_descriptor.rs` now duplicates the writer before the helper is
  forked (it used to do so after, in the engine, so the helper never held it); the fail-closed case with the
  listing forced to fail is `an_unlistable_descriptor_table_fails_the_spawn_and_frees_a_writer_above_any_bound`
  in `src/process_unix.rs`.

Breaking (Unix, public items):
- `OwnedChild::child()` returns `MutexGuard<UnixChild>` instead of `MutexGuard<std::process::Child>`. The new
  public type `UnixChild` has the stdio fields `stdin`, `stdout`, `stderr` and the methods `id`, `try_wait`, `wait`
  and `kill`, mirroring `WindowsChild`. Code that named `std::process::Child`, or used any other `Child` member
  through the accessor, no longer compiles. The std `Child` is no longer reachable, because a reap through it
  could not be seen by the crate. (`PlatformChild`, the alias behind the accessor, follows.)
- `kill_on_parent_exit(true)` combined with `UnixContainment::Session` fails with `ErrorKind::Unsupported` on
  macOS (the helper cannot join a process group in another session). Linux is unchanged.
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
- Windows: `OwnedCommand::spawn_detached_tolerating_denied_breakaway()` returns `(OwnedChild, DetachedBreakaway)`.
  It tries `CREATE_BREAKAWAY_FROM_JOB`; on `ERROR_ACCESS_DENIED` it retries once with `DETACHED_PROCESS |
  CREATE_NO_WINDOW` inside the parent job and reports `DetachedBreakaway::StayedInJob` (else `BrokeAway`). The
  plain `spawn_uncontained_detached` is unchanged and still never falls back.
- `OwnedCommand::kill_on_parent_exit(bool)`, off by default. On macOS and Linux, the child's process
  group is terminated when the supervising process dies without cleanup, such as SIGKILL.
  - Linux sets `PR_SET_PDEATHSIG(SIGTERM)` on the direct child in `pre_exec`, then checks `getppid()` so the spawn
    fails if the supervisor already died. The guarantee is weaker: the direct child only, one catchable
    `SIGTERM`, keyed to the forking thread, with no group kill and no `SIGKILL` escalation.
  - macOS: a helper process joins the child's process group and watches the supervisor with `EVFILT_PROC`
    `NOTE_EXIT`; on the supervisor's death it sends `SIGTERM` to the group, waits 2 s, then `SIGKILL`. The helper
    confirms over a pipe once its watch is armed and the engine waits up to 5 s for that. The spawn fails (`ESRCH`
    if the supervisor is already gone or was replaced, `ETIMEDOUT`, `ECONNRESET`, or the registration `errno`) and
    the helper is released and collected, so a child is never left running untied. The helper lives as long as the
    owner (see "Fixed").
  - Other Unix hosts fail the spawn with `ErrorKind::Unsupported` rather than run untied.
  - Windows is unchanged and the option is a no-op there, because kill-on-close job objects already cover parent death.
  - Combining the option with `spawn_uncontained_detached` returns `InvalidInput`.
- `process_usage`, `observe_idle`, `ProcessUsage`, `UsageRates`, `IdleObservation` and `BudgetLabels`: resource
  usage of the current process (macOS, Windows, Linux/Android).
- Tests: `tests/tree_identity.rs`, `tests/high_descriptor.rs`, `tests/atfork_handlers.rs`, `tests/parent_death.rs`
  (Linux and macOS; group-wide and outlived-engine cases on macOS), `tests/spawn_ready.rs`, and unit tests that
  record every signal and reap in order, including forced-id-reuse and clone-interleaving cases.
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
