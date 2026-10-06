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
