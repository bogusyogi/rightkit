# Changelog

## 0.1.0 (unpublished)

rightkit#557. Source only: not compiled or run here; the root integration owns the check batch. Needs `rightkit-child-process` 0.5.9 (`OwnedCommand::unix_controlling_terminal`, `pre_exec_last`, `windows_pseudoconsole`), so stage both together.

- First release: `PtyCommand` and `PtyChild` over a POSIX PTY (macOS, Linux) and ConPTY (Windows), with an owned process tree (session on Unix, Job Object on Windows), `take_reader` / `take_writer` as tokio `AsyncRead` / `AsyncWrite`, input bounded to 8 KiB chunks with `write_input`, `send_eof` (the terminal's `VEOF` on Unix, `0x1a` and a carriage return on Windows), `resize` with pixel size on Unix, `wait`, and a staged `cancel` of the whole session.
- Sandbox hooks: `PtyCommand::from_argv` (pre-wrapped argv), `pre_exec_last` (Unix, `unsafe`), `owned_mut`. `rightkit-sandbox` `PtySession` can be rebuilt on top of this crate; that change belongs to the sandbox owner (it still uses `portable-pty`).
- Device run needed on Windows: spawn `cmd.exe` through `PtyCommand`, read the prompt, `write_input("echo ok\r\n")`, `resize`, `send_eof`, `wait`, then spawn a child with a grandchild and `cancel`; check the output pipe ends after the child exits and the job is empty.
