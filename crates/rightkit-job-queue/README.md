# rightkit-job-queue

Shared engine base for Right Suite apps: a persistent SQLite job store (on `rightkit-db`), `request_id` de-duplication, a bounded progress-event log, cancel, crash recovery, sha256-addressed result artifacts, and the engine stdio and CLI command contract. The contract uses `rightkit-sidecar` frames and `rightkit-child-process` child ownership, and can be served over `rightkit-daemon`.

Product work plugs in through `JobHandler` (what a job does) and `CommandSet` (extra commands). Apps with their own wire protocol can use only the store (see below).

Catalog capability: `persistent-jobs`. Published to crates.io at 0.2.7 (registry record); the source is 0.3.0, which [INDEX.md](../../INDEX.md) flags as a mismatch. Version history: [CHANGELOG.md](CHANGELOG.md).

## What it owns

- `JobStore` and `StoreConfig`: schema, status transitions, submissions, events and artifacts.
- `Runner`, `JobLock`, `AttemptPolicy`: per-job locks, attempt accounting, and the handler call.
- `JobHandler` and `JobControl`: the handler trait and the control object (`step_boundary`, `progress`, `is_cancelled`, `generation`).
- `Dispatcher`, `CommandSet`, `JobCommands`, `PayloadMapper`: the stdio and CLI contract, with the built-in `job.*` methods.
- `EngineClient`: client for a running engine.
- `DispatcherHandler` and `Gate`: serve a `Dispatcher` on `rightkit-daemon`, and hand off to a new host.
- `StagedPool`, `PoolConfig`, `StagedHandler`: admission-limited parallel prepare with one thread-affine compute lane (see below).
- `HandlerOutcome`: a handler that settles its own jobs (or hands them on) says so, and the runner writes nothing more (see "Handler-owned outcomes").
- Settle-later (`JobStore::settle_terminal`, `flush_pending_settlements`, `shutdown_detailed`, `RunOutcome::Unsettled`): a terminal write that storage refuses is held and retried, not lost (see "Settle later").
- `ClaimScope`, `JobStore::claimed_paths_with`, `Runner::sweep_unclaimed_with`: which jobs' files a sweep treats as owned.
- `JobError::open_failure` and `OpenFailure`: a typed classification of database failures.

## When to use it

- Use it for long-running engine work that must survive a crash, be de-duplicated by request id, report progress, and be cancelled.
- Use it when an engine must answer a stdio or CLI job protocol.

Use `InlineCancelRegistry` for cancellable inline calls without persistence, & `DoctorProbe` for discovered engine capability probes. Do not use it to store app data outside jobs.

## Cargo features

No feature is on by default (0.3.0): a bare dependency builds error types & `InlineCancelRegistry`. Select what you use; `store`, `supervisor`, `contract` and `service` together restore the 0.2.x default.

- `store`: the job store, runner, locks, recovery classes, pruning, scheduler, pool and progress policy; pulls `rightkit-db`, `rightkit-fs` and `uuid`.
- `protocol`: the wire constants (`CONTRACT_VERSION`, `DEFAULT_FRAME_LIMIT`); pulls `rightkit-sidecar`.
- `contract` (implies `store` and `protocol`): the stdio/CLI command contract (`Dispatcher`, `serve`, `run_cli`, `JobCommands`).
- `doctor`: bounded CLI doctor probes & binary-identity capability caching; pulls only `rightkit-child-process`.
- `supervisor` (implies `protocol` & `doctor`): `EngineSupervisor`, `EngineClient` and the `rightkit-child-process` re-exports; pulls `rightkit-child-process`, not `rightkit-db`.
- `service` (implies `contract`): `DispatcherHandler` over `rightkit-daemon`.

Per-job `Recovery::Never` (destructive jobs are failed as `interrupted` after a crash, never re-run; set per submit, or once per job kind with `StoreConfig::recovery_for_kind`), `JobStore::prune`, `JobStore::delete`, `JobStore::submit_rejected`, `ProgressPolicy`, `CancelToken`, single-flight submit (`SubmitOptions::single_flight`), the staged pool and held jobs are documented on their types and in the changelog. The staged pool (`pool` module) and held state are ungated and build with `default-features = false`.

## Usage

Handler, taken from `examples/job_engine.rs` (a compiled example target):

```rust
use rightkit_job_queue::*;
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
- **Cancel on disconnect.** `job.submit` or `job.run` with `cancel_on_disconnect: true` cancels the job if the client connection closes while it runs (`CommandSet::connection_closed`). This works on stdio EOF and on `rightkit-daemon`.
- **Retryable handoff.** `Gate::request_handoff()`, wired to `rightkit-daemon` takeover by `DispatcherHandler::with_gate`, makes `step_boundary` return `JobError::handoff()`. The job pauses without spending an attempt. The wire error is `host_handoff` with `retryable: true`, and the client resubmits to the new host.
- **Stall timeout, no fixed total.** `Runner::with_stall_timeout(d)` abandons an attempt that shows no sign of life (`progress`, `step_boundary`, `heartbeat`) for `d`, and pauses the job as retryable `stalled`. A long job that keeps making progress is never cut off.
- **Dispatcher over the service convention.** `DispatcherHandler` serves a `Dispatcher` on `rightkit-daemon` (service.json, a user-only socket or pipe, a per-app allowlist, and ranking).

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
- `prepare` returns `Staged::Done(value)` to finish without the lane, `Staged::Compute(work)` to queue for it, or `Staged::Handled(HandlerOutcome)` when it already settled the job (see "Handler-owned outcomes").
- `compute` and `idle` are called only on the lane thread. The lane claims each item before it computes, so an item whose waiter already withdrew (cancel, stall, shutdown) is skipped.
- `shutdown` stops admission and withdraws buffered or waiting work with the retryable host-handoff error. Those jobs stay recoverable. A `prepare` or `compute` call already running is not interrupted; shutdown waits for it, so a call that never returns blocks shutdown.
- The pool adds a continuous thread (dispatcher and lane, plus one thread per admitted job). Its resource budget is not yet measured; see `runtime-budget.json`.

## Handler-owned outcomes

A handler that settles its own jobs (it writes the terminal state through `JobControl::transition`, `JobControl::settle` or the store) or hands them to something that will (a provider callback, a held status) returns a `HandlerOutcome` instead of a bare value. Override `JobHandler::run_outcome`; `run` must still exist, and a handler that implements only `run` is untouched (its `Ok(value)` is `HandlerOutcome::Done(value)`).

```rust
fn run_outcome(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<HandlerOutcome> {
    let result = work(job)?;                    // an Err is a failure, as always
    ctl.settle(&Transition::to(COMPLETE).result(result).error(None))?;
    Ok(HandlerOutcome::Settled)                 // the runner writes nothing more
}
```

- `Done(value)`: the runner completes the job with `value`, as before.
- `Settled`: the handler already wrote the terminal state. The runner writes **nothing** (no `COMPLETE`, no `FAILED`), checks that the row really is terminal, releases the job's lock, and emits the usual lifecycle event for that terminal status (`complete`, `failed` or `cancelled`) with the message `settled by handler`. A row that is *not* terminal is a handler bug: the job is failed with the typed error `handler_unsettled` (`HANDLER_UNSETTLED`) instead of being left running with no owner. A row in a held status counts as `Pending`.
- `Pending(reason)`: the outcome waits for something outside. The runner writes nothing, releases the lock, emits the usual `paused` event with the message `pending: <reason>` and returns `RunOutcome::Paused`. A held job waits for `resolve`. A job that is not held stays recoverable, so the next `resume_recoverable` runs the handler again (the polling-resume pattern); a `Recovery::Never` job that is not held cannot be `Pending` and is failed with `handler_unsettled`, because recovery would otherwise fail it as `interrupted`.
- Failure stays `Err(JobError)`: a retryable error pauses, any other fails. An error returned after the handler settled the job changes nothing (the row is terminal; the runner's write is skipped).
- The staged pool carries the same outcomes: `Staged::Handled(HandlerOutcome)` from `prepare`, and `StagedHandler::compute_outcome` (default: `compute` read as `Done`) from the lane.

## Settle later

A terminal write can fail on storage (database busy, I/O error, disk full: error code `db` or `io`). The runner no longer loses the outcome or turns it into a different state:

- The intended terminal transition (and the lifecycle event that announces it) is kept in memory **with the job's lock held**, so no other runner in this process starts the finished job again. `Runner::run` returns `RunOutcome::Unsettled(job)`, where `job` is the row as it will read once written; the stored row stays non-terminal until then.
- It is retried, with no timer and no sleep, when the store next completes a write (`submit`, a transition, `begin_run`, an event, `cancel`, ...) and on `JobStore::flush_pending_settlements()` (also on `Runner` and `StagedPool`). A flush stops at the first storage error. A retry that lands writes the state and event in one transaction, drops the lock, removes the journal file and delivers the event to the runner's sink, exactly once.
- The set is bounded by `MAX_PENDING_SETTLEMENTS` (64) entries and `MAX_PENDING_SETTLEMENT_BYTES` (4 MiB of JSON). Past either bound a failing write is not held: its error is returned, as before.
- Only storage errors (`db`, `io`) are held. A hook refusal (a spend cap), `stale_attempt`, `illegal_transition` and the like are returned at once and nothing is kept.
- **Who wins.** An explicit owner settle (`JobStore::settle_terminal` / `_fenced`, `JobControl::settle`) is never discarded in favour of an earlier outcome. While the row is still non-terminal it **replaces** the write the runner (or an earlier settle) held for the job: the new transition is kept whole, extension values included, the journal file is rewritten, the claim and sink stay, and a lifecycle event the held write carried becomes one for the new status. The write is tried at once; if storage takes it, the stale held write is dropped. The runner's own write never replaces a held one. Once the terminal row is stored, nothing is merged: the same status is idempotent (`Written(None)`), a different one fails with `illegal_transition`. The owner announces its own outcome when nothing was held.
- **Restored rows.** When `Runner::run` finds a write held from the journal and it lands (or the row is already terminal), the handler is not run and the result is `RunOutcome::Restored(JobRecord)`: the stored job and every extension value, including those the held write restored, so a caller holding older state does not write over them.
- A handler holds its own terminal write the same way: `JobControl::settle(&Transition)` (or `JobStore::settle_terminal` / `settle_terminal_fenced`) returns `Settlement::Deferred` instead of an error when storage refuses it. The handler returns `HandlerOutcome::Settled`; the runner takes over the lock and writes nothing else.
- `Runner::with_settle_later(false)` turns it off for that runner: the error is returned and the job stays `running` and recoverable, as in 0.2.9.

Restart. A held write is also written to a small journal, one file per job, in `<database file>.settlements/` next to the database, with `rightkit-fs`'s atomic write (temp file, `fsync`, rename) and owner-only permissions. It does not depend on the table that refused the write. Opening the store reads the journal and writes each recorded transition, so a job whose handler finished is **settled the way the handler left it instead of being run again**. A journal file that cannot be read (permission error, sharing violation while another process replaces it, I/O error) is left in place, not set aside: `pending_settlements()` lists it with an empty `status` and the read error, `shutdown_detailed` reports it as unsettled, and `flush_pending_settlements()` reads it again. Only a file that was read and fails to parse or validate is renamed to `.json.bad`. Until the file reads, the store does not know its outcome, so a runner can still recover that job as unfinished. If that write is still refused at open, the entry comes back pending and unclaimed, and by default (`UnsettledRecovery::Settle`) `resume_recoverable`, `recoverable_ids` and `run` leave the job alone (`run` returns `Unsettled`). `Runner::with_unsettled_recovery(UnsettledRecovery::Rerun)` discards such an entry and recovers the job as unfinished (re-run the handler, or fail a `Recovery::Never` job as `interrupted`); choose it only for idempotent handlers.

Shutdown. `JobStore::shutdown_detailed()` (also `Runner::shutdown_detailed` and `StagedPool::shutdown_detailed`, which stops the pool first) makes one last flush and returns a `SettlementShutdown { settled, dropped, unsettled }`: the jobs that finished but could not be stored ("finished, unsettled"), each with its intended status, `durable` (in the journal: applied at the next open) and the last error. `StagedPool::shutdown` and its drop flush too. This is a different object from `EngineSupervisor::shutdown_detailed`, which reports the child process.

Limits. When nothing durable can be written at all (the journal directory fails too, for example a full disk), the entry is memory-only (`durable == false`): a crash loses it and the next open sees an unfinished job, which recovery re-runs (or fails as `interrupted` for `Recovery::Never`). `shutdown_detailed` is the only warning in that case. The in-process lock stops duplicate runs in this process; another process sharing the database file sees the job as `running` with a free lock only after this process exits. Another process that opens the same file replays the journal too: writing a terminal state that is already there is harmless (the later write finds the job terminal and is dropped). Prune's announce guard covers the rows a runner terminalises between the start of `Runner::run` and its return, not a `cancel` or `complete` an app makes itself, and the guard and the held set live in the process that runs the job.

## Delete, keep-total, rejected submissions and recovering open

- `JobStore::delete(job_id) -> PruneReport`. A non-terminal job is cancelled first (the hook sees `cancelled`), then its rows are removed in one transaction and its cancel token is set. It does not wait for the handler to stop: a running handler sees the cancel at its next check, and a late write it makes is refused. With `StoreConfig::artifact_root` / `scratch_root` configured, the job's artifact files and `<scratch_root>/<job_id>` are removed too (inside those roots only, symlinks never followed, a path outside the roots fails with `path_outside_root`); `delete_with(id, &DeleteOptions { keep_files: true })` keeps them and returns a `DeleteReport`. The order is crash-safe (mark in `rk_job_deleting`, files, rows); call `finish_deletions()` at startup to finish an interrupted delete. With no root configured, files are left and the caller removes `report.artifacts[..].path`.
- `PrunePolicy::keep_total(n)` keeps the `n` newest **terminal** rows. Active jobs neither count toward `n` nor are ever removed by it, so a table with more than `n` active jobs still keeps its newest `n` finished rows (the first completion is not pruned the moment it lands). This replaces the whole-table meaning the unpublished 0.2.9 text gave it, and is now the same bound as `keep_latest(n)`. A terminal row whose runner has not finished announcing it (the lifecycle event and the sink call of `Runner::run`) is skipped by every prune until it has; the guard lives in the process that runs the job.
- `JobStore::submit_rejected(&NewJob, &Ext, &JobError) -> JobRecord`. The job is inserted, `on_create` runs, and the job moves to `failed` with the error and a `rejected` event, all in one transaction. It never runs, and a crash cannot leave it runnable. The app's entitlement check stays in the app; call this when the app refuses.
- `JobStore::open_recovering(&DbOptions, StoreConfig, RecoveryPolicy) -> (JobStore, Option<PathBuf>)`. If the database file is unreadable and the policy allows it, the file is moved aside and a fresh store is created. The second value is where the old file went; the original bytes are kept under the `.corrupt-` name.
- `JobStore::open_recovering_unreadable(&DbOptions, StoreConfig, RecoveryPolicy, UnreadablePolicy) -> (JobStore, OpenRecovery)` adds the opt-in policy for a database that cannot be *read* (permission denied, I/O read failure), which is not corruption. The default `UnreadablePolicy::Fail` fails with `db_unreadable` and touches nothing; `QuarantineAfter { attempts }` sets the file aside only after that many consecutive failed opens. A full disk, read-only filesystem, dropped share, lock or missing file never counts. `OpenRecovery { quarantined, action }` says what happened. See `rightkit_job_queue::db` below.

## Paging a long listing

`list` and `list_records` return one page, at most 1,000 rows. `list_page` walks the whole listing, newest first:

```rust
let filter = ListFilter { limit: Some(200), ..ListFilter::default() };
let mut after: Option<Cursor> = None;
loop {
    let page = store.list_page(&filter, after.as_ref())?;
    for rec in &page.records { /* ... */ }
    match page.next {
        Some(cursor) => after = Some(cursor),
        None => break,
    }
}
```

- **Keyset, not offset.** A page ends with the key of its last row, `(created_at, rowid)`, and the next page is every row strictly below it. Both are written once, so concurrent writes cannot move a row in the walk: a row that exists throughout and does not change is returned exactly once; one inserted, changed or deleted meanwhile at most once; a row inserted after the walk began sorts above it and is not part of it. A cursor survives deletion of the row it was cut from.
- **Cost.** `rk_jobs_created(created_at)` and `rk_jobs_status_created(status, created_at)` are created at open (additive, `IF NOT EXISTS`), so a page is an index range seek at any depth. The old listing scanned and sorted the whole table on every call.
- **`Cursor` is an opaque string** (`as_str`, `Display`, `Cursor::parse`, `FromStr`, and a plain JSON string under serde) that an app can give its UI and take back. It is tied to the filter it was issued for: a different `status` or `request_id` fails with `cursor_mismatch`, a malformed or cut-off one with `invalid_cursor`. The page size may change between pages.
- `ListFilter` is unchanged (it has no cursor field because apps build it as a full literal); `list` and `list_records` are the first page, and `truncated` is `next.is_some()`.

## Importing finished jobs

A legacy import that submits a job and then moves it to its final state makes two commits, and a crash between them leaves a runnable row. `import_terminal` inserts the finished job in one:

```rust
let rec = ImportRecord::complete("scan", json!({ "pages": 3 }))
    .id(&legacy.id)                       // or leave it out and key on request_id
    .request_id(&legacy.id)
    .request(legacy.payload.clone())
    .created_at("2025-11-02T09:14:00Z")   // YYYY-MM-DDTHH:MM:SSZ, UTC
    .updated_at("2025-11-02T09:15:30Z")
    .set("info", legacy_info_json)        // extension columns, checked against StoreConfig
    .artifact(artifact);
let out = store.import_terminal_with(&rec, &ImportOptions::new().idempotent())?;
// out.existed: an identical job was already there, so this was a re-run
```

- `ImportRecord::{complete, failed, cancelled, new}` and chained setters; a status other than `complete`, `failed` or `cancelled` is refused with `bad_input`, as is any invalid field.
- An id (or request id) already stored fails with `job_exists` and writes nothing. With `ImportOptions::idempotent()` an identical stored record is success (`existed: true`) and a different one is still `job_exists`, naming the first field that differs.
- `import_terminal_batch(&[ImportRecord], &ImportOptions)` takes up to `MAX_IMPORT_BATCH` (1,000) records, oldest first, in one transaction: every record is validated, then all are written or none. Larger imports are several batches; make them idempotent to re-run an interrupted one.
- The job is never queued or run: there is no queue in the store, `non_terminal_ids` and recovery do not list it, no event is written (no `started`), the attempt generation is 0. `StoreHook::on_create`, `on_attempt` and `on_transition` are not called; `StoreHook::on_import` is, in the same transaction.

## Classifying a failed open

`JobError::open_failure() -> Option<OpenFailure>` says what kind of database failure an error is, built from the underlying `rightkit_job_queue::db::DbError` and never from the message; the string `code` (`db`, `db_unreadable`) is unchanged and stays the wire identifier. It is set on the errors of `open`, `open_with`, `open_with_config`, `open_recovering` and `open_recovering_unreadable`, and on any other error converted from a database error; it is `None` for an error that never touched the database (a refused configuration, a handler error).

- `OpenFailure::Corrupt`: not a database, corrupt, or failed the open-time integrity check. `RecoveryPolicy` can set such a file aside.
- `OpenFailure::Unreadable(cause)`: permission denied or an I/O read failure (`UnreadableCause::PermissionDenied` or `IoReadFailure`).
- `OpenFailure::StorageUnavailable(problem)`: disk full, read-only filesystem, a dropped share, a lock held elsewhere, a missing file (`StorageProblem`). The file may be healthy.
- `OpenFailure::Other`: any other database failure.

Equality of `JobError` ignores the classification.

## Opening without a `rightkit-db` dependency

`rightkit_job_queue::db` re-exports, as the same types, the `rightkit-db` types in this crate's public API: `DbOptions` and what its fields are made of (`CacheProfile`, `DurabilityProfile`, `IntegrityCheck`, `JournalSelect`, `ConnInit`), the policies `open_recovering` and `open_recovering_unreadable` take (`RecoveryPolicy` with `QuarantineStamp` and `SidecarPolicy`, `UnreadablePolicy`), what the latter reports (`RecoveryAction` and the `UnreadableCause` and `StorageProblem` behind a refusal), and `DbError`, which converts into `JobError`. `RecoveryPolicy` and `rusqlite` are also at the crate root. An app that only opens a store needs no `rightkit-db` entry in its own `Cargo.toml`, and no version to keep matching this crate's.

## Per-job resume and sweep

- `Runner::resume_recoverable_each() -> Vec<(String, JobResult<RunOutcome>)>` attempts every recoverable job, oldest first. Each keeps its own result: one failing job does not stop the rest, and a job that cannot be examined is reported by id. `Runner::resume_recoverable` is unchanged and stops at the first error.
- `Runner::sweep_unclaimed(root) -> Vec<PathBuf>` removes child directories of `root` that contain no path claimed by a non-terminal job (`JobStore::claimed_paths`). Plain files are left alone and a symlink is never followed out of `root`. Call it only when this process is the sole engine for `root`. Not calling it is the off switch.
- **Claim scope.** `sweep_unclaimed` and `claimed_paths` claim the artifacts of **non-terminal** jobs only, so a finished job that keeps its media directory until it is deleted loses it to a sweep. `JobStore::claimed_paths_with(ClaimScope)` and `Runner::sweep_unclaimed_with(root, ClaimScope)` choose: `ClaimScope::NonTerminal` is the existing behaviour (and the default); `ClaimScope::All` claims the artifact paths of every job row still in the store, whatever its status, plus `<scratch_root>/<job_id>` of every row when `StoreConfig::scratch_root` is set. A row that was deleted or pruned claims nothing, so its directory becomes an orphan again.
- **Work directories (0.3.2).** `StoreConfig::work_root(root)` names a directory of per-request work directories, `<root>/<request_id>`. `claimed_paths_with` claims them (non-terminal jobs for `NonTerminal`, every job for `All`) and `delete` removes the job's directory with it.
- **Lock files (0.3.2).** The runner removes `<id>.lock` when the job is terminal. `JobLocks::remove`, `JobLocks::sweep(keep)` and `Runner::sweep_job_locks()` clear files left by crashes or earlier versions (call them only for finished or gone jobs).
- **Write-behind (0.3.2).** `JobStore::admit` (a new job), `set_ext_later` (a checkpoint in extension columns) and `delete_later` hold a write that storage refuses (`db` or `io`) and retry it in order after the next successful write and on `flush_deferred()`. Held writes are memory only and bounded by `MAX_DEFERRED_OPS`; `shutdown_detailed()` flushes them and lists what is left in `SettlementShutdown::deferred`. A held admission is not a row: `get` says `job_not_found` until it lands (`provisional_jobs()` lists it).
- **Caller connection, named table, due query, revision (unreleased).** `JobTable` (`JobStore::table()`, or `JobTable::new(&cfg)`) runs enqueue, transition, complete, fail, hold_submission, resolve and reads on a `&Connection` or `&Transaction` you supply, so job writes commit with your own rows and roll back with them. `StoreConfig::jobs_table(name)` names the table, `unique(cols)` adds UNIQUE constraints, `due_column(col)` enables `due(now, limit)`, and `row_revision(col)` with `Expect::new().revision(n)` gives a compare-and-swap that fails with `revision_conflict`.

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
- **Transactional hook.** `StoreHook::{on_create, on_attempt, on_transition}` (and `on_resolve`, `on_import`) run in the same write transaction as the change. An error rolls it back, for example a spend cap. `app_schema`, `write` and `read` serve app-owned tables on the same connection.
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

`tests/dispatcher.rs`, `tests/e2e.rs`, `tests/engine_contract.rs`, `tests/fencing.rs`, `tests/genright.rs`, `tests/job_lock.rs`, `tests/supervisor.rs`, `tests/v021.rs`, `tests/prune.rs`, `tests/delete_reject.rs`, `tests/delete_files.rs`, `tests/held.rs`, `tests/resume_sweep.rs`, `tests/pool.rs`, `tests/paging.rs`, `tests/import_terminal.rs`, `tests/reexport.rs`, `tests/handler_outcome.rs`, `tests/settle_later.rs`, `tests/terminal_announce.rs`, `tests/claim_scope.rs`, `tests/open_failure.rs`, `tests/settle_owner.rs`. The `examples/` directory has `job_engine.rs` (the test engine) and `engine_fixture.rs`.

## Platform support

The crate depends on `rightkit-child-process` and `rightkit-daemon`, which have their own per-OS behaviour. `src/` has no `cfg(unix)` or `cfg(windows)` code; the job lock comes from `rightkit_fs::FileLock` (POSIX record lock on Unix, `LockFileEx` elsewhere, `src/runner.rs:7` and `src/runner.rs:38`), and only some tests are `cfg(unix)` (`tests/job_lock_process.rs:1`). Windows behaviour is not otherwise documented in this crate.

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

## Discovered engine doctor probes

Enable `doctor` (also included by `supervisor`). Pass an absolute engine path;
probes never search PATH. Each `probe` runs `<engine> doctor` live. Exit 1 is valid
when stdout contains valid doctor JSON, preserving not-ready engine reports.

```rust,no_run
use rightkit_job_queue::{DoctorProbe, DEFAULT_DOCTOR_TIMEOUT, run_doctor};

let engine = std::env::current_exe().unwrap(); // replace with discovered engine path
let probe = DoctorProbe::default(); // retain this instance across discovery calls
let doctor = probe.probe(&engine)?;
let capabilities = probe.cached_capabilities(&engine);
let uncached = run_doctor(&engine, DEFAULT_DOCTOR_TIMEOUT)?;
# Ok::<(), rightkit_job_queue::JobError>(())
```

`parse_doctor` extracts `app_version` (string), `contract_version` (unsigned integer),
`capabilities` (array) & `readiness` (any JSON, null when absent). Other fields are
ignored. Capability entries stay engine-defined. `DoctorOptions` configures runtime
& stdout byte limits; stdout is byte-exact, drained past its cap & rejected if
truncated. Default timeout is 10 seconds; output ceiling is 8 MiB. Runtime budget
covers stdout EOF as well as child exit, so inherited pipes cannot extend it.
Timeout cleanup kills owned process trees with at most 250 ms additional reap wait;
remaining reaping continues in background. OS spawn/stat calls retain OS latency.

`DoctorProbe` caches only capabilities for unchanged binary identities: canonical
path, size, mtime, creation time when available, plus Unix device/inode/ctime.
Symlink aliases share entries. Probes compare identity before & after execution;
replacement during probing prevents caching. Missing or changed binaries miss.
Metadata identity detects ordinary upgrades; it is not content authentication.
Cache holds at most 128 binaries, removes older versions of each path & evicts
oldest entries. `clear_cache()` discards entries; concurrent live probes may add
entries afterward. Readiness & errors are never cached.

Failures return `JobError` with supervisor origin & codes `bad_input` (relative
path), `spawn_failed`, `wait_failed`, `doctor_timeout`, `doctor_read_failed`,
`doctor_output_limit`, `doctor_bad_json` or `doctor_bad_shape`.

## Inline cancellation without persisted jobs

This API needs no Cargo feature or database. Retain one registry across engine
handlers; clones share registrations. Keep guard alive throughout work.

```rust
use rightkit_job_queue::InlineCancelRegistry;
use std::sync::atomic::Ordering;

let registry = InlineCancelRegistry::new();
let (guard, cancel) = registry.register("render-42")?;
assert!(registry.clone().cancel("render-42"));
assert!(cancel.load(Ordering::SeqCst));
drop(guard);
assert!(!registry.cancel("render-42"));
# Ok::<(), rightkit_job_queue::JobError>(())
```

Registration rejects empty IDs (`bad_input`) & active duplicates (`job_exists`).
Guard drop removes registration, including during unwind. IDs can then be reused
with fresh flags. `cancel(id)` returns whether work is registered, including
already-cancelled work. `cancel_all()` flags current work & returns its count;
future registrations start uncancelled. Cancellation is cooperative: handlers
must observe returned `Arc<AtomicBool>`; guard drop never resets flags retained
by work. Registries do not persist, recover, fence or terminate jobs.

### Activity listing (0.4.1)

The same registry doubles as an activity registry for a UI that shows what runs.
`register_with(id, ActivityOptions::new().label("Export").progress(0.0))` sets a
label, message and progress (and may pass the caller's own cancel flag); the guard
updates them (`set_progress`, `set_message`, `set_label`, `report`). `list()`
returns an `ActivitySnapshot` per registration in registration order, `get(id)`
one, and `revision()` rises on every change so a poller re-lists only when it
moved. Still process-local and unpersisted.

## Cross-process lock order, and unsolicited events (0.4.1)

`JobLocks::new(dir).with_fifo_scope(FifoScope::CrossProcess)` serves `acquire`
waiters in arrival order across processes (the default `FifoScope::Process` orders
only waiters sharing one `JobLocks`). Every process that waits on the directory
must opt in; `try_lock` never joins the line. Waiters keep ticket files under
`<dir>/<job id>.queue/`; a crashed waiter's ticket is swept by the next waiter.

`Dispatcher::event_emitter()` returns an `EventEmitter`; `emit(json)` sends an
unsolicited `{"type":"event","event":..}` frame on the connection `serve` is
serving, from any thread. It errors with `not_serving` when none is served.
