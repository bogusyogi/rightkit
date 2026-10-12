# Changelog

## Unreleased

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
