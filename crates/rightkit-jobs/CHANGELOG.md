# Changelog

## 0.2.8

- **Progress events carry the request id.** `EngineClient::call_with_frames` and
  `call_timeout_frames` take an `OnEventFrame` callback that receives an `EventFrame { id, event }`
  for each event, so a host can rebuild the engine's `{id, event}` frame. `CallOptions::on_frame`
  is the supervisor form. The payload-only `on_event` API is unchanged, and one call may set both.
- **Contract version gate.** `ClientOptions::require_contract_version` and
  `SupervisorConfig::require_contract_version` take a `u32` or a `&str`. A hello that does not
  advertise the required version fails the connect with `CONTRACT_VERSION_MISMATCH`
  (`contract_version_mismatch`, not retryable); during a restart it spends restart budget.
  `ClientOptions` is no longer `Copy`, because it can hold the requirement and the sink.
- **Status: restarts and terminal state.** `SupervisorStatus` gains `restarts: u32` (restarts the
  policy scheduled since the supervisor was created, retries of a failed first boot included) and
  `terminal: Option<Terminal>`.
  `Terminal::Failed { reason }` marks a `Down` that will not restart by itself: a failed first
  `start()`, a spent restart budget, a crash with restarts disabled, or a spent on-demand budget.
  `Terminal::Stopped` marks an explicit `shutdown`. `Lifecycle` and `lifecycle()` are unchanged, so
  both terminal states still read as `Down`.
- **Non-blocking start.** `EngineSupervisor::start_background()` moves the supervisor to `Booting`
  and returns a `Readiness` handle at once. `ready()` waits for the outcome, `wait_timeout()` bounds
  the wait, and `try_ready()` polls. `start()` still blocks until `Ready`.
- **First-boot failures retry by default.** A failed first boot (spawn, hello, handshake) spends
  restart budget like a crash. `start()` and `Readiness` resolve once the engine is `Ready` or the
  supervisor is `Down`, and `Down` carries the last boot error. `SupervisorConfig::with_initial_failure(InitialFailure::Terminal)`
  restores the old give-up-at-once behaviour. A contract mismatch is terminal on every boot.
- **Errors say where they arose.** `JobError` gains `origin: ErrorOrigin` (`Engine` for errors the
  engine answered with, `Supervisor` for errors the shell raised: `down`, `not_ready`, `timeout`,
  `engine_exited`, `engine_write`, `io`, `frame_too_large`, `hello_timeout`, `engine_protocol`,
  `contract_version_mismatch`). Set `JobError::supervisor` and `with_origin` to build one. Struct
  literals of `JobError` need the new field.
- **Engine error replies always carry a message.** A reply with no `code` uses the fallback code
  (`engine_error`, or `ClientOptions::with_fallback_error_code` / `SupervisorConfig::with_fallback_error_code`).
  A reply with no usable `message` reads `engine reported failure without detail`.
- **Malformed stdout lines are reported.** `ClientOptions::with_protocol_error_sink` and
  `SupervisorConfig::with_protocol_error_sink` receive a `ProtocolError { line, message }` for each
  line that is not a JSON frame. The reader still drops the line and resynchronises.
- **Re-exports.** `OwnedCommand`, `RestartMode`, `RestartPolicy` and `JobAssignmentMode` from
  `rightkit-process` are re-exported, so a host needs no direct `rightkit-process` pin to build a
  supervisor or set a job-assignment mode (CutRight uses `BestEffort`).

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
