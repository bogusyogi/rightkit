# rightkit-jobs

Shared engine base for Right Suite apps: a persistent SQLite job store (on `rightkit-db`), `request_id` de-duplication, a bounded progress-event log, cancel, crash recovery, sha256-addressed result artifacts, and the engine stdio and CLI command contract. The contract uses `rightkit-framed-sidecar` frames and `rightkit-process` child ownership, and can be served over `rightkit-service`.

Product work plugs in through `JobHandler` (what a job does) and `CommandSet` (extra commands). Apps with their own wire protocol can use only the store (see below).

Catalog capability: `persistent-jobs`. Published to crates.io (see [INDEX.md](../../INDEX.md)). Version history: [CHANGELOG.md](CHANGELOG.md).

## What it owns

- `JobStore` and `StoreConfig`: schema, status transitions, submissions, events and artifacts.
- `Runner`, `JobLock`, `AttemptPolicy`: per-job locks, attempt accounting, and the handler call.
- `JobHandler` and `JobControl`: the handler trait and the control object (`step_boundary`, `progress`, `is_cancelled`, `generation`).
- `Dispatcher`, `CommandSet`, `JobCommands`, `PayloadMapper`: the stdio and CLI contract, with the built-in `job.*` methods.
- `EngineClient`: client for a running engine.
- `DispatcherHandler` and `Gate`: serve a `Dispatcher` on `rightkit-service`, and hand off to a new host.
- `StagedPool`, `PoolConfig`, `StagedHandler`: admission-limited parallel prepare with one thread-affine compute lane (see below).

## When to use it

- Use it for long-running engine work that must survive a crash, be de-duplicated by request id, report progress, and be cancelled.
- Use it when an engine must answer a stdio or CLI job protocol.

Do not use it for one-shot calls that need no persistence. Do not use it to store app data outside jobs.

## Cargo features

Default features are `service` and `supervisor`. `default-features = false` builds the store, runner, locks, recovery classes, pruning and progress policy only.

- `contract`: the stdio/CLI command contract (`Dispatcher`, `serve`, `run_cli`, `JobCommands`); pulls `rightkit-framed-sidecar`.
- `supervisor` (implies `contract`): `EngineSupervisor`, `EngineClient` and the `rightkit-process` re-exports; pulls `rightkit-process`.
- `service` (implies `contract`): `DispatcherHandler` over `rightkit-service`.

Per-job `Recovery::Never` (destructive jobs are failed as `interrupted` after a crash, never re-run), `JobStore::prune`, `JobStore::delete`, `JobStore::submit_rejected`, `ProgressPolicy`, `CancelToken`, single-flight submit (`SubmitOptions::single_flight`), the staged pool and held jobs are documented on their types and in the changelog. The staged pool (`pool` module) and held state are ungated and build with `default-features = false`.

## Usage

Handler, taken from `examples/job_engine.rs` (a compiled example target):

```rust
use rightkit_jobs::*;
use serde_json::Value;

struct Handler;

impl JobHandler for Handler {
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value> {
        let steps = job.request["steps"].as_i64().unwrap_or(5);
        for i in 0..steps {
            ctl.step_boundary()?;
            ctl.progress("step", Some((i + 1) as f64 / steps as f64), Some(&format!("window {i}")));
        }
        Ok(serde_json::json!({ "steps": steps }))
    }
}
```

`step_boundary()?` is where a batch job yields to interactive work, pauses for a handoff, or observes a stall. Handler errors that are retryable pause the job. Other errors fail it.

## Rules the crate enforces

- Passive reads (`get`, `list`, `events`, `recoverable_ids`, `job.status`, `job.wait`) never run or revive a job. Only `Runner::run`, `Runner::resume_recoverable`, `job.run`, `job.recover`, or `job.submit` with `run: true` execute work.
- A job left `queued` or `running` whose per-job lock is free is recoverable.
- Retryable handler errors pause the job, and it stays recoverable. Other errors fail it.
- Cancellation is observed through `JobControl::is_cancelled`. A cancelled job is never completed.

## Scheduling and handoff

Modelled on HeardRight's speech service (`src/sched.rs`, `src/runner.rs`):

- **Interactive preempts batch at step boundaries.** `request.priority: "interactive"` holds the runner's `Gate` for the whole run. A batch handler calling `ctl.step_boundary()?` blocks while interactive work is active. It emits `preempted` and `resumed` events, and the job is not retried.
- **Progress.** `ctl.progress(stage, fraction, message)` stores an event and streams it to the requesting client as an `event` frame.
- **Cancel on disconnect.** `job.submit` or `job.run` with `cancel_on_disconnect: true` cancels the job if the client connection closes while it runs (`CommandSet::connection_closed`). This works on stdio EOF and on `rightkit-service`.
- **Retryable handoff.** `Gate::request_handoff()`, wired to `rightkit-service` takeover by `DispatcherHandler::with_gate`, makes `step_boundary` return `JobError::handoff()`. The job pauses without spending an attempt. The wire error is `host_handoff` with `retryable: true`, and the client resubmits to the new host.
- **Stall timeout, no fixed total.** `Runner::with_stall_timeout(d)` abandons an attempt that shows no sign of life (`progress`, `step_boundary`, `heartbeat`) for `d`, and pauses the job as retryable `stalled`. A long job that keeps making progress is never cut off.
- **Dispatcher over the service convention.** `DispatcherHandler` serves a `Dispatcher` on `rightkit-service` (service.json, a user-only socket or pipe, a per-app allowlist, and ranking).

## Staged pool

`pool::StagedPool` runs jobs in two stages. `StagedHandler::prepare` runs on parallel threads, up to a limit; `StagedHandler::compute` runs on one lane thread that built its lane value with `new_lane` and uses it for its whole life. Use it when the first stage is I/O or CPU work that can overlap, and the second needs one thread-affine resource (a model, a device, a native handle).

```rust
let pool = StagedPool::start(
    store.clone(),
    JobLocks::new(lock_dir),
    handler.clone(),          // Arc<impl StagedHandler>
    PoolConfig::new(|| max_parallel())   // read on every admission
        .handoff(2)           // buffered items; a full buffer blocks prepare
        .idle_after(Duration::from_secs(45)),
    |runner| runner.with_stall_timeout(Duration::from_secs(600)),
)?;
pool.enqueue(&job_id)?;       // FIFO admission; ids already queued are ignored
pool.resume_recoverable()?;   // enqueue every recoverable job, oldest first
pool.shutdown();
```

- Admission is FIFO and bounded by the limit closure. A raised limit is seen within 50 ms.
- Each admitted job still runs through `Runner::run`, so the per-job lock, fencing, attempt accounting and lifecycle events are unchanged.
- `prepare` returns `Staged::Done(value)` to finish without the lane, or `Staged::Compute(work)` to queue for it.
- `compute` and `idle` are called only on the lane thread. The lane claims each item before it computes, so an item whose waiter already withdrew (cancel, stall, shutdown) is skipped.
- `shutdown` stops admission and withdraws buffered or waiting work with the retryable host-handoff error. Those jobs stay recoverable. A `prepare` or `compute` call already running is not interrupted; shutdown waits for it, so a call that never returns blocks shutdown.
- The pool adds a continuous thread (dispatcher and lane, plus one thread per admitted job). Its resource budget is not yet measured; see `runtime-budget.json`.

## Delete, keep-total, rejected submissions and recovering open

- `JobStore::delete(job_id) -> PruneReport`. A non-terminal job is cancelled first (the hook sees `cancelled`), then its rows are removed in one transaction and its cancel token is set. It does not wait for the handler to stop: a running handler sees the cancel at its next check, and a late write it makes is refused. Artifact files are never deleted; the caller removes `report.artifacts[..].path` if it wants them gone.
- `PrunePolicy::keep_total(n)` bounds the whole table to `n` rows. It keeps `n` minus the non-terminal count of the newest terminal rows, so active jobs are never dropped. Inside `SubmitOptions::prune` the incoming row counts.
- `JobStore::submit_rejected(&NewJob, &Ext, &JobError) -> JobRecord`. The job is inserted, `on_create` runs, and the job moves to `failed` with the error and a `rejected` event, all in one transaction. It never runs, and a crash cannot leave it runnable. The app's entitlement check stays in the app; call this when the app refuses.
- `JobStore::open_recovering(&DbOptions, StoreConfig, RecoveryPolicy) -> (JobStore, Option<PathBuf>)`. If the database file is unreadable and the policy allows it, the file is moved aside and a fresh store is created. The second value is where the old file went; the original bytes are kept under the `.corrupt-` name.

## Per-job resume and sweep

- `Runner::resume_recoverable_each() -> Vec<(String, JobResult<RunOutcome>)>` attempts every recoverable job, oldest first. Each keeps its own result: one failing job does not stop the rest, and a job that cannot be examined is reported by id. `Runner::resume_recoverable` is unchanged and stops at the first error.
- `Runner::sweep_unclaimed(root) -> Vec<PathBuf>` removes child directories of `root` that contain no path claimed by a non-terminal job (`JobStore::claimed_paths`). Plain files are left alone and a symlink is never followed out of `root`. Call it only when this process is the sole engine for `root`. Not calling it is the off switch.

## Held jobs

A held status is a non-terminal status that waits for a person or an outside check. Declare it on the status model and enter it from a submission:

```rust
let model = StatusModel::new("submitted", ["submitted", "running", "needs_auth"])
    .allow("running", "submitted")
    .hold("needs_auth");
// later, on the submission's failure path:
store.hold_submission(&job_id, &JobError::new("needs_auth", "sign in again"), "needs_auth")?;
// once an operator or outside check decides:
store.resolve(&job_id, &Resolution::Resubmit, Some("submitted"))?;
```

- **A held job leaves only through `JobStore::resolve`.** Every other status move returns `illegal_transition`.
- `Resolution::Resubmit` marks the submission failed, clears its external id and moves the job to `to`; the next `begin_submission` counts one new attempt. `Resolution::Abandon(error)` ends the job `failed`. `Resolution::Adopt(value)` ends it `complete`.
- `resolve` runs in one transaction with `StoreHook::on_transition` and `StoreHook::on_resolve`, so a ledger keeps or releases its reservation atomically. A hook error rolls the resolution back and the job stays held.
- The runner neither lists nor runs a held job: `Runner::run` returns `RunOutcome::Paused`, and recovery skips it. `cancel`, `delete` and single-flight supersede skip or refuse it.

## Extension points

`JobStore::open` opens the base store. `JobStore::open_configured(path, StoreConfig)` adapts it to an app, including an existing database. Changes are additive only.

- **Extension columns.** `StoreConfig::column(ExtColumn::text("provider").not_null())` declares app columns on `jobs` (TEXT, INTEGER, REAL or JSON, with NOT NULL and defaults). Missing columns are added with `ALTER TABLE`. An undeclared NOT NULL column without a default is refused with `schema` before anything is touched. Values travel as `Ext` (`submit_ext`, `set_ext`, `JobRecord::ext`) and appear as top-level wire fields.
- **Status vocabulary.** `StatusModel::new("submitted", ["submitted", "running"]).allow(..)`. `transition` enforces it and returns `illegal_transition` otherwise. Terminal statuses stay `complete`, `failed` and `cancelled`.
- **External submissions.** `StoreConfig::external_id("provider_job_id")` with `begin_submission`, `accept_submission` and `fail_submission`. A stored external id resumes (poll, never resubmit). A counted but unrecorded attempt replays with the same idempotency key and without repeating its hook. Resubmission is bounded. `Runner::with_attempt_policy(AttemptPolicy::ByHandler)` lets the handler own attempts.
- **Transactional hook.** `StoreHook::{on_create, on_attempt, on_transition}` run in the same write transaction as the change. An error rolls it back, for example a spend cap. `app_schema`, `write` and `read` serve app-owned tables on the same connection.
- **Artifacts.** `ArtifactNames::new("assets", "asset_id", "asset_id")`. `add_artifact_with_id` and `record_artifact` take app-chosen ids idempotently.
- **Wire contract.** `JobCommands` is optional. `with_name`, `without` and a `PayloadMapper` (input, output, event, error) map it onto an app's own `job.*` contract. `Dispatcher::without_builtin`, `with_doctor` and `with_cli_target` adjust built-ins and CLI targets.
- **Attempt fencing.** Each run start advances a durable generation (`JobStore::generation`). Writes through `JobControl` from a superseded attempt, for example a stalled worker abandoned by the watchdog, fail with `stale_attempt` and change nothing. The watchdog fences an abandoned attempt before it releases the job lock. `JobControl::generation()` returns the generation a handler writes under. Writes through `JobControl::store()` are not fenced.
- **Events with explicit status and attempt.** `JobStore::add_event_with(job_id, status, stage, attempt, progress, message)` records the given status (it must be in the configured vocabulary) and attempt. `add_event` is unchanged.
- **Optional `retryable`.** `StoreConfig::include_retryable(false)` writes and reads job errors as `{code, message}`. `Dispatcher::with_error_retryable(false)` does the same for response errors. The default keeps `retryable`.
- **Quiet runner.** `Runner::with_implicit_events(false)` stops the runner emitting `started`, `complete`, `paused`, `failed` and `cancelled`. The handler owns the stream through `JobControl::progress` and `add_event_with`.

## Apps with an existing wire protocol

Apps that already have a stdio or CLI protocol can adopt `JobStore` with `StoreConfig`, `StoreHook` and `StatusModel`, and keep their own run loop, back-off, wait and follow loop, and wire. The other parts are optional and independent:

- **`Runner`** is optional. It owns lock, attempt accounting and the handler call, and (unless `with_implicit_events(false)`) the lifecycle events. It does not model poll back-off, a `recoverable: true` record shape, or `RunOptions.total`. Those stay in the handler or app (`AttemptPolicy::ByHandler`, `JobControl::transition`, `begin_submission`, `accept_submission`, `fail_submission`).
- **`Dispatcher`** is optional. Non-job methods go in through `Dispatcher::with(CommandSet)`. It does not grow a per-method CLI grammar. An app whose CLI words differ keeps its own parsing and calls `Dispatcher::call` or its own functions.
- **`JobCommands`** is optional. It serves canonical `job.*` and `artifact.resolve` methods, with `with_name`, `without` and `PayloadMapper`. `job.submit` with `wait` or `run`, and `job.wait`, already give create, run and wait, and blocking wait.
- **`JobStore`** is the shared part: schema, transitions, submissions, events and artifacts.

## Tests

`tests/dispatcher.rs`, `tests/e2e.rs`, `tests/engine_contract.rs`, `tests/fencing.rs`, `tests/genright.rs`, `tests/job_lock.rs`, `tests/supervisor.rs`, `tests/v021.rs`, `tests/prune.rs`, `tests/delete_reject.rs`, `tests/held.rs`, `tests/resume_sweep.rs`, `tests/pool.rs`. The `examples/` directory has `job_engine.rs` (the test engine) and `engine_fixture.rs`.

## Platform support

The crate depends on `rightkit-process` and `rightkit-service`, which have their own per-OS behaviour. `src/` has no `cfg(unix)` or `cfg(windows)` code; the job lock comes from `rightkit_fs::FileLock` (POSIX record lock on Unix, `LockFileEx` elsewhere, `src/runner.rs:34-35`), and only some tests are `cfg(unix)` (`tests/job_lock_process.rs:1`). Windows behaviour is not otherwise documented in this crate.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 14.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: 1 per second.
- Disk operations ceiling: 2 operations per second.
- Required scale: 10,000 calls or more.
- Required hosts: macOS and Windows.
- Status exposure: `EngineSupervisor::status()` in `src/supervisor.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
