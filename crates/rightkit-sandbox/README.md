# rightkit-sandbox

Shared sandbox, PTY, process-tree, and resource authority for Right Suite applications. It provides sandbox policies, PTY lease fencing, bounded process-tree handling, and reserve-before-spawn resource control.

Confinement backends: macOS Seatbelt, Linux Landlock/seccomp, and Windows AppContainer (ported from CodeRight). On Windows the policy's paths are ACLed to a per-grant-set container SID only while a child that needs them is alive, network is the `internetClient` capability (the crate sets no loopback exemption), and every child is created suspended inside a kill-on-close Job Object. Sandboxed PTYs remain macOS-only.

Publish through the RightKit release workflow.

## Windows

- Confinement: AppContainer. The profile is created with `CreateAppContainerProfile` under the name `rightkit.sandbox.<fnv1a hash of the grant set>`. The child is started with `CreateProcessW`, a `SECURITY_CAPABILITIES` attribute and `CREATE_SUSPENDED | CREATE_NO_WINDOW`. Only stdin, stdout and stderr are inherited.
- Job: each child is assigned to a `WindowsJob` (kill-on-close, `ActiveProcessLimit` 64) while still suspended, then resumed. If assignment fails, the process is killed.
- Filesystem: granted paths are ACLed to the container SID while a child that needs them is alive. Read grants get read and execute. Write grants add write and delete. Directory grants inherit to children. Each physical ancestor up to the drive root gets a non-inheriting `(RD,RA,S)` ACE (mask `0x00100081`). Leases are reference-counted and removed when the last child using them ends. Filesystem roots are refused.
- Admin-owned ancestors: setup fails with `SandboxError::AncestorAccess` unless `S-1-15-2-1` already has `(RD,RA,S)` on them. RightKit's broker bootstrap provides this for `D:\.rightkit-managed`.
- Network: `NetworkPolicy::Allow` adds the `internetClient` capability (`S-1-15-3-1`). Without it the child has no network egress. The crate sets no loopback exemption.
- Denied by default: AppContainer blocks file and registry access outside the granted paths, as the module header states. A `cwd` outside every grant is refused.
- `rightkit-sandbox-exec` (`src/bin/rightkit-sandbox-exec.rs`, Windows only) takes `--read-only`, `--writable`, `--cwd` and `--env`, then `-- <command>`. It relays stdout and stderr with a 64 MiB cap per stream and propagates the exit code. Setup failures exit 125.
- PTYs: `spawn_pty` with a policy returns `SandboxError::Unsupported` on Windows. Without a policy the PTY is unconfined.
- Dependency: `windows` 0.61 under `cfg(windows)`, with features `Win32_Foundation`, `Win32_System_Threading`, `Win32_System_JobObjects` and `Win32_Security`. Most AppContainer calls are declared directly in `windows_appcontainer.rs` (kernel32, userenv, ole32, advapi32). Consumers enable no Windows features.
- Nothing in this section was run on Windows for this README.

## Captured output (0.3.0)

- `SandboxedChild::wait_with_output(timeout)` caps each stream at `DEFAULT_OUTPUT_CAP` (16 MiB);
  `wait_with_output_capped(timeout, cap)` sets it. `SandboxOutput::truncated` reports discarded
  bytes or a stream that did not reach end-of-pipe.
- The timeout covers the whole process tree and its pipes. A descendant that keeps stdout or
  stderr open past the deadline gets the tree killed, and the call returns `timed_out`.

## Also (0.2.0)

Windows launches resolve granted workspace paths with `GetFinalPathNameByHandleW`
before leasing every physical ancestor through drive root. Each added AppContainer
ACE allows only `FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE`
(`RD,RA,S`) on that folder, without inheritance. Existing effective grants,
including ALL APPLICATION PACKAGES (`S-1-15-2-1`), are skipped. Reference-counted
RAII guards remove exactly their added ACEs when final child lease drops.

Setup fails visibly with `SandboxError::AncestorAccess { path, win32_error }`
when required ancestor access cannot be established. Admin-owned roots need a
one-time `S-1-15-2-1` `(RD,RA,S)` **this-folder-only** grant. RightKit's broker
bootstrap supplies this for `D:\.rightkit-managed` & its tmp.

Windows-only `sweep_stale_appcontainer_ancestor_aces(path: &std::path::Path) ->
Result<usize, SandboxError>` removes stale explicit allow ACEs on physical
ancestors, preserving live in-process leases. Acquisition runs this sweep too.
Ownership is verified from registered `rightkit.sandbox.` profile names by
deriving each SID & comparing it with its profile mapping; unrelated
`S-1-15-2-*` SIDs are untouched. Returned count measures removed ACEs.
