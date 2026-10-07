# rightkit-jobs

Shared engine base for Right Suite apps: a persistent SQLite job store
(`rightkit-db`), `request_id` de-duplication, a bounded progress-event log,
cancel, crash recovery, sha256-addressed result artifacts, and the engine
stdio/CLI command contract (`rightkit-framed-sidecar` frames, `rightkit-process`
child ownership). Product work plugs in through `JobHandler` (what a job does)
and `CommandSet` (extra commands). See `API.md` in the extraction lane for the
adapter interface.

Rules the crate enforces:

- Passive reads (`get`, `list`, `events`, `recoverable_ids`, `job.status`,
  `job.wait`) never run or revive a job. Only `Runner::run`,
  `Runner::resume_recoverable`, `job.run`, `job.recover`, or `job.submit` with
  `run: true` execute work.
- A job left `queued`/`running` whose per-job lock is free is *recoverable*.
- Retryable handler errors pause the job (still recoverable); other errors fail it.
- Cancelling is observed through `JobControl::is_cancelled`; a cancelled job is
  never completed.

Scheduling and handoff (modelled on HeardRight's speech service; see `sched.rs`,
`runner.rs`):

- **Interactive preempts batch at step boundaries.** `request.priority:
  "interactive"` holds the runner's `Gate` for the whole run. A batch handler calls
  `ctl.step_boundary()?` between steps (e.g. one audio window per turn); while
  interactive work is active it blocks there, emitting `preempted` / `resumed`
  events, and the job is not retried.
- **Progress.** `ctl.progress(stage, fraction, message)` stores an event and streams it
  to the requesting client as an `event` frame.
- **Cancel on disconnect.** `job.submit|run` with `cancel_on_disconnect: true`: if the
  client connection closes while the job runs, the job is cancelled
  (`CommandSet::connection_closed`; works on stdio EOF and on `rightkit-service`).
- **Retryable handoff.** `Gate::request_handoff()` (wired to `rightkit-service`'s
  takeover by `DispatcherHandler::with_gate`) makes `step_boundary` return
  `JobError::handoff()`: the job pauses without spending an attempt, the wire error is
  `host_handoff` with `retryable: true`, and the client resubmits to the new host.
- **Stall timeout, no fixed total.** `Runner::with_stall_timeout(d)` abandons an attempt
  that shows no sign of life (`progress`, `step_boundary`, `heartbeat`) for `d` and pauses
  the job as retryable `stalled`. A long job that keeps making progress is never cut off.
- **Over the service convention.** `DispatcherHandler` serves a `Dispatcher` on
  `rightkit-service` (service.json, user-only socket/pipe, per-app allowlist, ranking).

## Extension points (0.2)

`JobStore::open` keeps the 0.1 store. `JobStore::open_configured(path, StoreConfig)`
adapts it to an app, including an existing database (adopted in place, additive only):

- **Extension columns** — `StoreConfig::column(ExtColumn::text("provider").not_null())`
  declares app columns on `jobs` (TEXT/INTEGER/REAL/JSON, NOT NULL, defaults). Missing
  ones are added with `ALTER TABLE`; an undeclared NOT NULL column without a default is
  refused with `schema` before anything is touched. Values travel as `Ext`
  (`submit_ext`, `set_ext`, `JobRecord::ext`) and appear as top-level wire fields.
- **Status vocabulary** — `StatusModel::new("submitted", ["submitted", "running"])
  .allow(..)`; `transition` enforces it (`illegal_transition`). Terminal statuses stay
  `complete`/`failed`/`cancelled`.
- **External submissions** — `StoreConfig::external_id("provider_job_id")` plus
  `begin_submission` / `accept_submission` / `fail_submission`: a stored external id
  resumes (poll, never resubmit); a counted-but-unrecorded attempt replays with the
  same idempotency key and without repeating its hook; resubmission is bounded.
  `Runner::with_attempt_policy(AttemptPolicy::ByHandler)` lets the handler own attempts.
- **Transactional hook** — `StoreHook::{on_create, on_attempt, on_transition}` run in the
  same write transaction as the change; an error rolls it back (e.g. a spend cap).
  `app_schema`, `write`, and `read` serve app-owned tables on the same connection.
- **Artifacts** — `ArtifactNames::new("assets", "asset_id", "asset_id")`;
  `add_artifact_with_id` / `record_artifact` take app-chosen ids idempotently.
- **Wire contract** — `JobCommands` is optional; `with_name`, `without`, and a
  `PayloadMapper` (input/output/event/error) map it onto an app's own `job.*` contract.
  `Dispatcher::without_builtin`, `with_doctor`, and `with_cli_target` adjust built-ins
  and CLI targets.

### 0.2.1 additions

- **Events with explicit status/attempt** — `JobStore::add_event_with(job_id, status, stage,
  attempt, progress, message)` records the given status (must be in the configured
  vocabulary) and/or attempt (`None` keeps the job row's value). `add_event` is unchanged.
- **`retryable` is optional** — `StoreConfig::include_retryable(false)` writes and reads job
  errors as `{code, message}` (also stripping the key from rows stored earlier);
  `Dispatcher::with_error_retryable(false)` does the same for response errors;
  `JobError::to_json_with(false)` is the primitive. Default is `true` (0.2.0 shape).
- **Quiet runner** — `Runner::with_implicit_events(false)` stops the runner emitting
  `started`/`complete`/`paused`/`failed`/`cancelled`; the handler owns the stream through
  `JobControl::progress` (and `add_event_with`). Locking, attempts, status changes and
  outcomes are unchanged.

### Apps with an existing wire protocol

Apps that already have a stdio/CLI protocol (GenRight) should adopt `JobStore` plus
`StoreConfig`/`StoreHook`/`StatusModel` and keep their own run loop, back-off, wait/follow
loop, and wire. These parts are optional and independent:

- **`Runner`** — optional. It owns lock + attempt accounting + the handler call, and (unless
  `with_implicit_events(false)`) the lifecycle events. It does not model poll back-off, a
  `recoverable:true` record shape, or `RunOptions.total`; those stay in the handler or app
  (`AttemptPolicy::ByHandler`, `JobControl::transition`, `begin/accept/fail_submission`).
- **`Dispatcher`** — optional. Non-job methods already go in through `Dispatcher::with(CommandSet)`,
  so an app's shared command table can live there; `without_builtin`, `with_doctor` and
  `with_cli_target` adjust built-ins and CLI targets. It does not grow a per-method CLI
  grammar (`generate --wait`, `job wait --wait-timeout`); an app whose CLI words differ keeps
  its own CLI parsing and calls `Dispatcher::call` or its own functions.
- **`JobCommands`** — optional. It serves canonical `job.*`/`artifact.resolve`, with
  `with_name`, `without` and `PayloadMapper`; `job.submit` with `wait`/`run` and `job.wait`
  already give create+run+wait and blocking wait.
- **`JobStore`** — the shared part: schema, transitions, submissions, events, artifacts.
