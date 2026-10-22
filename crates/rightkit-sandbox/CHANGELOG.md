# Changelog

## 0.6.0 (unpublished staging)

Breaking: `AppContainerProfile::with_grant` returns `WithGrantError` (it was `SandboxError`), and Windows refuses every `deny_read` policy. Includes the unpublished 0.5.1 changes below.

- **Windows: `deny_read` now fails closed (issue [#887](https://github.com/bogusyogi/rightkit/issues/887), security).** A lowbox token is checked in a second pass that honours only allow entries for its container SID, its capability SIDs and ALL APPLICATION PACKAGES, so the DENY ACE this crate wrote was inert and confined code could read a denied path inherited from a granted root. Deny-read is **unsupported on Windows**; this release contains the gap by refusal (it does not enforce a deny) instead of silently leaking. A policy with ANY `deny_read` entry (a path, a glob, a missing path, a path outside every grant) is refused with `SandboxError::Unsupported` before any profile, lease or ACE exists, at every entry point: `Sandbox::spawn`, pipe-pair spawns, `AppContainerProfile::for_policy`, `with_grant`, `prepare_container(_from)` and reusable profiles. `AppContainerProfile::for_workspace` refuses whenever its policy has denies; that is correct fail-closed behaviour until the full design lands.
- **Windows: no DENY ACE is ever written.** The deny lease path (`LeaseKind::Deny`, `lease_deny`, `DENY_PERMISSIONS`) is removed. Older versions may have left DENY ACEs on denied paths after a crash; they are inert for a lowbox token and are not swept by this release (documented in the README; cleanup is part of #887).
- **`deny_read_enforced()` is now `true` on macOS only** (it was `true` on Windows). It reports whether a deny is carved out of a grant; on Windows every deny is refused. Documented on the function and in the README; callers should check it before building a policy with denies.
- **Windows: ordering and `with_grant`.** The deny refusal is the first step of `Sandbox::spawn(_piped)`, `spawn_with_pipe_pair(_stdio_file)` and `spawn_inner`: before validation, the resource reservation and the executable lookup (a policy with a deny and a missing executable returns `Unsupported`). `AppContainerProfile::with_grant` now returns `Result<Self, WithGrantError>`: every refusal is checked against `&self` first, and on any failure the original profile comes back in `WithGrantError::profile` with its leases and ACEs untouched (it used to be dropped, revoking them). A path with a protected secret inside it is refused as `Unsupported`.
- **Tests.** `confined_grandchildren_die_with_the_child` no longer depends on PowerShell: a self-hosted blocking helper in the test binary signals readiness through a file, its identity is verified (descendant of the confined child, executable path equal to the test binary, verbatim prefix stripped), it is asserted still running, then confirmed gone after the child is dropped; a negative control proves the harness is not vacuous. The deny e2e tests assert the typed refusal, that the secret is untouched, and that no profile was created, with an unsandboxed read as positive control. Unit tests cover every entry point, the ordering, and a refused `with_grant` keeping the profile and its ACEs. A test-only mutation log (`note_mutation`, compiled out of release builds) records every profile, profile-mutex, DACL, named-pipe and stdio-file creation on the calling thread; `a_deny_is_refused_before_validation_reservation_and_any_mutation` (a Strict authority and invalid commands and policies make any reordering change the error type) and `a_refused_deny_leaves_every_resource_permit_available` (permit counts before and after, and with the pool exhausted) prove the refusal precedes all of them, and `a_rejected_with_grant_changes_no_profile_grant_or_lease` compares name, SID, grants, denies, leases and ACEs before and after a rejected `with_grant`. Scratch roots are unique per run. `a_deny_ace_is_added_and_revoked_on_a_scratch_folder`, `allow_and_deny_leases_never_share_a_registry_key` and `best_effort_still_fails_when_a_deny_carve_out_cannot_be_leased` are removed with the deny lease. Not compiled or run.

## 0.5.1 (unpublished staging)

- **Windows: pipe-pair launches take ancestor traverse leases best effort.** `Sandbox::spawn_with_pipe_pair` and `spawn_with_pipe_pair_stdio_file` (the browser fixture) failed for a standard user with `AncestorAccess` Win32 error 5 on `C:\Users`, which the user cannot re-ACL. They now use `AncestorLeasePolicy::BestEffort`, as `AppContainerProfile` does: an ancestor whose DACL write is denied and that does not already allow the container is skipped (the token reaches the grants through bypass-traverse). The skip happens only when the write is denied (permission denied) AND the readable DACL still lacks the access; an unreadable DACL or any other error still fails the launch. The grants, deny carve-outs and image lease stay strict, and `Sandbox::spawn` stays `Strict`. Tests added; not compiled or run.

- **Windows: deny-read globs keep the verbatim prefix literal.** A deny glob that starts with `\\?\` (the prefix `canonicalize` adds) had its `?` read as a wildcard, so expansion walked the drive root and hit the entry cap. The prefix is now literal and rebuilt with backslash separators (`src/deny_read.rs`).

## 0.5.0 (2026-10-17)

CodeRight port (issues #558, #559, #560, #561, #562, #564, #566). New API and opt-in policy knobs, except the Linux
hardening in the first entry, which applies to every Linux spawn (a behaviour change by design: ABI 2/3
rights on newer kernels and the seccomp architecture check). Nothing here was compiled or run for this entry.

- **Linux: `confine_command(&mut Command, &SandboxPolicy)`** confines a command the caller owns (a
  `tokio::process::Command` through `as_std_mut()`), with `is_landlock_available()` and
  `landlock_abi_version()`. Landlock now probes its ABI: ABI 2 adds `REFER` (renaming or linking across
  directories outside the granted trees is denied) and ABI 3 adds `TRUNCATE`; read and exec roots never carry
  them. A rule on a file (not a directory) drops the directory rights, where it used to fail with `EINVAL`.
  Every seccomp program now starts with an architecture check (`AUDIT_ARCH_X86_64`; a foreign architecture, such
  as i386 `int 0x80`, is killed) and the allowlist denies the x32 alias of every number, closing the compat-syscall
  bypass. New knobs: `SandboxPolicy::seccomp(SeccompMode::NetworkOnly)` is a default-allow filter that denies
  `socket`/`socketpair` for every domain except `AF_UNIX` and `io_uring_*` (x86_64 and aarch64);
  `SandboxPolicy::landlock(LandlockMode::SkipWhenNoPaths)` applies no Landlock ruleset when no path is listed (opt-in,
  the only documented way to run on Linux without one; a listed path turns it back on and Landlock must then be
  available). `exec_any` adds Landlock `EXECUTE` on `/`.
- **macOS: `ConfinementMode::WriteOnly`** (`SandboxPolicy::mode`) is a default-allow Seatbelt profile that denies
  every write except `write_paths`, the temporary locations and a few `/dev` nodes; reads and execution stay open
  (`seatbelt::write_only_profile(write_paths, network)` builds it). `SandboxPolicy::exec_any(true)` emits
  `(allow process*)` for vendor CLIs that exec helpers from arbitrary install roots; it grants execution, not
  reading. `seatbelt::Prepared::from_profile(String)` applies a caller-built profile, so a private `sandbox_init`
  copy can go. `try_profile_for` and `try_wrap_argv` return the error when a profile cannot be built. Linux and
  Windows refuse `WriteOnly` (fail closed). **Not verified on a Mac**: `exec_any` against real vendor drivers.
- **`SandboxPolicy::deny_read` / `deny_read_glob`** keep paths and globs unreadable (and unwritable) inside a granted
  root. macOS: `(deny file-read* file-write*)` after every allow (a glob becomes an anchored regex that also covers
  children). Windows: an explicit DENY ACE for the container, held with the child; globs expand against the
  files that exist at spawn (bounded; a huge tree fails the spawn), a missing path has no object and is skipped,
  and the deny set is part of the profile name. **Linux: not enforced** (Landlock only adds allowances); check
  `deny_read_enforced()`. New planner API: `plan_deny_read_paths` (lexical and canonical spellings; refuses
  relative paths, `..` and filesystem roots, including an alias that resolves to one), `secret_roots`,
  `secret_roots_under`, `secret_paths_within`, `path_is_protected_secret`, `path_is_within` (`..` resolved first),
  `user_profile_root`, `USERPROFILE_SECRET_FOLDERS`, `glob_matches`.
- **`report_denial` and `DenialReport`** (`category`, `denied_path`, `protected_secret`, `requested_grant: Grant`,
  `message`) are built from command output, command text and `ConfinementInfo`: quoted-path and `tool: /path:
  Permission denied` extraction, PowerShell and curl network-block phrasing, a write-versus-read guess for the
  grant. Output is untrusted, so a protected secret, a filesystem root or a `..` path never gets a requested grant.
- **`Sandbox::spawn_piped`** is `spawn` with a stdin pipe (`SandboxedChild::take_stdin`); on Windows the pipe is an
  ACL-bound named pipe, on Unix a plain pipe. **Not done (needs a dependency change, see issue #562):** the
  tokio `AsyncWrite`/`AsyncRead` wrappers (`ConfinedPipedChild`), which need an optional `tokio` dependency with
  features `rt` and `time` and a `tokio` crate feature. Windows `resolve_program(&str)` is exported.
- **Windows: `AppContainerProfile`** (`for_workspace`, `for_policy`, `with_network`, `with_loopback`,
  `lease_grant`/`with_grant`, `name`, `sid_string`, `folder_path`, `git_ceilings`) is a reusable container
  spawned into with `Sandbox::spawn_in_profile` / `spawn_piped_in_profile`. Its allow and deny leases are
  reference counted, shared with every child and revoked with the last holder; `for_workspace` denies the
  protected secret folders inside the workspace; `lease_grant` refuses protected locations, roots and a path that
  resolves into a secret. Its ancestor leases are best effort: an ancestor the user cannot re-ACL is skipped and
  the deepest one is exported as `GIT_CEILING_DIRECTORIES` (unless the caller sets it). `SandboxPolicy::loopback(
  LoopbackPolicy::Exempt)` adds the container to the machine's loopback-exempt list for a network-enabled
  child, reference counted per container and resolved from `FirewallAPI.dll` at run time; it needs an elevated
  process (otherwise the container stays loopback-blocked, never wider; `SandboxedChild::loopback_exempt()`
  reports it) and a crashed holder leaves the exemption until the next holder for that container ends.
  The profile key now includes deny entries and the loopback choice; with neither, names are unchanged.
- **`Sandbox::pin_read_roots(&[PathBuf]) -> PinHandle`**, `PinHandle::release_all()`, `read_root_is_too_broad`,
  `is_interpreter_dir`, `default_user_dirs`: interpreter install directories shared as read and execute roots with
  a user count per root. A root must be an existing directory that holds a known interpreter (python, node,
  ruby, ...) and must not be, or contain, a filesystem root, the user profile, a user data folder or a protected
  secret. Windows: an ALL APPLICATION PACKAGES read/execute ACE (so readable by every AppContainer app on the
  machine), added once and removed with the last handle unless the DACL already allowed it. Elsewhere the roots
  join the read and execute roots of each confined spawn.


## 0.4.0

- Republished on the renamed dependencies: `rightkit-child-process` (was `rightkit-process`). The 0.3.5 on crates.io was packaged before the renames and still named the old crates.
- **Breaking (0.x minor):** `SandboxedChild::terminate_tree_bounded` returns `rightkit_child_process::ReapOutcome`.
## 0.3.5

- **Windows: the ancestor-ACE sweep sees leases held in another logon session.** The lease mutex was in the
  session-local object namespace, so a sweep run in a second logon session of the same user (fast user switching,
  Remote Desktop) found no mutex for a lease held in the first, called its ancestor entry stale and removed it while
  the confined child ran; the child then lost traverse access to those folders. Access was never widened. The mutex
  is now `Global\rightkit-ancestor-lease-<hash>`, with no user in the name: the AppContainer SID follows from the
  grant set, so two users with the same grants share one entry on a shared folder and must see each other's lease.
  Every authenticated user may open it for `SYNCHRONIZE`. An entry whose lease cannot be read is kept and logged,
  where the sweep used to fail. The old name `Local\rightkit-ancestor-lease-<hash>` is kept for compatibility: a
  lease is created under both names and the sweep counts either as live, so a process on 0.3.3 or earlier and a
  process on this version see each other inside one logon session (across sessions an old process is still not
  seen). The old name can go once no supported application ships rightkit-sandbox 0.3.3 or earlier. Limit: a local
  user who creates the global name first with a closed security descriptor makes spawns that need that lease fail;
  nothing is removed in that case. Not run on a machine yet.
- **Windows: `remove_unused_appcontainer_profiles()` removes AppContainer profiles nothing is using.** A spawn creates or
  adopts one `rightkit.sandbox.<hash>` profile per distinct grant set and nothing deleted them, so they accumulated for
  the user. The new call deletes those no process holds and reports what it removed and what it kept and why
  (`ProfileSweepReport`, `ProfileKept`). To make "in use" provable, every spawn now marks its profile in use (a named
  kernel object held with the child's leases) inside a per-profile lock, before it creates or adopts the profile; the
  call takes the same lock for its check and deletion. A foreign-named profile is never touched. **Until an application
  calls it, profiles still accumulate.** Behaviour change for spawns: one named mutex is taken briefly per spawn, and a
  spawn fails with a timeout if another process holds a profile's lock for 30 s, or with an error if the lock or mark
  cannot be created. The lock and mark are in the machine-wide object namespace and carry the user's SID, so another
  logon session of the same user is seen; a profile whose lock or mark cannot be created or read is kept
  (`ProfileKept::CouldNotProveUnused`). Run the ancestor-ACE sweep before this call; the other order is harmless but
  cannot clean a deleted profile's leftovers. Tests (CI or opt-in only): a held profile survives and an unused one
  goes, a foreign name is untouched, two processes sweeping at once both succeed, the wrong order is harmless. Not
  run on a machine yet.
- Tests (Windows): the seven tests that spawn a confined process (`tests/e2e.rs` `windows` module, `tests/sandbox_exec.rs`)
  register an AppContainer profile for the real user, so they run only on CI (`GITHUB_ACTIONS=true`) or with
  `RIGHTKIT_TEST_REAL_USER_STORES=1`, and otherwise print why and pass. Each deletes the profile it registered when it
  ends, through two doc-hidden test-support functions (`appcontainer_profile_name_for_test`,
  `delete_appcontainer_profile_for_test`; the second accepts only a `rightkit.sandbox.<16 hex digits>` name).
  Known product gap, not changed here: a spawn creates or adopts one profile per distinct set of grants and nothing in
  the crate ever deletes one, so profiles accumulate for the user. A maintenance call that removes unused ones follows.
- Fixed (Unix): `SandboxedChild::terminate_tree` sweeps descendants only while the leader is proven to still be the spawned child (`OwnedChild::while_unreaped`); a reaped leader's pid is never signalled. Requires the `rightkit-process` release that adds `OwnedChild::while_unreaped`.
- `wait_with_output` ends the process tree before its final reap. When the leader exited and the
  pipes closed on their own, it now kills the group while the leader is still unreaped (and
  terminates a confined Windows child's job), instead of a bare `wait`. A descendant that
  redirected its output and kept running no longer outlives a successful run.

## 0.3.4

(0.3.3 was already on crates.io; the entries below ship as 0.3.4.)

- New default-on `pty` feature gates `Sandbox::spawn_pty`, `PtySession`, `PtySize`, `PtyError`,
  `PtyLeaseSnapshot`, `UnacknowledgedInputDecision` and the `portable-pty` dependency. Default
  builds are unchanged; `default-features = false` drops `portable-pty`.
- `wait_with_output` no longer reaps the child before its pipes are drained. It observes the exit with
  `ProcessHandle::wait_exited_timeout` and reaps after the drain. An unreaped leader keeps pinning its
  process-group id, so a descendant that outlives it and holds stdout or stderr is still killed with the
  group at the deadline and the capture is complete. Before, `rightkit-process` 0.3.4 (which stops signalling
  a group once its leader is reaped) left such a descendant running and `truncated` was set. Requires
  `rightkit-process` 0.3.4 (the manifest requirement is still `0.3.3` and must be raised at release).

## 0.3.0 — staged, unpublished

- **Breaking:** `SandboxOutput` gains `truncated: bool`. Struct literals of `SandboxOutput`
  must add the field.
- **Bounded capture.** `wait_with_output` keeps at most `DEFAULT_OUTPUT_CAP` (16 MiB) per stream;
  bytes past the cap are read and discarded, so the child never blocks on a full pipe and
  `truncated` is set. `wait_with_output_capped(timeout, cap)` sets the cap per call.
- **Deadline covers the tree and its pipes.** The timeout bounds the whole run, including
  descendants that inherit stdout/stderr. If a descendant still holds a pipe at the deadline,
  the tree is killed (process group on Unix, Job Object on Windows), readers get a 500 ms
  grace to reach end-of-pipe, and the result is `timed_out` with whatever was captured.
  Previously a background grandchild could hold `wait_with_output` for as long as it lived.
- Normal runs (child and pipes finish before the deadline) return the same output as before.

## 0.2.4

- Prior release; see git history.
