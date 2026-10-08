# rightkit-sandbox

Shared sandbox, PTY, process-tree, and resource authority for Right Suite applications. It provides sandbox policies, PTY lease fencing, bounded process-tree handling, and reserve-before-spawn resource control.

Confinement backends: macOS Seatbelt, Linux Landlock/seccomp, and Windows AppContainer (ported from CodeRight). On Windows the policy's paths are ACLed to a per-grant-set container SID only while a child that needs them is alive, network is the `internetClient` capability (loopback stays blocked even then: an AppContainer loopback exemption needs elevation), and every child is created suspended inside a kill-on-close Job Object. Sandboxed PTYs remain macOS-only.

Publish through the RightKit release workflow.

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
