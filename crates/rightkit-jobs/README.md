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

## When to use it

- Use it for long-running engine work that must survive a crash, be de-duplicated by request id, report progress, and be cancelled.
- Use it when an engine must answer a stdio or CLI job protocol.

Do not use it for one-shot calls that need no persistence. Do not use it to store app data outside jobs.

## Cargo features

`service` (default on): `DispatcherHandler`, which serves a `Dispatcher` over `rightkit-service` (`Cargo.toml:14-18`, `src/lib.rs:10`). Engines that only speak stdio or CLI build with `default-features = false`.

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

`tests/dispatcher.rs`, `tests/e2e.rs`, `tests/engine_contract.rs`, `tests/fencing.rs`, `tests/genright.rs`, `tests/job_lock.rs`, `tests/supervisor.rs`, `tests/v021.rs`. The `examples/` directory has `job_engine.rs` (the test engine) and `engine_fixture.rs`.

## Platform support

The crate depends on `rightkit-process` and `rightkit-service`, which have their own per-OS behaviour. `src/` has no `cfg(unix)` or `cfg(windows)` code; the job lock comes from `rightkit_fs::FileLock` (POSIX record lock on Unix, `LockFileEx` elsewhere, `src/runner.rs:34-35`), and only some tests are `cfg(unix)` (`tests/job_lock_process.rs:1`). Windows behaviour is not otherwise documented in this crate.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).
