# rightkit-process

Ownership-safe process lifecycle, restart, and health primitives for Right
Suite desktop apps. It contains no app protocol, probe implementation, or
business message.

## Ownership boundary

- `OwnedCommand` is the only way to create an `OwnedChild`. The crate may
  terminate that child and its descendants because it created and owns them.
- `AdoptedProcess` can only report an existing PID and whether it is running.
  It deliberately has no terminate method. There is no public `kill_pid` API.
- Dropping `OwnedChild` terminates and reaps its tree. A failure after OS spawn
  but before wrapper completion also cleans up the partial child.

Windows children are created suspended, assigned to a Job Object, then
resumed. `windows_hide()` preserves `CREATE_NO_WINDOW` through that sequence.
Explicit termination uses the Job Object, so descendants die while unrelated
sibling processes survive. Unix children lead a new process group;
`wait_or_kill` sends `SIGTERM`, waits for the bounded grace period, then sends
`SIGKILL` to the group and reaps it.

`process-wrap` is pinned exactly to `8.2.1`. Its `tracing` feature is required
because that version's Windows Job Object implementation references the
feature-gated tracing macro even when the caller does not emit tracing events.

## Use

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
