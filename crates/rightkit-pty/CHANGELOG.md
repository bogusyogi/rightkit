# Changelog

## 0.1.3

- Fixed (macOS): `cancel` no longer waits for as long as a reader is stalled. macOS completes a process's exit only once its terminal's pending output is drained, so a killed child whose output nobody was reading stayed unreapable until the streams were dropped. After `options.grace` plus 250 ms, `cancel` now discards the unread output until the leader is reaped. A reader that keeps reading loses nothing; a stalled one loses the unread tail of a child that was being killed.

## 0.1.2

- Windows: new `ConPty` exposes the pseudoconsole and its bridged streams so rightkit-sandbox can start an AppContainer child on it; `PtyCommand` behaviour is unchanged.

## 0.1.1

- Manifest only: dependency requirements and workspace metadata changed since 0.1.0 was published; no source change.

## 0.1.0 (unpublished)

rightkit#557. Source only: not compiled or run here; the root integration owns the check batch. Needs `rightkit-child-process` 0.5.9 (`OwnedCommand::unix_controlling_terminal`, `pre_exec_last`, `windows_pseudoconsole`), so stage both together.

- First release: `PtyCommand` and `PtyChild` over a POSIX PTY (macOS, Linux) and ConPTY (Windows), with an owned process tree (session on Unix, Job Object on Windows), `take_reader` / `take_writer` as tokio `AsyncRead` / `AsyncWrite`, input bounded to 8 KiB chunks with `write_input`, `send_eof` (the terminal's `VEOF` on Unix, `0x1a` and a carriage return on Windows), `resize` with pixel size on Unix, `wait`, and a staged `cancel` of the whole session.
- Sandbox hooks: `PtyCommand::from_argv` (pre-wrapped argv), `pre_exec_last` (Unix, `unsafe`), `owned_mut`. `rightkit-sandbox` `PtySession` can be rebuilt on top of this crate; that change belongs to the sandbox owner (it still uses `portable-pty`).
- Device run needed on Windows: spawn `cmd.exe` through `PtyCommand`, read the prompt, `write_input("echo ok\r\n")`, `resize`, `send_eof`, `wait`, then spawn a child with a grandchild and `cancel`; check the output pipe ends after the child exits and the job is empty.
