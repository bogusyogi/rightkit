# rightkit-process

Ownership-safe process lifecycle, restart, and health primitives for Right
Suite desktop apps. It contains no app protocol, probe implementation, or
business message.

## Ownership boundary

- `OwnedCommand` is the only way to create an `OwnedChild`. Ordinary spawns own
  their tree; explicitly detached spawns leave lifetime with the caller.
- `AdoptedProcess` can only report an existing PID and whether it is running.
  It deliberately has no terminate method. There is no public `kill_pid` API.
- Dropping an ordinary `OwnedChild` terminates and reaps its tree. A failure
  after OS spawn also cleans up the partial child.

Windows children are created suspended, assigned to a Job Object, then
resumed. `windows_hide()` preserves `CREATE_NO_WINDOW` through that sequence.
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

## Use

`OwnedCommand::inherit_handles_only` restricts inheritance to caller handles/fds plus stdio; Windows requires `windows_spawn_config` for opaque stdio, environment-inheritance & raw-argument settings.

```rust,no_run
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

```powershell
cargo test -p rightkit-process --all-targets
cargo fmt --check
cargo clippy -p rightkit-process --all-targets -- -D warnings
cargo package -p rightkit-process --allow-dirty --no-verify --list
cargo package -p rightkit-process --allow-dirty
```

Windows integration tests prove parent plus grandchild termination, sibling
survival, bounded grace, kill-on-drop, and partial-spawn cleanup. Run the same
suite on macOS before publication to validate Unix process-group behavior.

Licensed under either MIT or Apache-2.0, at your option.
