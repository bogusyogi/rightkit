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

- Unix: children lead a new process group (or a session with `UnixContainment::Session`). Termination signals the group.
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
`wait_or_kill` sends `SIGTERM`, waits for the bounded grace period, then sends
`SIGKILL` to the group and reaps it.

Native std children retain their group/job identity independently of direct-child
exit. Windows resumes the suspended primary thread only after assignment decisions.

## Also (0.3.0)

- `OwnedChild::child()` borrows std `Child` on Unix or `WindowsChild` on Windows through a lock guard, including
  stdin/stdout/stderr and wait/try_wait; existing stream-taking APIs remain.
- `unix_containment(UnixContainment::Session)` runs `setsid`; Windows
  `job_assignment_mode(JobAssignmentMode::BestEffort)` retains non-fatal errors
  in `job_assignment_failures()`. Strict assignment remains default.
- `terminate_tree_with_options(TerminationOptions)` sends Unix TERM immediately,
  then KILL after configurable tree-wide `grace` (zero means immediate escalation).
  Retained group/job also works after parent exit; `windows_exit_code` accepts 1067.
  `terminate_tree_bounded_with_options(timeout, options)` bounds grace plus reap.
- `spawn_uncontained_detached()` skips owner containment and survives owner drop.
  Windows requires parent-job breakaway permission and fails without fallback.
  Unix starts a separate session and reaps asynchronously; configure stdio so EOF
  or closed pipes do not request application shutdown.
- `process_handle()` returns an independent `ProcessHandle` exit reader/waiter.
  Windows implements `AsHandle` with an owned duplicate and exposes FILETIME
  `creation_time_ticks()`; `creation_time()` reports native time on Windows/macOS,
  spawn observation time on other Unix hosts. Unix readers share std's cached reap
  state. Release `child()` guards before calling another accessor or reader.
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
  - macOS: a helper forked before exec joins the child's process group and waits on `EVFILT_PROC`/`NOTE_EXIT`
    for the supervisor. When the supervisor dies, the helper sends `SIGTERM` to the group, waits 2 s, then
    sends `SIGKILL` to the group. It ignores `SIGTERM` itself, closes inherited descriptors (so pipe readers
    get EOF as usual), and exits when the engine exits while the supervisor is alive. Cost: one extra process
    per opted-in child while the child runs. The helper keeps the group id reserved, so the group cannot be
    reused under the signal.
  - Other Unix hosts: the spawn fails with `Unsupported`.
  - Windows: a no-op. Kill-on-close job objects already terminate the tree when the owner handle closes.
  - Combining the option with `spawn_uncontained_detached` returns `InvalidInput`.
- `tests/parent_death.rs` covers the Linux and macOS behavior by re-executing the test binary as the parent,
  SIGKILLing it, and checking the child pid. The group-wide case runs on macOS only.

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

`RestartTracker` is a pure rolling-window/backoff state machine. Restart
permits are single-use scheduling authorities: issuing a replacement permit,
starting a new generation, or shutting down invalidates every older permit.
`RestartMode` is `Automatic`, `OnDemand`, or `Disabled`.

`HealthTracker` is also pure: callers provide monotonic `Duration` timestamps
and probe outcomes, making startup timeout and consecutive-failure thresholds
deterministic in tests.

## Verification

Run through the RightKit wrapper, per the workspace's `docs/rules/rightkit.md`:

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
