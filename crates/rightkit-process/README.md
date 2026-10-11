# rightkit-process

Ownership-safe process lifecycle, restart, and health primitives for Right
Suite desktop apps. It owns child spawn with tree ownership, tree termination,
restart scheduling (`RestartTracker`), and health thresholds (`HealthTracker`).
It contains no app protocol, probe implementation, or business message.

**Use it** wherever an app starts a child process that must not outlive the app, or must be restarted by policy.

**Do not use it** to sandbox a process (use `rightkit-sandbox`), to adopt and kill arbitrary PIDs, or to define an app's health probe.

## Features

None. The crate has no Cargo features. Platform dependencies: `libc` (Unix) and `windows` 0.62 (Windows) (`Cargo.toml:14-18`).

## Platform support

- Unix: children lead a new process group (or a session with `UnixContainment::Session`). Termination signals the group, under the identity rule below.
- Windows: children are created suspended, assigned to a Job Object, then resumed. Termination uses the Job Object.
- The crate has `cfg(unix)`, `cfg(windows)`, `cfg(target_os = "macos")`, and `cfg(target_os = "linux")` branches across `src/owned.rs`, `src/process_handle.rs`, `src/process_unix.rs`, `src/process_windows.rs`, and `src/spawn_windows.rs`.

Published version: see `INDEX.md` at the repository root. Release notes: `CHANGELOG.md`.

## Ownership boundary

- `OwnedCommand` is the only way to create an `OwnedChild`. Ordinary spawns own
  their tree; explicitly detached spawns leave lifetime with the caller.
- `AdoptedProcess` can only report an existing PID and whether it is running.
  It deliberately has no terminate method. There is no public `kill_pid` API.
- Dropping an ordinary `OwnedChild` terminates and reaps its tree. A failure
  after OS spawn also cleans up the partial child.

Windows children are created suspended, assigned to a Job Object, then
resumed. Children are hidden by default (`CREATE_NO_WINDOW`); `windows_hide()` is a no-op kept for compatibility.
Explicit termination uses the Job Object, so descendants die while unrelated
sibling processes survive. Unix children lead a new process group;
`wait_or_kill` sends `SIGTERM` to the group, waits for the leader for the bounded grace
period, then sends `SIGKILL` to the group and reaps it. If the leader exits inside the grace,
descendants that are still running are killed before the leader is reaped.

Windows resumes the suspended primary thread only after assignment decisions.

## Unix process-group identity

A process-group id can be recycled for an unrelated group once the leader has been reaped and
the group is empty, so a signal sent to it afterwards can hit a program that is not ours.
Version 0.3.3 checked group membership after the reap and then signalled, which left a window
in which that could happen. The rule that replaces it:

- **A group signal is sent only while this process still holds the leader as its own unreaped
  child.** An unreaped child, running or exited, pins its pid and group id; only this process
  can reap it. Every group signal is sent under the same lock that guards reaping, after the
  leader is confirmed unreaped, so no reap can fall between the check and the signal.
- **After the first reap, by any path, group signalling is over** for that `OwnedChild` and all
  its `ProcessHandle` clones: no signal, and no `kill(-pgid, 0)` membership probe. Paths that
  reap: `OwnedChild::wait`, `try_wait`, `wait_timeout`, the same methods on a `ProcessHandle`
  or a clone, the detached reaper, and `wait`/`try_wait` through `OwnedChild::child()`.
- **`wait()` followed by drop signals nothing.** Grandchildren that outlive a plainly waited
  leader are not killed. To kill them, use `terminate_tree`, `terminate_tree_with_options`,
  `terminate_tree_bounded`, or `wait_or_kill` while the leader is still unreaped. These keep
  an exited leader unreaped (it is observed with `waitid(WNOWAIT)`), signal the group while
  that zombie pins the id, and only then reap it.
- A handle clone that waits on another thread reaps as soon as the leader exits, and so
  retires group signalling for the owner too. A program that needs both a monitor thread and
  tree termination must not let the monitor reap: **a monitor calls `ProcessHandle::wait_exited`
  (or `wait_exited_timeout(timeout) -> bool` when it enforces a deadline), which returns once
  the leader has exited and leaves it an unreaped zombie.** The owner then kills the group with
  `wait_or_kill` or `terminate_tree*` and reaps afterwards. A caller that drains the leader's
  pipes (a run loop, `rightkit-sandbox`'s `wait_with_output`) keeps the leader unreaped until
  the drain is over, so a descendant that outlives the leader and holds the pipes is still
  killed with the group at the deadline. A plain `ProcessHandle::wait` on another thread
  reaps and ends that. On Unix hosts other than Linux, Android, macOS and iOS there is no
  non-reaping probe, so `wait_exited` reaps like `wait` there.
- **An unreaped child is not proof the pid is still ours.** Other code in the process can collect
  the leader (`waitpid(-1)`) and a replacement child can take its number. So every signal, every
  `waitpid`/`waitid` and every group-membership probe that names the pid first compares the
  kernel's description of it (parent and start time: `proc_pidinfo` on macOS, `/proc/<pid>/stat`
  on Linux) with the one taken at spawn. A different or missing description marks the leader
  lost: nothing is sent, waited or reaped (`wait` answers `ECHILD`) and the replacement keeps its
  status. A child the host cannot describe at spawn is stopped and the spawn fails. The same
  proof guards the macOS parent-death helper's reap, and a helper that cannot be described is
  never waited on. The residual is the gap between the proof and the call against foreign code
  that reaps in exactly that gap and a replacement that takes the number there; no user-space API
  closes it. `OwnedChild::while_unreaped` runs a caller's pid-based action (a descendant sweep)
  under the same proof and lock.
- **Waits are event-driven, never timed.** Each Unix child owns an exit watch opened at spawn: a
  kqueue `EVFILT_PROC`/`NOTE_EXIT` knote on macOS, a pidfd on Linux and Android (a thread blocked in
  `waitid(WNOWAIT)` where the kernel has no pidfd). Both are bound to the process, not to its
  number. `ProcessHandle::wait`, `wait_timeout`, `wait_exited*`, `OwnedChild::wait*`,
  `wait_or_kill` and the tree-wide grace of `terminate_tree*` read the shared state under the
  lock, release it, and sleep in `poll(2)` on the watch (the grace, on one `poll` over the leader
  and every other group member), so a wait costs one wake-up however long the child lives and
  `kill` or `terminate_tree` from another thread runs while it blocks. After the exit event the
  pid is proven and reaped under the lock. Only a wait made through the `OwnedChild::child()`
  guard (`UnixChild::wait`) blocks with the lock held, because the caller holds it. Linux leader
  signals go through `pidfd_send_signal`, and the proof is a signal-0 pidfd probe, neither of which
  involves the pid number; a group signal is still `kill(-pgid)` after that proof. Hosts with no
  event source (Unix other than Linux, Android, macOS and iOS, or a process out of descriptors)
  refuse bounded waits with `ErrorKind::Unsupported`, skip a termination grace, and reap an
  unbounded `wait` with a blocking call. Each child holds one extra descriptor (the watch).
- `OwnedChild::child()` no longer exposes the std `Child` on Unix. It returns `UnixChild`
  (stdio fields, `id`, `try_wait`, `wait`, `kill`), so a reap through it is recorded.
- `UnixChild::kill`, and every direct-pid signal for detached children, follow the same rule.

## Also (0.3.0)

- `OwnedChild::child()` borrows `UnixChild` on Unix (0.3.4; std `Child` before) or `WindowsChild` on Windows through
  a lock guard, including stdin/stdout/stderr and wait/try_wait; existing stream-taking APIs remain.
- `unix_containment(UnixContainment::Session)` runs `setsid`; Windows
  `job_assignment_mode(JobAssignmentMode::BestEffort)` retains non-fatal errors
  in `job_assignment_failures()`. Strict assignment remains default.
- `terminate_tree_with_options(TerminationOptions)` sends Unix TERM immediately,
  then KILL after configurable tree-wide `grace` (zero means immediate escalation).
  On Unix both signals go out while the leader is unreaped (see the identity rule above);
  Windows' retained job also works after parent exit; `windows_exit_code` accepts 1067.
  `terminate_tree_bounded_with_options(timeout, options)` bounds grace plus reap.
- `spawn_uncontained_detached()` skips owner containment and survives owner drop.
  Windows requires parent-job breakaway permission and fails without fallback.
  Unix starts a separate session and reaps asynchronously; configure stdio so EOF
  or closed pipes do not request application shutdown.
- `process_handle()` returns an independent `ProcessHandle` exit reader/waiter.
  Windows implements `AsHandle` with an owned duplicate and exposes FILETIME
  `creation_time_ticks()`; `creation_time()` reports native time on Windows/macOS,
  spawn observation time on other Unix hosts. Unix readers share the owner's cached reap
  state, and a reap by a reader ends group signalling for the owner (identity rule above).
  `wait_exited()` / `wait_exited_timeout(timeout)` wait for the exit without reaping (Unix) and
  so leave the group killable; use them for a monitor.
  Release `child()` guards before calling another accessor or reader.
- `partial_spawn_cleanup_timeout(Duration::from_secs(5))` bounds failed-spawn
  cleanup; default remains unbounded. Timed-out children go to a background reaper.

## Also (0.3.4)

- `OwnedCommand::kill_on_parent_exit(true)` is an opt-in tie to the supervising process for the case where
  the parent dies without cleanup (SIGKILL, abort, crash). Destructors and `wait_or_kill` cannot run then,
  so the kernel or a helper has to do the work:
  - Linux: `prctl(PR_SET_PDEATHSIG, SIGTERM)` is set in `pre_exec`, and `getppid()` is checked afterwards so a
    supervisor that died before the prctl makes the spawn fail. The signal goes to the direct child only.
    It fires when the thread that spawned the child exits, not only when the process exits, so spawn from a
    thread that lives as long as the child. The child gets one TERM and no KILL escalation, so a child that
    ignores TERM survives. Descendants in the group are not signalled.
  - macOS: a helper process, forked by the supervisor itself (never by a second `fork` inside the new process,
    which would re-run `pthread_atfork` handlers there), joins the child's process group and waits on
    `EVFILT_PROC`/`NOTE_EXIT` for the supervisor. The engine blocks in `pre_exec`, before `exec`, until the
    helper confirms its watch. When the supervisor dies, the helper sends `SIGTERM` to the group, waits 2 s, then
    sends `SIGKILL` to the group. It ignores `SIGTERM` itself, closes every inherited descriptor except its three
    pipe ends (found by asking the kernel, not guessed; if it cannot, the spawn fails), and stays in the group so the group id cannot be
    recycled under its signal.
  - **How long the macOS tie lasts:** the helper outlives the engine. It lives as long as the `OwnedChild`
    (and any `ProcessHandle` clone) does, so a supervisor that dies while the `OwnedChild` still exists also
    kills descendants that outlived the engine. When the last owner is dropped the helper is told to stand
    down and collected, and nothing is protected afterwards. Cost: one extra process per opted-in child while
    its owner lives.
  - **The helper's pid is never signalled.** It is released over a private pipe (one byte; it then exits by
    itself with status 0), because an embedding's `waitpid(-1)` can reap it behind our back and the pid can
    be recycled for an unrelated process. The pid is forgotten once `waitpid(pid, WNOHANG)` says it is gone
    or `ECHILD`. End of file on that pipe is not a release (the supervisor's death closes it too); the
    helper then keeps waiting for the supervisor's exit and still kills the group.
  - **Descriptor closure fails closed.** The helper lists its descriptor table with the kernel after the fork
    and closes everything but its three pipe ends. If it cannot list the table it cannot prove it holds no
    pipe writer a reader of the supervisor waits on, so the spawn fails (`EIO`, or `EPIPE` if the engine
    wrote after the helper exited) instead of walking up to a number taken before the fork.
  - macOS with `UnixContainment::Session` fails with `Unsupported`: `setsid` puts the engine in another session,
    and the helper can only join a group inside its own session.
  - Other Unix hosts: the spawn fails with `Unsupported`.
  - Windows: a no-op. Kill-on-close job objects already terminate the tree when the owner handle closes.
  - Combining the option with `spawn_uncontained_detached` returns `InvalidInput`.
- `tests/parent_death.rs` covers the Linux and macOS behavior by re-executing the test binary as the parent,
  SIGKILLing it, and checking the child pid. The group-wide and outlived-engine cases run on macOS only.

## Example and usage

Illustrative; not compiled as a doctest. The compiled checks are `tests/api.rs`, `tests/supervision_primitives.rs`, and `tests/restart_health.rs`.

`OwnedCommand::inherit_handles_only` restricts inheritance to caller handles/fds plus stdio; Windows requires `windows_spawn_config` for opaque stdio, environment-inheritance & raw-argument settings.

```rust,ignore
use std::{process::Stdio, time::Duration};
use rightkit_process::{OwnedCommand, WaitOutcome};

let mut command = OwnedCommand::new("my-engine");
command.command_mut().stdout(Stdio::piped());
command.windows_hide();
let mut child = command.spawn()?;
let stdout = child.take_stdout();

// The app sends its own graceful protocol message before this call on Windows.
match child.wait_or_kill(Duration::from_secs(2))? {
    WaitOutcome::Exited(status) => assert!(status.success()),
    WaitOutcome::Terminated(_) => { /* grace expired */ }
}
# Ok::<(), std::io::Error>(())
```

`StderrTail::capture(stderr, max_bytes, map, on_line)` keeps at most `max_bytes` bytes of the most recent
lines (a single longer line is cut to its last bytes) and shows `map`, the redactor, only whole lines. A
line longer than four times `max_bytes` is discarded up to its newline and replaced by one marker line
(`[stderr line over N bytes dropped]`), because handing a line-based redactor the pieces of a cut line
would let a secret that spans the cut through.

`RestartTracker` is a pure rolling-window/backoff state machine. Restart
permits are single-use scheduling authorities: issuing a replacement permit,
starting a new generation, or shutting down invalidates every older permit.
`RestartMode` is `Automatic`, `OnDemand`, or `Disabled`.

`HealthTracker` is also pure: callers provide monotonic `Duration` timestamps
and probe outcomes, making startup timeout and consecutive-failure thresholds
deterministic in tests.

## Verification

Run through the RightKit wrapper, per the workspace's `docs/rules/rightkit.md`
(run the Unix suites in both thread modes: default and `RUST_TEST_THREADS=1`):

```text
rightkit cargo test -p rightkit-process --all-targets
rightkit cargo fmt --check
rightkit cargo clippy -p rightkit-process --all-targets -- -D warnings
rightkit cargo package -p rightkit-process --allow-dirty --no-verify --list
```

`tests/windows_process_tree.rs` covers parent and grandchild termination, sibling
survival, bounded grace, kill-on-drop, and partial-spawn cleanup on Windows.
`tests/unix_process_group.rs` covers Unix process-group behavior. Run both suites on
their own host platforms.

## Windows

- Spawn: `CreateProcessW` with `CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT`, plus `CREATE_NO_WINDOW` (the default) or `CREATE_BREAKAWAY_FROM_JOB` (detached). Inherited handles are limited by a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` built from the caller's handles and stdio.
- Ownership order: a caller job from `windows_job()` is assigned first, then a new job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The primary thread is resumed by ID from a Toolhelp thread snapshot, so the child does not run outside its job.
- `job_assignment_mode(BestEffort)` records caller-job and owned-job errors in `job_assignment_failures()`. Strict, the default, fails the spawn.
- Termination: `terminate_tree` calls `TerminateJobObject` on the owned job, then `TerminateProcess` on the direct child. An ACCESS_DENIED from a child that is already exiting gets a 2 s wait. Processes outside the job are untouched.
- Detached spawns (`spawn_uncontained_detached`) need the parent job to allow breakaway. The spawn fails without fallback when it does not, and a detached spawn cannot take a caller job.
- Errors: a `HRESULT_FROM_WIN32` value (0x8007xxxx) is returned as the raw OS error. Other failures are `io::Error::other`.
- Dependency: `windows` 0.62 under `cfg(windows)` (`Cargo.toml:18`), with features `Win32_Foundation`, `Win32_Security`, `Win32_System_Diagnostics_ToolHelp`, `Win32_System_JobObjects` and `Win32_System_Threading`. Consumers enable no Windows features.
- The code has no session or desktop check. Nothing here was run on Windows for this section.

Licensed under either MIT or Apache-2.0, at your option.
