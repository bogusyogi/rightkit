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
