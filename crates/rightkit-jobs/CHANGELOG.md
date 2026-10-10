# Changelog

## 0.2.7

- The job lock uses `rightkit_fs::FileLock` (same POSIX record lock on unix, `LockFileEx` on
  Windows, process-wide registry). The crate no longer contains `unsafe` or a direct `libc`
  dependency. No owner side file is written (`rightkit-fs` `json` feature is not used by this
  crate). Behaviour is unchanged.
- New default-on feature `service` gates `DispatcherHandler` and the `rightkit-service`
  dependency. Engines that only speak stdio/CLI can build with `default-features = false`.
- `store::now()` formats the UTC timestamp with std; the `time` dependency is gone. Output
  (`YYYY-MM-DDTHH:MM:SSZ`) is unchanged.

## 0.2.6

- **`nix` removed.** The job lock calls `libc::fcntl(F_SETLK)` directly (same POSIX record lock), retrying on `EINTR`. Behaviour is unchanged.

## 0.2.4 — unpublished

- **Job locks no longer look busy after a fork.** `JobLocks::try_lock` used `flock`, which
  belongs to the open file description. A child forked by another thread (for example a
  `rightkit-process` spawn with `pre_exec`) inherits the lock descriptor until it execs, so a
  job whose lock was just released could report `WouldBlock`. That made `recoverable_ids` and
  `resume_recoverable` skip the job, and `run` return `locked_elsewhere`. Locks are now POSIX
  record locks (`fcntl F_SETLK`, through `nix`), which belong to the process and are not
  inherited. A process-wide registry keeps in-process exclusion, and every acquire and release
  runs under it, so one process never holds two descriptors on one lock file. Windows keeps
  `LockFileEx` through `File::try_lock`.
- Adds a `unix` dependency on `nix` (pinned `=0.30.1`, `fs` feature, already in the workspace).

## 0.2.3 — staged, unpublished

- **Attempt fencing.** Every run start advances a durable attempt generation stored on the
  job (`jobs.attempt_gen`, added additively on adoption). Writes made through `JobControl`
  (`transition`, `set_ext`, `progress`, `begin_submission`, `accept_submission`,
  `fail_submission`, `add_artifact`) carry that generation and are rejected with the new
  `stale_attempt` error (`STALE_ATTEMPT`, non-retryable) once a newer attempt owns the job.
- **Watchdog keeps ownership until fenced.** When the stall watchdog abandons an attempt, the
  runner fences it (`JobStore::fence`) before releasing the job lock. The abandoned worker
  thread can keep running, but its late writes no longer reach the job or its event stream.
- **Store API.** New `JobStore::{generation, fence, begin_run_fenced, transition_fenced,
  set_ext_fenced, record_error_fenced, refund_attempt_fenced, fail_fenced, complete_fenced,
  begin_submission_fenced, accept_submission_fenced, fail_submission_fenced, add_event_fenced,
  add_artifact_fenced}`. The unfenced methods are unchanged.
- Known gap: `JobControl::store()` returns the raw store, whose unfenced methods bypass the
  fence. Handlers must write through `JobControl`.

## 0.2.2

- Prior release; see git history.
