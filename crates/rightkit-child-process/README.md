# rightkit-child-process

Ownership-safe process lifecycle, restart, and health primitives for Right
Suite desktop apps. It owns child spawn with tree ownership, tree termination,
restart scheduling (`RestartTracker`), and health thresholds (`HealthTracker`).
It contains no app protocol, probe implementation, or business message.

**Use it** wherever an app starts a child process that must not outlive the app, or must be restarted by policy.

**Do not use it** to sandbox a process (use `rightkit-sandbox`), to adopt and kill arbitrary PIDs without an identity check, or to define an app's health probe.

## Features

One optional feature, off by default: `tokio` (adds `tokio` with `process` and `time`): `NoWindow` for `tokio::process::Command`, `tokio_support::{command, lead_session, terminate_tree, terminate_process_group, terminate_session}`. Platform dependencies: `libc` (Unix) and `windows` 0.62 (Windows). Also without the feature: `NoWindow` for `std::process::Command`, `OutputCapture` / `read_bounded` (byte-exact bounded capture of a stream).

## Platform support

- Unix: children lead a new process group (or a session with `UnixContainment::Session`). Termination signals the group, under the identity rule below. `kill_on_parent_exit` is Linux and macOS (see below).
- Windows: children are created suspended, assigned to a Job Object, then resumed. Termination uses the Job Object.
- The crate has `cfg(unix)`, `cfg(windows)`, `cfg(target_os = "macos")`, and `cfg(target_os = "linux")` branches across `src/owned.rs`, `src/process_handle.rs`, `src/unix_child.rs`, `src/exit_watch.rs`, `src/identity.rs`, `src/process_unix.rs`, `src/process_windows.rs`, and `src/spawn_windows.rs`.

Published version: see `INDEX.md` at the repository root. Release notes: `CHANGELOG.md`.

## Ownership boundary

- `OwnedCommand` is the only way to create an `OwnedChild`. Ordinary spawns own
  their tree; explicitly detached spawns leave lifetime with the caller.
- `AdoptedProcess` can only report an existing PID and whether it is running.
  It deliberately has no terminate method. There is no public `kill_pid` API.
  `AdoptedProcess::identity` yields an `AdoptedIdentity` (pid, start time, executable)
  whose `signal` verifies the pid still names that process before acting and refuses
  otherwise; `sweep` and `sweep_descendants` apply the same rule to descendants.
- `reveal` shows or opens a file in the system file manager, only inside caller-supplied
  roots (`PathScope`); it refuses escapes and runnable files.
- Dropping an `OwnedChild` kills at once and waits for the reap for at most
  `drop_reap_timeout` (default 2 s), then reaps in the background.
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
  status. A child the host cannot describe at spawn is stopped and the spawn fails.
  `OwnedChild::while_unreaped` runs a caller's pid-based action (a descendant sweep) under the
  same proof and lock. **The proof does not close the race entirely; see "Known limits".**
- **Waits are loops of non-blocking looks, and the exit event is only a hint.** `ProcessHandle::wait`,
  `wait_timeout`, `wait_exited*`, `OwnedChild::wait*`, `wait_or_kill`, `terminate_tree*`, `Drop`, the
  detached reaper thread (`rightkit-detached-reap`) and the failed-spawn cleanup (up to its timeout) take
  the shared lock for one non-blocking look (`waitpid(WNOHANG)`, or the non-reaping
  `waitid(WNOHANG | WNOWAIT)` for `wait_exited*`), release it, sleep, and look again until the leader
  is reaped or the caller's deadline passes. None of them reaps with a blocking call or sleeps while
  holding the lock, so a deadline is honoured whatever the child does, `kill` or `terminate_tree` from
  another thread runs while one of them is in progress, and no wait system call names the pid outside
  the lock. The sleep between looks is a kqueue `NOTE_EXIT` / pidfd wake-up where the kernel offers one
  (at most 1 s per sleep, ending early at the exit), and 5 ms, the interval 0.3.3 polled at, where it
  does not (no such facility, no descriptor to spare, or a process already exiting at spawn). The event
  never decides anything: the look after it does. The tree-wide grace of `terminate_tree*` lists the
  group every 5 ms until only the exited, unreaped leader is left, or the grace ends. Each child holds
  one extra descriptor (the hint) while it is alive. Waiting threads wake about once a second with the
  hint and about 200 times a second without it, for as long as the child lives (0.3.3's Linux and macOS
  `wait()` blocked in the kernel).
- **Where the shared lock is held across a wait or a long action: the complete list.** Everything else
  releases it between looks.
  1. **`wait` through the `OwnedChild::child()` guard (`UnixChild::wait`).** The guard is the lock, so
     the wait holds it until the child has exited, and a bounded wait on a clone, a `kill`,
     `terminate_tree` or `Drop` on another thread queues behind it and can exceed its own deadline. The
     caller chose to hold the guard; `std::process::Child::wait` through the same accessor did the same
     in 0.3.3, and the accessor's contract has always been "release this guard before another accessor
     or exit reader". It still reaps only with non-blocking looks. Wait through `OwnedChild::wait`,
     `wait_timeout` or a `ProcessHandle` when anything else must stay live. (Windows:
     `WindowsChild::wait` through the guard blocks in `WaitForSingleObject` under the lock, as in 0.3.3.)
  2. **Failed-spawn cleanup with no timeout** (the default), and
  3. **the `rightkit-partial-reap` thread** it hands a timed-out child to. Both wait under the lock, as
     0.3.3 did. They run only for a child whose spawn failed after the process existed, and nothing
     but the spawning thread (then the reaper thread) holds that child: the partial `OwnedChild` was
     never returned, no `ProcessHandle` was cloned from it, so no other thread can reach its lock.
  4. **`OwnedChild::while_unreaped`** holds the lock while the caller's closure runs (that keeps a reap
     out between the proof and the action), so a bounded wait on another thread is delayed by as long
     as the closure takes. `rightkit-sandbox` passes a descendant sweep with a 200 ms grace. New since
     0.3.3.
  The spawn-time identity check (`require_identity`) reaps a just-killed child with a blocking wait
  before the child is shared with anyone.
- `OwnedChild::child()` no longer exposes the std `Child` on Unix. It returns `UnixChild`
  (stdio fields, `id`, `try_wait`, `wait`, `kill`), so a reap through it is recorded.
- `UnixChild::kill`, and every direct-pid signal for detached children, follow the same rule.

## Also (0.3.0)

- `OwnedChild::child()` borrows `UnixChild` on Unix (0.4.0; std `Child` before) or `WindowsChild` on Windows through
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
  Windows asks for parent-job breakaway first; if the caller's job denies it (`ERROR_ACCESS_DENIED`), it retries
  once as `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` inside that job, and `OwnedChild::stayed_in_parent_job()`
  reports it. `spawn_uncontained_detached_strict()` never falls back: it fails with a `PermissionDenied`
  `io::Error` wrapping `BreakawayDenied`. Unix is unchanged.
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

## Also (0.4.0)

- **Windows has no parent-death tie.** `kill_on_parent_exit` is a no-op on Windows. A child there is
  contained by its kill-on-close job object instead, so an app does not need its own parent-death
  watchdog: dropping or losing the owner closes the job and terminates the tree.
- `OwnedCommand::kill_on_parent_exit(true)` is an opt-in tie to the supervising process for the case where
  the parent dies without cleanup (SIGKILL, abort, crash). **Linux only.** `prctl(PR_SET_PDEATHSIG, SIGTERM)`
  is set in `pre_exec`, and `getppid()` is checked afterwards so a supervisor that died before the prctl makes
  the spawn fail. The signal goes to the direct child only. It fires when the thread that spawned the child
  exits, not only when the process exits, so spawn from a thread that lives as long as the child. The child
  gets one TERM and no KILL escalation, so a child that ignores TERM survives. Descendants in the group are not
  signalled.
  - macOS (0.4.2): a watchdog process, because macOS has no parent-death primitive. After the child starts,
    `spawn` forks a helper (double fork, so it is never this process's zombie; `setsid`; every inherited
    descriptor closed) that waits in a `kqueue` for `EVFILT_PROC | NOTE_EXIT` on this process and on the child.
    If this process exits first (SIGKILL included) it sends `SIGTERM` to the child's process group, and `SIGKILL`
    two seconds later if the group's leader is still there; if the child exits first it just exits. The helper
    never waits for or reaps any process, so it cannot take another child's status; the only pid this process
    waits on is the helper's short-lived middle process. `spawn` returns only after the helper reported both
    registrations (the child is still this process's unreaped child then, so the registration cannot name a
    recycled pid) and kills the child and fails if it cannot be armed. Trade-offs against Linux: one extra
    process per tied child; the trigger is the whole process exiting, not the spawning thread; the signal reaches
    the group and escalates to KILL; the child runs untied from its spawn until `spawn` returns; and the
    helper's final signal names the pid after the owner died (a child that exits and whose number is recycled
    inside that window of microseconds could be mistaken for the child).
  - Other Unix hosts: the spawn fails with `ErrorKind::Unsupported` before anything is started.
  - Windows: a no-op. Kill-on-close job objects already terminate the tree when the owner handle closes.
  - Combining the option with `spawn_uncontained_detached` returns `InvalidInput`.
- `tests/parent_death.rs` (Linux and macOS) re-executes the test binary as the parent, SIGKILLs it, and checks the
  child pid. `src/owned.rs` has the unit tests for the refusals.

## Also (0.4.2)

- `terminate_all_owned(TerminationOptions) -> TerminateAllReport` stops every child this process owns (every
  `OwnedCommand::spawn` whose `OwnedChild` is not dropped) in one call, for a shutdown path. Unix: TERM to every
  group, one shared `grace`, KILL to every group, bounded reap, each signal under the identity rule. Windows: the
  retained jobs and processes are terminated (no graceful stage). Detached children are not owned and not touched.
  `owned_count()` reports how many are listed. The list holds weak references only. Tests:
  `tests/owned_tree_control.rs`.
- `OwnedChild::terminate_tree_deep(TerminationOptions) -> TreeTermination` is the on-demand tree kill for a child
  that is not terminated on drop (`spawn_uncontained_detached`) and works for ordinary children too: it records the
  descendants' identities, signals the leader's whole group (a detached child leads its own session), reaps the
  leader, then sweeps the recorded descendants by identity (a recycled pid is refused). Descendants whose parent
  already exited before the call are not findable.
- `find_processes(&ProcessQuery)` and `sweep_matching(&ProcessQuery, &MatchSweepOptions)` find, and optionally stop,
  processes by exact executable path (`exe`), exact argument token (`arg`) and/or a substring of one argument
  (`arg_containing`); all set criteria must hold and at least one is required. Only the current user's processes are
  considered, never this process or its ancestors, and a process whose path or command line cannot be read never
  matches. `MatchSweepOptions::dry_run(true)` returns the matches and sends nothing. The stop is the identity-checked
  `sweep` (pid plus start time), so a recycled pid is refused. Linux reads `/proc`, macOS `proc_pidpath` and
  `KERN_PROCARGS2`, Windows `QueryFullProcessImageNameW` and `NtQueryInformationProcess`. A process that `exec`s a
  different program between the scan and the signal keeps its identity and is not re-checked. Tests:
  `tests/sweep_matching.rs`.

## Also (0.5.2)

- `process_table() -> io::Result<Vec<ProcessEntry>>` snapshots every process the OS lets this process see, so callers
  stop shelling out to `ps` or PowerShell `Win32_Process`. A `ProcessEntry` has `pid`, `parent_pid`, `command_line:
  Option<String>` and `working_set_bytes: Option<u64>` (resident memory in bytes) and is `#[non_exhaustive]`. The
  command line is one string: Linux and macOS join the arguments with a single space (quoting is not recoverable, the
  macOS environment is excluded), Windows returns the raw command line. A field that cannot be read (another user's,
  protected or exiting process, a kernel thread) is `None`; it never fails the scan. On Linux and macOS a process whose
  parent cannot be read (it exited mid-scan) is left out. The table is a racy snapshot and pids are reused, so use
  `AdoptedIdentity` before signalling anything found in it. Linux reads `/proc/<pid>/{stat,statm,cmdline}`, macOS
  `proc_listpids`, `proc_pidinfo` and `KERN_PROCARGS2`, Windows a Toolhelp snapshot, `GetProcessMemoryInfo` and
  `NtQueryInformationProcess` (command line needs Windows 8.1+). No shell-out, no new dependency or feature, nothing
  held between calls. Other hosts return `ErrorKind::Unsupported`. Tests: the unit tests in `src/process_table.rs`.

## Command builders (0.5.2)

- Command builders that are already no-window: `command_std(program)` returns a
  `std::process::Command`, and with feature `tokio`, `tokio_support::command(program)` returns a
  `tokio::process::Command`. Both return by value, so they chain. To keep app code from building
  bare commands, add to the app's `clippy.toml`:

  ```toml
  disallowed-methods = [
    { path = "std::process::Command::new", reason = "use rightkit_child_process::command_std" },
  ]
  ```

  `tokio_support::lead_session(&mut tokio::process::Command)` puts the child in a new session on
  Unix (`setsid`) without a hand-written `pre_exec`. `CREATE_BREAKAWAY_FROM_JOB` is exported on
  every platform next to `CREATE_NO_WINDOW`.
- `run(program, args, &RunOptions)` runs a program to completion with a deadline (the tree is
  terminated at the deadline), bounded stdout and stderr capture, and no window on Windows by
  default. It returns the status, `timed_out`, and both `CapturedOutput`s.
- `run_command(OwnedCommand, input, &RunOptions)` is `run` / `run_with_stdin` for a command the
  caller configured (environment, working directory); the same bounded stdin writer and tree kill
  apply. `run_streaming(cmd, &StreamOptions, cancel, on_stdout, on_stderr_line)` shows output while
  the child runs: raw stdout chunks and CR-aware stderr lines arrive on the caller's thread, a cancel
  predicate and a timeout end the whole tree with a bounded kill and reap, and only the last bytes
  of each stream are kept (`StreamOutput`).
- `open_url(url)` opens a URL with its scheme's handler (`file:` is refused). `launch_app(id)`
  launches an app by macOS bundle identifier or Windows AppUserModelID. Both hand off to a
  detached desktop launcher, as `reveal` does.
- `reveal::open_folder(path, scope)` opens the folder itself. A folder is refused only when it is
  a real `.app` or `.workflow` bundle, so a folder named `chart.js` opens. `reveal::open_installer`
  runs a Windows `.exe`, `.msi` or `.msp` through the shell (`explorer.exe`), so SmartScreen and
  Mark-of-the-Web checks still apply; other platforms return `Unsupported`.
- `login_shell::path(timeout)` and `login_shell::env_values(names, timeout)` read the login
  shell's `PATH` and selected variables under a hard deadline (Unix; nothing on Windows). Values
  are secrets and are never logged.
- `WindowsJob::builder()` adds `job_memory_limit` and `process_memory_limit` next to the
  active-process cap. `peak_job_memory()` and `exit_reason()` report `ExitReason::JobMemoryLimit`
  when the job's peak memory reached its job limit (evidence, not a per-process event).
  `WindowsJob::contain_current_process()` puts this process in a kill-on-close job and leaks the
  handle for the process lifetime; it fails with `PermissionDenied` when an outer job forbids
  nesting.
  `WindowsJobBuilder::allow_breakaway(true)` adds `JOB_OBJECT_LIMIT_BREAKAWAY_OK` (a member leaves
  the job only by creating a process with `CREATE_BREAKAWAY_FROM_JOB`; the rest stay contained). A
  caller-built job has no silent mode. `WindowsJob::contain_current_process_with(builder)`
  applies those options to the current-process job, so a daemon can contain its descendants and
  still start a replacement with `CREATE_BREAKAWAY_FROM_JOB` that outlives it. Without breakaway the
  flag fails with `ERROR_ACCESS_DENIED`.
- `OwnedCommand::windows_visible()` clears the hidden-console default. `windows_allow_breakaway(true)`
  sets explicit-only breakaway (`JOB_OBJECT_LIMIT_BREAKAWAY_OK`): only grandchildren that request
  breakaway can outlive the helper, all others stay contained. `windows_allow_silent_breakaway(true)`
  is the separate silent mode: no descendant is contained. Dropping the owned child still terminates
  the helper's tree.
- `AdoptedIdentity::start_time()` returns the process start as wall-clock time (exact on macOS and
  Windows; boot time plus ticks on Linux). `AdoptedIdentity::from_recorded_unix_ms(pid, ms,
  tolerance)` verifies a pid recorded earlier as epoch milliseconds and returns the identity only
  when the start is within `tolerance`.
- `AdoptedIdentity::suspend()` (`SIGSTOP`) and `resume()` (`SIGCONT`), plus `suspend_session` and
  `resume_session`, are additive methods (Unix; Windows returns `Unsupported`); `AdoptedSignal` is unchanged. `suspend_current_process()` raises `SIGTSTP` on Unix.
- `signal_session(sid, signal)` (Unix) signals every process group whose session id is `sid`,
  except this process's own group. `tokio_support::terminate_session(sid, options)` does the same
  with a graceful grace and then a forced stage. Both find groups by one process-table scan, so
  they are best effort for groups that start after the scan.
- `HeartbeatWatch::spawn(name, HeartbeatConfig { path, poll, threshold }, on_event)` watches the
  modification time of a file another process rewrites. It reports `HeartbeatEvent::Stalled` once
  per episode and `Recovered` when the file is fresh again. A missing file ages from its last
  seen write, or from the watch start.

## Known limits

Open in this release; none is new in the sense of being introduced by it, and each is stated so a
caller can decide.

- `StderrTail::finish_within` returns at its timeout and never joins the drain thread, so the thread may still be
  running the caller's `map`/`on_line` destructors (and their thread-local destructors) after it returns. `finish`
  joins and has no bound: a callback or destructor that blocks makes it wait.

- **Unix: proof and signal are two steps.** The identity proof above runs before every signal,
  wait and probe, but the proof and the call it guards are separate system calls. Foreign code in
  the same process that reaps one of our children in exactly that gap, followed by a replacement
  process that takes the same pid, defeats it for that one call. The identity is also read just
  after the spawn, not by it. No user-space API available here closes this (there is no `kill` or
  `waitpid` conditional on a start time on macOS). An application that embeds a foreign reaper
  (a `waitpid(-1)` loop) needs that reaper isolated from the children this crate owns; another
  check in this crate cannot close it.
- **Windows: inherited handles.** `inherit_handles_only` raises the inherit flag on the caller's
  handles for the length of `CreateProcessW`. This crate's own ordinary spawns take the same lock,
  so they cannot run in that window, but a process created by other code (a plain
  `std::process::Command::spawn`, another library) can, and inherits those handles.
  `with_windows_spawn_lock` lets such code join the lock; nothing enforces it, and the lock is not
  re-entrant (do not create a RightKit process inside it). Not addressed further here and not
  verified on Windows.
- **macOS: descriptor bound.** `inherit_handles_only` closes descriptors up to a bound taken
  before the fork (as in 0.3.3). A descriptor opened above it, or a soft limit raised, between that
  snapshot and the fork is not closed.
- **Hosts that will not describe a child** (Linux without a readable `/proc`, macOS where
  `proc_pidinfo` is refused) cannot prove a pid later, so the spawn is stopped and fails. Other Unix
  hosts have no identity source and keep the unreaped-child rule alone.

## Example and usage

Illustrative; not compiled as a doctest. The compiled checks are `tests/api.rs`, `tests/supervision_primitives.rs`, and `tests/restart_health.rs`.

`OwnedCommand::inherit_handles_only` restricts inheritance to caller handles/fds plus stdio; Windows requires `windows_spawn_config` for opaque stdio, environment-inheritance & raw-argument settings.

```rust,ignore
use std::{process::Stdio, time::Duration};
use rightkit_child_process::{OwnedCommand, WaitOutcome};

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
rightkit cargo test -p rightkit-child-process --all-targets
rightkit cargo fmt --check
rightkit cargo clippy -p rightkit-child-process --all-targets -- -D warnings
rightkit cargo package -p rightkit-child-process --allow-dirty --no-verify --list
```

`tests/windows_process_tree.rs` covers parent and grandchild termination, sibling
survival, bounded grace, kill-on-drop, and partial-spawn cleanup on Windows.
`tests/unix_process_group.rs` covers Unix process-group behavior. Run both suites on
their own host platforms.

## Windows

- Spawn: `CreateProcessW` with `CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT`, plus `CREATE_NO_WINDOW` (the default) or `CREATE_BREAKAWAY_FROM_JOB` (detached). Inherited handles are limited by a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` built from the caller's handles and stdio.
- Ownership order: a caller job from `windows_job()` is assigned first, then a new job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The primary thread is resumed through the thread handle `CreateProcessW` returned (`src/spawn_windows.rs:147-150`). Resume by process ID from a Toolhelp thread snapshot (`src/process_windows.rs:132`) runs only when that handle is no longer held and the child is not detached. Either way the child is resumed after its job assignment.
- `job_assignment_mode(BestEffort)` records caller-job and owned-job errors in `job_assignment_failures()`. Strict, the default, fails the spawn.
- Termination: `terminate_tree` calls `TerminateJobObject` on the owned job, then `TerminateProcess` on the direct child. An ACCESS_DENIED from a child that is already exiting gets a 2 s wait. Processes outside the job are untouched.
- Detached spawns (`spawn_uncontained_detached`) need the parent job to allow breakaway. The spawn fails without fallback when it does not, and a detached spawn cannot take a caller job.
- Errors: a `HRESULT_FROM_WIN32` value (0x8007xxxx) is returned as the raw OS error. Other failures are `io::Error::other`.
- Dependency: `windows` 0.62 under `cfg(windows)` (`Cargo.toml:18`), with features `Win32_Foundation`, `Win32_Security`, `Win32_System_Diagnostics_ToolHelp`, `Win32_System_JobObjects`, `Win32_System_ProcessStatus` (resource usage) and `Win32_System_Threading`. Consumers enable no Windows features.
- The code has no session or desktop check. The Windows suites (`native_capture`, `output_capture`, `windows_process_tree`, `handle_inheritance`, `drop_bounded`, `parent_death`) passed on Windows 11 (the Dell, build 26300) on 2026-10-20, parallel and serial: 185 passed, 0 failed, 1 ignored helper.

Licensed under either MIT or Apache-2.0, at your option.
