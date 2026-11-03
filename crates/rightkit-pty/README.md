# rightkit-pty

An async managed PTY child for Right Suite apps: a program started on a pseudo-terminal
(POSIX PTY on macOS and Linux, ConPTY on Windows) whose whole process tree is owned.

**Use it** where an app runs an interactive program (a shell, an agent CLI) behind a terminal view
and needs to read its output, send it input, resize it, wait for it and cancel it.

**Do not use it** to run a command and collect its output (use `rightkit-child-process` `run`), or
to confine a process (use `rightkit-sandbox`, which composes with the hooks below).

```rust
let mut command = rightkit_pty::PtyCommand::new("zsh");
command.arg("-l").size(rightkit_pty::PtySize::new(40, 120));
let mut child = command.spawn()?;              // inside a tokio runtime
let mut output = child.take_reader()?.unwrap(); // AsyncRead
let mut input = child.take_writer()?.unwrap();  // AsyncWrite
input.write_input(b"echo hi\n").await?;         // 8 KiB chunks, each accepted before the next
child.resize(rightkit_pty::PtySize::new(50, 132))?;
input.send_eof().await?;                        // Ctrl-D (Unix) / Ctrl-Z Enter (Windows)
let status = child.wait().await?;
```

## What it provides

- `PtyCommand` (`new`, `from_argv`, `arg`, `args`, `env`, `env_remove`, `current_dir`, `size`,
  `owned_mut`, `pre_exec_last`) and `PtyChild` (`take_reader`, `take_writer`, `resize`, `wait`,
  `try_wait`, `cancel`, `id`, `process_handle`, `into_owned`).
- Containment through `rightkit-child-process` `OwnedCommand`: on Unix the child leads a session with
  the pty slave as controlling terminal (so `SIGINT` from the terminal and `/dev/tty` work and group
  signals reach the whole tree); on Windows the child is created suspended, put in a Job Object and
  attached to the pseudoconsole. Dropping a `PtyChild` runs the staged kill.
- `cancel(TerminationOptions)`: `SIGTERM` to the group, grace, `SIGKILL`, then the same for every other
  process group of the session (Unix); job termination (Windows).
- Bounded input: one `poll_write` takes at most 8 KiB (`INPUT_CHUNK`); `write_input` returns after every
  chunk was accepted by the terminal.
- Sandbox hooks: `from_argv` for an already wrapped argv (a Seatbelt or `bwrap` prefix),
  `pre_exec_last` (Unix, `unsafe`) for a filter installed as the last step before `exec`, and
  `owned_mut` for the Job limits and environment allowlist of the underlying `OwnedCommand`.

## Platform notes

- Unix starts no thread: the pty master is registered with the tokio reactor.
- Windows bridges the two blocking ConPTY pipes to tokio with one thread each. The output pipe of a
  ConPTY ends only when the pseudoconsole is closed, which happens when the child has exited (`wait`,
  `try_wait`, `cancel`) or is dropped.
- A Windows AppContainer token is not expressible through this crate; a Windows sandbox starts the
  program it wraps through `from_argv`.
- The Windows half was written without a Windows host. See `CHANGELOG.md` for the device run.

Release notes: `CHANGELOG.md`.
