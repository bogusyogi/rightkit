# Changelog

## 0.2.8 (additions for long-running desktop jobs)

- **Recovery class.** `JobStore::submit_with(&NewJob, &Ext, &SubmitOptions)` takes
  `Recovery::Resume` (default, unchanged behaviour) or `Recovery::Never`. Once a `Never` job has
  begun an attempt, `Runner::resume_recoverable`, `Runner::recoverable_ids` and `Runner::run`
  fail it (`failed`, error code `INTERRUPTED` = `interrupted`, an `interrupted` event, the store
  hook's `on_transition`) and never hand it to a handler again; a retryable handler error fails
  it the same way instead of pausing. A `Never` job that never began still runs. The class is in
  the new crate-owned table `rk_job_meta`; the app's `jobs` table is not altered, and every row
  written by 0.2.7 reads as `Resume`. Downgrading to 0.2.7 would let it resume such jobs again.
  `recoverable_ids` is no longer purely passive for `Never` jobs.
- **Pruning.** `JobStore::prune(&PrunePolicy)` (`older_than`, `keep_latest`; either limit
  removes a job, a policy with neither is refused) deletes terminal jobs with their events,
  artifact rows, submission rows and metadata in one transaction. Artifact files are never
  deleted; `PruneReport::artifacts` returns the rows and paths. `SubmitOptions::prune` applies
  the same bound inside the submit transaction (`Submitted::pruned`).
- **Progress durability.** `StoreConfig::progress_policy(ProgressPolicy)`: `relaxed_durability`
  (WAL only) and `coalesce(interval)` apply to `JobControl::progress` ticks that carry a fraction.
  Default is unchanged. Transitions, results, runner lifecycle events and `Never` markers stay
  fully durable, and pending ticks are flushed in the commit of the next state change.
  `JobStore::progress_fenced`, `flush_progress`, `progress_stats`.
- **Cancel token.** `JobControl::cancel_token()` / `JobStore::cancel_token(id)`: an atomic flag
  set by `JobStore::cancel` and by a single-flight supersede in the same process.
  `JobControl::is_cancelled` checks it first, then the database (cross-process cancel), which is
  the source of truth.
- **Single-flight.** `SubmitOptions::single_flight(key)`: the new job cancels every non-terminal
  job with the same key (error code `SUPERSEDED`). A running predecessor is cancelled in the
  database at once and signalled; its late result is discarded; nothing waits for it.
- **Features.** New `contract` (gates `rightkit-framed-sidecar`: `Dispatcher`, `serve`,
  `run_cli`, ...) and `supervisor` (gates `rightkit-process`: `EngineSupervisor`,
  `EngineClient`, re-exports; implies `contract`). `service` now implies `contract`. Default is
  `service` + `supervisor`, the same build as before. `default-features = false` is now the
  store and runner only; consumers that serve the contract without defaults add `contract`.

## 0.2.8 (supervisor and contract)

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
