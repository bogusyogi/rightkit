# rightkit-sandbox

Shared sandbox, PTY, process-tree, and resource-authority primitives for Right Suite applications. It owns:

- `Sandbox::spawn` confines a child to a `SandboxPolicy`: macOS Seatbelt, Linux Landlock and seccomp, or Windows AppContainer inside a kill-on-close Job Object (ported from CodeRight).
- `Sandbox::spawn_pty` gives a PTY child with lease-epoch input fencing (`PtySession`).
- `process_tree` enumerates and kills whole process trees, with bounds.
- `ResourceAuthority` reserves capacity before any process exists; `Strict` denies.
- `classify_denial` turns raw error text into a cross-platform `DenialSignature`.

**Use it** when an app must run untrusted or semi-trusted commands with filesystem and network limits, needs a PTY whose input cannot be replayed after a lease change, or must bound how many child processes it starts.

**Do not use it** as a general container or VM. It confines a process; it does not isolate the kernel. Sandboxed PTYs are macOS-only (see Platform support).

## Features

None. The crate has no Cargo features. Dependencies: `rightkit-process`, `portable-pty`, `libc` (Unix), and `windows` (Windows).

## Platform support

| Capability | macOS | Linux | Windows |
|---|---|---|---|
| `Sandbox::spawn` (confined child) | Seatbelt | Landlock + seccomp | AppContainer in a kill-on-close Job Object |
| `Sandbox::spawn_pty` with a policy | Yes | Returns `SandboxError::Unsupported` | Returns `SandboxError::Unsupported` |
| `Sandbox::spawn_pty` without a policy | Yes | Yes | Yes |
| Windows ancestor-ACE lease and sweep | No | No | Yes (`windows_ancestors.rs`, `windows_appcontainer.rs`) |

On Windows, the policy's paths are ACLed to a per-grant-set container SID only while a child that needs them is alive. Network is the `internetClient` capability; loopback stays blocked even then, because an AppContainer loopback exemption needs elevation. Every child is created suspended inside a kill-on-close Job Object.

Published version: see `INDEX.md` at the repository root. Release notes: see `CHANGELOG.md` in this crate.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/e2e.rs`, `tests/sandbox_exec.rs`, and `tests/output_bounds.rs`.

```rust,ignore
use rightkit_sandbox::{Sandbox, SandboxCommand, SandboxPolicy};

let policy: SandboxPolicy = /* build the workspace-scoped policy */ todo!();
let command = SandboxCommand { program: "my-tool".into(), args: vec![], /* ... */ ..Default::default() };
let child = sandbox.spawn(&policy, command)?;
let output = child.wait_with_output(std::time::Duration::from_secs(30))?;
```

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
one-time `S-1-15-2-1` `(RD,RA,S)` **this-folder-only** grant. The build broker
bootstrap supplies this for its managed root. The broker is moving to RustRight; see that product's docs.

Windows-only `sweep_stale_appcontainer_ancestor_aces(path: &std::path::Path) ->
Result<usize, SandboxError>` removes stale explicit allow ACEs on physical
ancestors, preserving live in-process leases. Acquisition runs this sweep too.
Ownership is verified from registered `rightkit.sandbox.` profile names by
deriving each SID & comparing it with its profile mapping; unrelated
`S-1-15-2-*` SIDs are untouched. Returned count measures removed ACEs.
