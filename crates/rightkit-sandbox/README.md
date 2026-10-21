# rightkit-sandbox

Shared sandbox, PTY, process-tree, and resource-authority primitives for Right Suite applications. It owns:

- `Sandbox::spawn` confines a child to a `SandboxPolicy`: macOS Seatbelt, Linux Landlock and seccomp, or Windows AppContainer inside a kill-on-close Job Object (ported from CodeRight).
- `Sandbox::spawn_pty` gives a PTY child with lease-epoch input fencing (`PtySession`).
- `process_tree` enumerates and kills whole process trees, with bounds.
- `ResourceAuthority` reserves capacity before any process exists; `Strict` denies.
- `classify_denial` turns raw error text into a cross-platform `DenialSignature`; `report_denial` builds a `DenialReport` (denied path, protected-secret flag, the `Grant` to request) from it plus the command text.
- `confine_command` confines a command the caller owns (Linux Landlock/seccomp, macOS Seatbelt).
- `SandboxPolicy::deny_read` and the secret planner keep credentials unreadable inside a wide grant.
- `Sandbox::spawn_piped` is `spawn` with a stdin pipe; `Sandbox::pin_read_roots` shares interpreter install directories.

**Use it** when an app must run untrusted or semi-trusted commands with filesystem and network limits, needs a PTY whose input cannot be replayed after a lease change, or must bound how many child processes it starts.

**Do not use it** as a general container or VM. It confines a process; it does not isolate the kernel. Sandboxed PTYs are macOS-only (see Platform support).

## Features

`pty` (default): `Sandbox::spawn_pty`, `PtySession`, `PtySize`, `PtyError`, and the lease-epoch input fence, over `portable-pty`. With `default-features = false` those items and `portable-pty` are gone. Other dependencies: `rightkit-child-process`, `libc` (Unix), and `windows` (Windows).

## Platform support

| Capability | macOS | Linux | Windows |
|---|---|---|---|
| `Sandbox::spawn` (confined child) | Seatbelt | Landlock + seccomp | AppContainer in a kill-on-close Job Object |
| `Sandbox::spawn_pty` with a policy | Yes | Returns `PtyError::Sandbox(SandboxError::Unsupported)` | Returns `PtyError::Sandbox(SandboxError::Unsupported)` |
| `Sandbox::spawn_pty` without a policy | Yes | Yes | Yes |
| Windows ancestor-ACE lease and sweep | No | No | Yes (`windows_ancestors.rs`, `windows_appcontainer.rs`) |
| `confine_command` (caller-owned command) | Seatbelt | Landlock + seccomp | `Unsupported` (an AppContainer is set at process creation) |
| `ConfinementMode::WriteOnly` | Yes | Refused | Refused |
| `SandboxPolicy::deny_read` | `(deny file-read* file-write*)` after the allows | **Not enforced** (`deny_read_enforced()` is `false`) | **Refused** (`Unsupported`), not supported yet, [#887](https://github.com/bogusyogi/rightkit/issues/887) |
| `SandboxPolicy::exec_any` | `(allow process*)` | Landlock `EXECUTE` on `/` | No effect |
| `Sandbox::spawn_piped` | Yes | Yes | Yes (ACL-bound named pipe) |
| Loopback exemption, `AppContainerProfile`, `resolve_program` | No | No | Yes |

On Windows, the policy's paths are ACLed to a per-grant-set container SID only while a child that needs them is alive. Network is the `internetClient` capability; loopback stays blocked even then unless the policy asks for `LoopbackPolicy::Exempt`, which adds the container to the machine's loopback-exempt list for as long as a child holds it and needs an elevated process (see the Windows section). Every child is created suspended inside a kill-on-close Job Object.

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

## Linux (`confine_command`, Landlock, seccomp)

`confine_command(&mut Command, &SandboxPolicy)` registers the confinement in `pre_exec` and leaves cwd, environment, stdio and process group to the caller (the policy's `env` is not applied; use `cmd.as_std_mut()` for a tokio command). It fails closed: an invalid policy, an unavailable Landlock or a refused mode is an error and the command must not be spawned.

- Landlock ABI is probed (`landlock_abi_version()`, `is_landlock_available()`). ABI 2 adds `REFER` and ABI 3 adds `TRUNCATE`; both are handled when present and granted only on write roots, so a cross-directory rename or a truncate outside the grants is denied. On an older kernel those operations keep that ABI's behaviour. Read and exec grants on a single file carry file rights only.
- `SandboxPolicy::landlock(LandlockMode::SkipWhenNoPaths)` skips Landlock when no path is listed (a network-only tier). It is opt-in; the default always applies it.
- seccomp applies when the network is denied. `SeccompMode::Allowlist` (default) is the default-deny allowlist (x86_64 only). `SeccompMode::NetworkOnly` is default-allow and denies `socket`/`socketpair` for every domain except `AF_UNIX` and `io_uring_*` (x86_64 and aarch64). Both start with an architecture check: a foreign architecture is killed, and on x86_64 the x32 range is denied. Both are checked by a BPF interpreter in the unit tests, on any host.
- `deny_read` is not enforced (Landlock only adds allowances): grant narrower roots when `deny_read_enforced()` is `false`.
- Nothing in this section was run on Linux.

## macOS (`WriteOnly`, `exec_any`, `from_profile`)

`ConfinementMode::WriteOnly` builds `seatbelt::write_only_profile`: default-allow, `(deny file-write*)`, then writes allowed under `write_paths` (realpath-resolved), `/private/tmp`, `/private/var/folders`, `/private/var/tmp`, `/tmp` and a few `/dev` nodes; with the network denied, outbound is denied and unix sockets stay allowed. `exec_any` emits `(allow process*)` and grants execution only: the image still needs a read grant. `seatbelt::Prepared::from_profile(String)` applies a profile you built, as is. `try_profile_for` and `try_wrap_argv` return the error when a profile cannot be built; `profile_for` and `wrap_argv` keep their signatures and, if a `deny_read` entry cannot be planned, return a profile that denies everything. Nothing in this section was run on a Mac; `exec_any` against real vendor drivers is unverified.

## Secrets and denial reports

`SandboxPolicy::deny_read(path)` and `deny_read_glob(pattern)` deny reads and writes of a path or absolute glob inside a granted root (macOS; Linux omits them; Windows refuses them for now). `secret_paths_within(root)` lists the protected folders (`~/.ssh`, `.aws`, `.gnupg`, `.kube`, `.docker`, `.config`, `.npm`, ...) beneath a root, `plan_deny_read_paths` plans a deny set (lexical and canonical spellings; relative paths, `..` and filesystem roots, including an alias that resolves to one, are refused), and `path_is_protected_secret(path)` answers for one path, resolving `..` and symlinks. `report_denial(output, command, exit_ok, &ConfinementInfo)` returns a `DenialReport`; its `requested_grant` is a request for a person or an admission authority, never an approval: output is untrusted, so a protected secret, a filesystem root or a `..` path never gets one.

## Pinned read roots

`Sandbox::pin_read_roots(&[PathBuf]) -> PinHandle` shares interpreter install directories as read and execute roots until the last handle on each root drops (`PinHandle::release_all()` at shutdown). A root must be an existing directory holding a known interpreter (`is_interpreter_dir`) and must not be too broad (`read_root_is_too_broad`: a filesystem root, the user profile, a user data folder or a folder containing one, a protected secret). On Windows it is an ALL APPLICATION PACKAGES read/execute ACE, readable by every AppContainer app on the machine, which is why the vetting is strict; elsewhere the roots join each confined spawn's read and execute roots.

## Also (release not recorded in CHANGELOG.md)

Windows launches resolve granted workspace paths with `GetFinalPathNameByHandleW`
before leasing every physical ancestor through drive root. Each added AppContainer
ACE allows only `FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE`
(`RD,RA,S`) on that folder, without inheritance. Existing effective grants,
including ALL APPLICATION PACKAGES (`S-1-15-2-1`), are skipped. Reference-counted
RAII guards remove exactly their added ACEs when final child lease drops.

Setup fails visibly with `SandboxError::AncestorAccess { path, win32_error }`
when required ancestor access cannot be established. Admin-owned roots need a
one-time `S-1-15-2-1` `(RD,RA,S)` **this-folder-only** grant. The build broker
bootstrap, which provides this grant for its managed root, is outside this repository (RustRight, see INDEX.md) and is not verified here.

`spawn_with_pipe_pair` and `AppContainerProfile` lease ancestors best effort: an ancestor whose DACL write is
refused (a standard user on `C:\Users`) is skipped only when the readable DACL confirms the container lacks the
access, and the token's bypass-traverse privilege reaches the grants below it. The deepest skipped ancestor is
exported as `GIT_CEILING_DIRECTORIES` unless the caller sets it. Grants and the image lease
still fail the launch; one-shot `spawn` stays strict.

Windows-only `sweep_stale_appcontainer_ancestor_aces(path: &std::path::Path) ->
Result<usize, SandboxError>` removes stale explicit allow ACEs on physical
ancestors, preserving live in-process leases. Acquisition runs this sweep too.
Ownership is verified from registered `rightkit.sandbox.` profile names by
deriving each SID & comparing it with its profile mapping; unrelated
`S-1-15-2-*` SIDs are untouched. Returned count measures removed ACEs.

## Windows

- Profile clean-up: a spawn creates or adopts one profile per distinct grant set and never deletes it, so the next spawn with the same grants reuses it. **Until an application calls `remove_unused_appcontainer_profiles()`, profiles accumulate for the user, one per grant set ever used** (a registry mapping and a folder under the local application data). The call deletes this user's `rightkit.sandbox.*` profiles that no process holds and returns a `ProfileSweepReport` (`removed`, and `kept` with `ProfileKept::InUse`, `Busy`, `CouldNotProveUnused` or `DeleteFailed`). Every spawn marks its profile in use before creating or adopting it, for as long as the child's leases live; the mark is a named kernel object in the machine-wide namespace, named with the user's SID, so every logon session of that user sees it, and it disappears when the holding process exits. A profile whose lock or mark cannot be created or read is kept, never deleted. The check and the deletion run under a per-profile lock that spawns take too, so the call is safe at application start and while other RightKit processes run. A profile whose name this crate did not make is never touched. Order of clean-up: run `sweep_stale_appcontainer_ancestor_aces` for the paths you grant first, then this call; a deleted profile's SID is no longer recognised as this crate's, so entries a crashed process left for it would stay (the sweep run afterwards is harmless, it just cannot clean those).
- Confinement: AppContainer. The profile is created with `CreateAppContainerProfile` under the name `rightkit.sandbox.<fnv1a hash of the grant set>`. The child is started with `CreateProcessW`, a `SECURITY_CAPABILITIES` attribute and `CREATE_SUSPENDED | CREATE_NO_WINDOW`. Only stdin, stdout and stderr are inherited.
- Job: each child is assigned to a `WindowsJob` (kill-on-close, `ActiveProcessLimit` 64) while still suspended, then resumed. If assignment fails, the process is killed.
- Filesystem: granted paths are ACLed to the container SID while a child that needs them is alive. Read grants get read and execute. Write grants add write and delete. Directory grants inherit to children. Each physical ancestor up to the drive root gets a non-inheriting `(RD,RA,S)` ACE (mask `0x00100081`). Leases are reference-counted and removed when the last child using them ends. Filesystem roots are refused.
- Admin-owned ancestors: setup fails with `SandboxError::AncestorAccess` unless `S-1-15-2-1` already has `(RD,RA,S)` on them. The broker bootstrap that provides this for `D:\.rightkit-managed` is outside this repository and is not verified here.
- Network: `NetworkPolicy::Allow` adds the `internetClient` capability (`S-1-15-3-1`). Without it the child has no network egress. Loopback stays blocked unless the policy sets `LoopbackPolicy::Exempt` (or `AppContainerProfile::with_loopback`): the container is then added to the machine's loopback-exempt list (`NetworkIsolationSetAppContainerConfig`, resolved from `FirewallAPI.dll` at run time) with a process-wide reference count and removed with the last holder. Writing the list needs an elevated process; without one the container stays loopback-blocked (never wider) and `SandboxedChild::loopback_exempt()` is `false`. A crashed holder leaves the exemption in place until the next holder for that container ends.
- Deny: **`deny_read` is not supported on Windows yet ([#887](https://github.com/bogusyogi/rightkit/issues/887)); this release fails closed instead of silently leaking.** A lowbox token is checked in a second pass that honours only allow entries for its container SID, its capability SIDs and ALL APPLICATION PACKAGES, so the DENY ACE earlier versions wrote was inert and confined code could read the denied path. No DENY ACE is written now. A policy with ANY `deny_read` or `deny_read_glob` entry (a path, a glob, a missing path, a path outside every grant) is refused with `SandboxError::Unsupported` as the first step of `Sandbox::spawn`, `spawn_piped`, `spawn_with_pipe_pair(_stdio_file)` and `AppContainerProfile::for_policy`: before policy validation, the resource reservation, the executable lookup, and any profile, lease or ACE; the glob is never expanded. `AppContainerProfile::for_workspace` therefore refuses whenever its policy has denies (a workspace that contains a protected secret folder, a user-profile root for one); that is correct fail-closed behaviour until the full design lands, and callers should check `deny_read_enforced()` (`false` on Windows) first and grant a narrower root instead. `AppContainerProfile::with_grant` refuses a path that has a protected secret inside it the same way, and returns the original profile, with its leases untouched, inside `WithGrantError` on any failure. A denied path outside every grant is not guaranteed unreadable either: ALL APPLICATION PACKAGES and capability permissions on it still apply. **Legacy DENY ACEs:** versions before 0.5.2 wrote DENY ACEs for the container SID on denied paths and revoked them with the child; a crashed run may have left one behind. They only ever narrowed that container's access and are inert for a lowbox token; this release neither writes nor sweeps them (a bounded, ownership-verified cleanup is part of [#887](https://github.com/bogusyogi/rightkit/issues/887)). Remove one with `icacls <path> /remove:d *<container SID>` if it matters.
- Stdin: `Sandbox::spawn_piped` gives the child an ACL-bound named pipe (DACL: the user and the container) for stdin; dropping the write end from `take_stdin()` is end-of-input. The tokio wrappers of issue #562 are not implemented (they need an optional `tokio` dependency).
- Reusable container: `AppContainerProfile::for_workspace(root)` (or `for_policy`) holds the allow leases, the ancestor leases and the loopback lease for as long as it and its children live; spawn into it with `Sandbox::spawn_in_profile`. Its ancestor leases are best effort: an ancestor the user cannot re-ACL is skipped, and the deepest one becomes a `GIT_CEILING_DIRECTORIES` entry in each child's environment unless the caller sets that variable. `lease_grant` refuses protected locations, filesystem roots and any path that resolves into a secret.
- Denied by default: AppContainer blocks file and registry access outside the granted paths, as the module header states. A `cwd` outside every grant is refused.
- `rightkit-sandbox-exec` (`src/bin/rightkit-sandbox-exec.rs`, Windows only) takes `--read-only`, `--writable`, `--cwd` and `--env`, then `-- <command>`. It relays stdout and stderr with a 64 MiB cap per stream and propagates the exit code. Setup failures exit 125.
- PTYs: `spawn_pty` with a policy returns `PtyError::Sandbox(SandboxError::Unsupported)` on Windows. Without a policy the PTY is unconfined.
- Dependency: `windows` 0.62 under `cfg(windows)` (`Cargo.toml:26`), with features `Win32_Foundation`, `Win32_System_Threading`, `Win32_System_JobObjects` and `Win32_Security`. Most AppContainer calls are declared directly in `windows_appcontainer.rs` (kernel32, userenv, ole32, advapi32). Consumers enable no Windows features.
- Nothing in this section was run on Windows for this README.

