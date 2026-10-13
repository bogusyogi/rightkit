# Changelog

## Unreleased

- Additive: `ExitObserver` callback and `SupervisorConfig::with_exit_observer`; `EngineSupervisor` reports abnormal engine exits (not shutdown) with the restart decision and count. Off-by-default `crash` feature adds `CrashExitObserver`, forwarding to `rightkit_crash::record_helper_exit`.

### M3/M4 owner gaps, additive; written without running Cargo, CI-only

Locked M3/M4 design (READINESS "LEGION CHOSEN M3/M4 DESIGN"), implemented one design point at a time. All additions are opt-in; no default changes.

- **Restart policy (M3).** The supervisor reports each generation that reaches `Ready` to `RestartTracker::ready`, so `rightkit-process`'s new `RestartPolicy::with_consecutive_failures(cap, cooldown)` and `with_reset_budget_on_ready()` take effect: a replacement that fails before ready counts toward the cap, a ready generation resets it. Policies built without those calls behave as before. Tests: `tests/m3_supervision.rs` (cap ends a never-ready run before the rolling budget; a crash after ready does not count).
- **Contract readiness and snapshots (M3).** `SupervisorConfig::require_current_contract_version()` (shorthand for `require_contract_version(CONTRACT_VERSION)`) and `with_mandatory_contract_check()`: with the latter `start`/`start_background` refuse to boot, spawning nothing, unless a version was configured (`contract_check_required`, not retryable). Snapshots now carry a revision: a `set_snapshot` that arrives while a generation is replaying (after the replay list was copied) is replayed to that generation before it reports `Ready`; the final check and the switch to `Ready` happen under one lock. Before, that generation booted on the superseded value and only a later restart saw the update. Tests: `tests/m3_supervision.rs` (update during a slow replay reaches the engine; no-update replay is one call per snapshot; the gate refuses without a version, passes with the current one, and a wrong-version engine is still refused). Fixture gains `slow_set:<ms>`, `env` and `slow_progress`.
- **Child environment (M3).** `SupervisorConfig::require_scrubbed_env()`: a spawn closure whose command did not clear the inherited environment (`OwnedCommand::env_allowlist` / `env_allowlist_with_defaults`) fails the boot with `env_not_scrubbed`, terminal, nothing spawned. Off by default; the default still inherits. Tests: `tests/m3_supervision.rs` (engine sees only the allowlist plus explicit variables; unscrubbed command refused; inherited without the requirement).
- **Deadlines (M3).** The default is unchanged: one absolute deadline per call. New opt-in `EngineSupervisor::call_with_progress_deadline` / `EngineClient::call_progress_frames` take a `ProgressDeadline { idle, max_total }`: the call times out after `idle` without an event tagged with *its own* request id (unsolicited events and other requests' events never count), and `max_total`, when set, still caps it. Replaces ScrapeRight's app-side inactivity reset for calls that report their own progress. Tests: `tests/m3_supervision.rs` (own progress extends it; the same call misses an absolute deadline; silence and `max_total` time out; unrelated chatter does not extend it). Fixture gains `chatter`.
- **Write ACK, response timeout, bounded stop and reap (M3; two design points, one change).**
  - A request write that does not complete within the call's deadline kills the engine that is not reading, as before, but the kill is now bounded (`EngineClient::set_kill_bound`, default 2 s; the supervisor uses `shutdown_grace`) instead of a blocking `terminate_tree`, and the call fails with the new code `DELIVERY_UNKNOWN` (`delivery_unknown`, not retryable) instead of `timeout`: the engine may have received none, part or all of the request. `keep_alive_on_timeout` still applies only after the write succeeded (a response timeout stays `timeout`, retryable, engine kept). The lifecycle records the exit as `rpc_timeout_kill`. This changes the code of a stalled-write failure from `timeout` to `delivery_unknown`; a caller that matched `timeout` for that case must also match the new code.
  - A killed child the OS has not reported gone within its bound is no longer recorded or reported as reaped. It moves to a reaper thread owned by the client, `ChildExit::reaped` stays false and `ShutdownReport` says `Unconfirmed` until the reaper has waited it; the supervisor's unreaped count and the live-token release follow the real wait. (Before, `retire` recorded `reaped: true` after a bounded wait that had timed out, and the stalled-write path never released its token.) Pulse's leak-then-poison pattern is not needed: the child is retained and counted, never leaked.
  - `SupervisorConfig::with_max_unreaped_children(n)` (default 4): a boot is refused (`unreaped_children`, spends restart budget) while more than `n` engine processes are unconfirmed, so a stuck child cannot accumulate replacements.
  - Tests: `tests/m3_supervision.rs` (stalled write is bounded, reports `delivery_unknown`, the engine is replaced and reaped; response timeout after a write keeps the same engine and stays `timeout`; a zero-grace kill ends Killed or Unconfirmed and is reaped later, no zombie), unit tests in `src/supervisor.rs` (cap refuses at more than `n`, allows `n`). Fixture gains `deaf`. The OS-never-reports-gone case cannot be produced from a test; the cap is tested at the counter.

### a paused job stays paused; an unlistable journal fails the open

Two fixes from a consumer's independent read of the runner and the journal (ScrapeRight, 2026-10-12). Nothing here was compiled or run locally; verification is the CI run.

- **A retryable handler error keeps the job paused when its bookkeeping write is refused.** The runner recorded the error text (and, for a host hand-off, refunded the attempt) with `?`, so a storage error on that write (disk full, database locked) replaced `RunOutcome::Paused` with an `Err`, and a caller treated a resumable job as a hard failure. A storage error on the refund, on the diagnostic write or on the read-back is now tolerated and the run returns `Paused` with the job as it was read at the start; any other error (a lost fence) still ends the run.
- **A settlement journal that exists and cannot be listed fails `JobStore::open`.** `replay_journal` returned silently when the directory could not be read and skipped entries whose listing failed, so after a crash a job with a durable Done or Cancelled in the journal was not held and recovery could run it again. Only a missing directory means "no journal" now; any other listing error is returned (code `io`) and the store does not open. This is a behaviour change: an application that ignored an unreadable `<db>.settlements` directory now gets an error at open.

### settle-later: an owner settle cannot lose to a concurrent flush or runner

Fix, found by ScrapeRight's review of `store/settle.rs` at snapshot c9f222a1. Nothing here was compiled or run locally; verification is the CI run. No signature or field changed.

- **Owner settle against a flush.** A flush copied a held write, released the pending lock, committed the copy and removed the entry by job id. An owner settle that replaced the held outcome in between was told `Deferred`, the flush then stored the replaced outcome and removed the owner's entry and its journal file. An owner settle now holds the flush lock from before it looks at the held write until it has committed or is held (released before any sink runs), so the two never interleave: the flush either sees the owner's outcome, or has finished and the owner is told the row is already terminal (`illegal_transition`, or idempotent for the same status). Test: `tests/settle_flush_order.rs`.
- **Owner settle against the runner.** `upsert_entry` released the pending lock between finding no held write and inserting, so a runner holding its own outcome in the gap won and the owner was still told `Deferred`. Look and insert are now one step under the lock. No deterministic test: the gap cannot be opened from outside the crate.
- **Second pass, from an independent read of the fix above (`docs/audits/2026-10-12-astra-jobs-settle-9177feaa.md`).** The runner's settle takes the flush lock too, from its look at the held writes to its commit or hold: without it an owner whose write storage refused could hold its outcome in that window, and the runner's commit, then the flush after it, dropped the owner's accepted outcome. `discard_unclaimed_settlement` runs under the flush lock, so a flush cannot commit an entry after it was discarded, and a discard cannot remove the journal file of an owner settle held meanwhile. Lock order is flush, pending, connection, everywhere.
- Known and not fixed here (ledger 207): two stores or two processes replaying one journal directory are not coordinated; a failed journal rewrite followed by a failed removal of the old file can replay the replaced outcome after a crash; a runner that adopts a held write and then attaches its event can attach it to an owner's replacement; a busy database stalls every settle of that store, because the flush lock is held across the database wait.

### settle-later journal: an unreadable file is not a bad file

Fix. Nothing here was compiled or run locally; verification is the CI run. No signature or field changed.

- Replaying the settle-later journal at open no longer sets a file aside because it could not be read. Before, any read error renamed `<job>.json` to `<job>.json.bad`, which hid a finished job's terminal outcome behind a transient refusal (on Windows, a file another process is replacing can be refused for an instant). Now only a file that was read and fails to parse or validate is renamed. A file that cannot be read stays where it is, is listed by `pending_settlements()` (and `shutdown_detailed().unsettled`, so `is_clean()` is false) with an empty `status` and the read error, and is read again, once and without a sleep, by each explicit `flush_pending_settlements()`; a file that reads then is held and written like any other, and a file that has vanished is forgotten. A file that has vanished between the listing and the read is skipped, as before.
- Limit: until the file reads, the store does not know the outcome, so a runner cannot be told to leave that job alone; `UnsettledRecovery` applies only to entries that were read.
- Test: `tests/settle_unreadable_journal.rs` (a directory at `<job>.json` stands in for a read error that is not `NotFound` on every platform).

### handler-owned outcomes, settle-later, claim scopes, open-failure classification, terminal-only keep_total

Nothing here was compiled or run locally; verification is the CI run. Four additions and one fix, written for an adopter whose handlers settle jobs themselves. Behaviour changes for existing callers are listed first.

**Behaviour changes (read these).**

- **`PrunePolicy::keep_total(n)` now counts and removes terminal rows only.** It keeps the `n` newest terminal rows; active jobs neither count toward `n` nor are removed by it. The 0.2.9 text bounded the whole table (`n` minus the active count), so with 201 active rows and `keep_total(200)` the first completion was pruned the moment it landed. 0.2.9 is not published, and no GenRight, PostRight, CutRight or ScrapeRight call of `keep_total` exists in the checkouts; the bound is now the same as `keep_latest(n)`. `tests/prune.rs` changed with it.
- **A terminal write that fails on storage no longer ends `Runner::run` with `Err`.** With a `db` or `io` error the runner now holds the write (see Settle later) and returns the new `RunOutcome::Unsettled(job)`; the job's lock stays held instead of being released. `Runner::with_settle_later(false)` restores the 0.2.9 behaviour (the error, the job left `running` and recoverable). Errors that are not storage errors (a hook refusal, `stale_attempt`, `illegal_transition`) are returned as before.
- **New public enum variants.** `RunOutcome::Unsettled(Job)` and `Staged::Handled(HandlerOutcome)`. An exhaustive `match` on either needs a new arm; the compiler says so. No GenRight, PostRight or CutRight source matches on either.
- **`JobError` gains a private field** (`open_failure()` reads it). Equality still compares `code`, `message`, `retryable` and `origin` only. Code that built a `JobError` as a struct literal outside the crate must use `JobError::new`, `retryable`, `supervisor` and `with_origin` (the 0.2.8 note already said literals need the new `origin` field). No such literal exists in the four apps.
- **The runner writes its terminal lifecycle event in the same transaction as the terminal state** (`complete`, `failed`, `cancelled` after a failed write, `interrupted`), and `RunOutcome::Finished` carries the row that transaction wrote. Before, the event was a second commit and `Finished` a re-read, so a row removed in between lost both. The event rows, their order and their text are the same.
- **`prune` and `submit` with `SubmitOptions::prune` skip a terminal row whose runner is still announcing it** (between its terminal write and the end of `Runner::run`, including the sink call). The guard lives in the process that runs the job.
- **Opening a store reads `<database file>.settlements/` when it exists.** The directory is created only when a terminal write is held, so a store that never held one is untouched.

**Trial-build fixes (settle-later).**

- **An explicit owner settle replaces a held write.** `JobStore::settle_terminal` / `settle_terminal_fenced` and `JobControl::settle` used to leave the first held transition standing (merging only the claim and sink), so a generic `FAILED` the runner had journalled beat a later, more specific `Cancelled` with extension values. Now, while the row is non-terminal, the explicit settle replaces the held write, extension values and fence included, rewrites the journal file (removing it if the rewrite fails), keeps the claim and sink, and swaps a carried lifecycle event for one for the new status (`settled by owner`). It is tried at once and the stale held write is dropped when storage accepts it. The runner's own write still never replaces a held one. Once the terminal row is stored, a settle to a different status now fails with `illegal_transition` (before: silently `Written(None)`), and the same status stays idempotent.
- **A replay inside a run returns the restored row.** New `RunOutcome::Restored(JobRecord)`: when `Runner::run` finds a held write from the journal and it lands (or the row is already terminal) it returns the stored record with its extension values instead of `Finished(Job)`. This is a new enum variant (exhaustive matches need an arm); `RunOutcome::job()` and `to_value()` cover it.
- Tests: `tests/settle_owner.rs`.

**Handler-owned outcomes.**

- New `HandlerOutcome` (`#[non_exhaustive]`): `Done(Value)`, `Settled`, `Pending(String)`. New provided `JobHandler::run_outcome(&self, &Job, &Arc<JobControl>) -> JobResult<HandlerOutcome>`; its default is `run_shared` with `Ok(v)` read as `Done(v)`, so a handler that implements only `run` behaves as before. `Runner` calls `run_outcome`.
- `Settled`: the handler already wrote the terminal state. The runner writes nothing (no `COMPLETE`, no `FAILED`), verifies the row is terminal, releases the lock and emits the usual terminal lifecycle event with the message `settled by handler`. A row that is not terminal is a handler bug: the job is failed with the new typed error `handler_unsettled` (`HANDLER_UNSETTLED`); a row in a held status counts as `Pending`.
- `Pending(reason)`: the runner writes nothing, releases the lock, emits `paused` with `pending: <reason>` and returns `RunOutcome::Paused`. A `Recovery::Never` job that is not held cannot be `Pending` (recovery would fail it as `interrupted`): it is failed with `handler_unsettled`.
- `Staged::Handled(HandlerOutcome)` from `StagedHandler::prepare`, and provided `StagedHandler::compute_outcome` (default: `compute` read as `Done`); the pool adapter implements `run_outcome`, and its `run_shared` refuses a non-`Done` outcome with `pool_misuse`.
- The `hold_submission` / `resolve` API already expressed "pending" for a held job and is reused; `Pending` adds the case of a job that is not held.

**Settle later.**

- A terminal write refused with a `db` or `io` error is kept in memory with the job's lock held, retried when the store next completes a write and on `JobStore::flush_pending_settlements() -> FlushReport { settled, dropped, pending }` (also `Runner`), with no timer and no sleep. A flush stops at the first storage error. The retry writes the state and the lifecycle event in one transaction and delivers the event to the runner's sink once.
- Bounded: `MAX_PENDING_SETTLEMENTS` (64) entries and `MAX_PENDING_SETTLEMENT_BYTES` (4 MiB); past either, the error is returned. Opportunistic retries run immediately for an entry's first three tries and at most once a second after that; an explicit flush always tries.
- Handlers: `JobControl::settle(&Transition) -> JobResult<Settlement>` and `JobStore::settle_terminal` / `settle_terminal_fenced` write a terminal transition and return `Settlement::{Written, Deferred}`. After `Deferred` the handler returns `HandlerOutcome::Settled`; the runner takes over the claim and returns `Unsettled`.
- Restart: each held write is journaled, one file per job, in `<database file>.settlements/` with `rightkit_fs::atomic_write_with` (temp, `fsync`, rename, owner-only). `JobStore::open*` reads the journal and writes the recorded transitions, so a finished job is settled instead of run again. A still-refused entry stays pending and unclaimed; `Runner::with_unsettled_recovery`: `UnsettledRecovery::Settle` (default: recovery skips the job, `run` returns `Unsettled`) or `Rerun` (discard it and recover the job as unfinished).
- Shutdown: `JobStore::shutdown_detailed()`, `Runner::shutdown_detailed()` and `StagedPool::shutdown_detailed()` flush once and return `SettlementShutdown { settled, dropped, unsettled: Vec<UnsettledJob> }` ("finished, unsettled", with `durable`). `StagedPool::shutdown` and drop flush once too. `JobStore::pending_settlements()` lists the held writes at any time.
- Limit: if the journal cannot be written either, the entry is memory-only (`durable == false`) and a crash loses it; the next open sees an unfinished job.

**Claim scopes.** New `ClaimScope::{All, NonTerminal}` (`NonTerminal` is the default), `JobStore::claimed_paths_with(scope)` and `Runner::sweep_unclaimed_with(root, scope)`. `claimed_paths()` and `sweep_unclaimed(root)` are `NonTerminal` and unchanged. `All` adds the artifact paths of every job row still in the store, and `<scratch_root>/<job_id>` of each row when a scratch root is configured.

**Typed open failures.** New `JobError::open_failure() -> Option<OpenFailure>` with `OpenFailure::{Corrupt, Unreadable(UnreadableCause), StorageUnavailable(StorageProblem), Other}` (`#[non_exhaustive]`), derived from the `DbError` or SQLite error behind the `JobError`. `code` is unchanged. `Unreadable` holds only `PermissionDenied` or `IoReadFailure`; every storage cause is `StorageUnavailable`.

**Tests.** `tests/handler_outcome.rs` (no second write: row, event log and hook count; a typed handler bug for `Settled` on a non-terminal row; `Pending` plain, held and `Never`; the pool), `tests/settle_later.rs` (injected failing terminal write held, retried by flush and by the next write, bounded, claim held, shutdown report, restart applies the journal, parked and `Rerun` recovery, handler-deferred write, non-storage errors not held, opt-out), `tests/terminal_announce.rs` (201 active rows with `keep_total(200)`: one terminal event, row readable from the sink; a prune from inside the sink skips the row being announced), `tests/claim_scope.rs`, `tests/open_failure.rs`; unit tests for the classification in `src/error.rs`; `tests/prune.rs` updated for the terminal-only bound.

### keyset paging, atomic terminal import, rightkit-db re-exports

Additive. Nothing here was compiled or run locally; verification is the CI run. No existing signature, field, wire frame or `JOB_METHODS` entry changed; `ListFilter` gained no field (apps build it as a full literal).

- **Keyset paging.** New `JobStore::list_page(&ListFilter, Option<&Cursor>) -> ListPage { records, next: Option<Cursor> }`. A page ends with the key of its last row (`created_at`, `rowid`: the order `list` has always had, both written once at insert), and the next page is every row strictly below it: `WHERE (created_at, rowid) < (?, ?)`, no `OFFSET`, so a page costs the same at any depth and `list_records`'s 1,000-row cap no longer bounds a walk. The cursor is a position, not a row: it survives deletion of the row it was cut from, and under concurrent writes a row that exists for the whole walk and does not change is returned exactly once, a row inserted, changed or deleted meanwhile at most once. `Cursor` is an opaque string (`as_str`, `Display`, `Cursor::parse`, `FromStr`, serde as a plain string) bound to the filter and sort it was issued for: a malformed one fails with `invalid_cursor` (`INVALID_CURSOR`), one from a different `status`/`request_id` filter or sort with `cursor_mismatch` (`CURSOR_MISMATCH`). `limit` may change between pages. `list_records` and `list` are `list_page(f, None)` and return what they always did; `truncated` is `next.is_some()`. The listing has one sort order, newest first; the cursor carries a sort tag so a future order is refused rather than misread. Known limit: `rowid` is the tie-breaker (the table has a text primary key), and `VACUUM` may renumber such rowids; nothing in this crate vacuums.
- **Schema: two additive indexes, created at open.** `CREATE INDEX IF NOT EXISTS rk_jobs_created ON jobs(created_at)` and `rk_jobs_status_created ON jobs(status, created_at)` in the same `IF NOT EXISTS` batch as `jobs_status` (the crate has no schema-version number; `user_version` is the app's). Before this the listing query was a full scan plus a temporary sort for every call, because `(?1 IS NULL OR request_id=?1)` cannot use an index; the query now names only the filters that are set and SQLite seeks on `request_id`, on `status` plus the order, or on the order alone. A first open on a large existing table pays one index build per index. Older versions ignore the indexes. `Adoption::added_columns` is unchanged (indexes are not columns).
- **Atomic terminal import.** `JobStore::import_terminal(&ImportRecord) -> JobRecord`, `import_terminal_with(&ImportRecord, &ImportOptions) -> Imported { record, existed }`, `import_terminal_batch(&[ImportRecord], &ImportOptions) -> Vec<Imported>` (at most `MAX_IMPORT_BATCH` = 1000 records). One transaction inserts the job row directly in `complete`, `failed` or `cancelled` with its error, result, timestamps, payload, extension values and artifact rows; a non-terminal status is refused (`bad_input`) and so is every invalid field (an id that is not a single file-name-safe token, a timestamp not in `YYYY-MM-DDTHH:MM:SSZ`, a `failed` job without an error, a `complete` job with one, unknown, mistyped or missing required extension columns, a repeated or foreign artifact). An existing id, or a request id already in the table, fails with `job_exists` (`JOB_EXISTS`) and writes nothing. `ImportOptions::new().idempotent()` treats an identical stored record (every field the record states, including the extension columns it names, its artifacts and its timestamps when given) as success with `existed: true`, so an interrupted import can be run again; a different one is still `job_exists`, naming the first field that differs. A batch validates every record first and then commits all or none; its errors name the record by position and id. The job is never queued (there is no queue in the store; `non_terminal_ids` and the runner's recovery do not list it), no event is written (no `started`), the attempt generation is 0, and `StoreHook::on_create`, `on_attempt` and `on_transition` are not called. New provided `StoreHook::on_import(conn, &JobRecord)` runs in the same transaction (an error rolls back the whole import or batch). `ImportRecord` is `#[non_exhaustive]` with constructors `complete`, `failed`, `cancelled`, `new` and chained setters.
- **`rightkit_jobs::db`.** Re-exports, as the same types, the `rightkit-db` types in this crate's public API so a consumer needs no `rightkit-db` dependency: `DbOptions` and the types its fields are built from (`CacheProfile`, `DurabilityProfile`, `IntegrityCheck`, `JournalSelect`, `ConnInit`), `RecoveryPolicy` (still also at the root), `QuarantineStamp`, `SidecarPolicy`, `UnreadablePolicy`, `UnreadableCause`, `StorageProblem`, `RecoveryAction`, and `DbError`. `rightkit_jobs::rusqlite` was already re-exported.
- **`JobStore::open_recovering_unreadable(&DbOptions, StoreConfig, RecoveryPolicy, UnreadablePolicy) -> (JobStore, OpenRecovery { quarantined, action })`.** `open_recovering` plus the opt-in policy for a database that cannot be read (permission denied, I/O read failure). The default `UnreadablePolicy::Fail` fails with the new `db_unreadable` code (`DB_UNREADABLE`) and never touches the file; `QuarantineAfter { attempts }` moves it aside only after that many consecutive failed opens. A busy database is retried for up to 30 seconds, as in every open. `From<DbError> for JobError` maps `DbError::Unreadable` to `db_unreadable`; no existing open can produce that variant, so every existing error keeps its `db` code. **Needs the `rightkit-db` that ships `UnreadablePolicy`** (unreleased in the tree when written; this crate's `rightkit-db` requirement still reads `0.4.0`).
- **Tests:** `tests/paging.rs` (2,500 rows with inserts, deletes and status changes between pages; ties; filters; cursor errors), `tests/import_terminal.rs` (atomicity, idempotence, validation, batch bound, hook), `tests/reexport.rs` (names only `rightkit_jobs` paths); unit tests for the cursor codec, the query plan (no sort, an `rk_jobs_` index) and the error mapping.

### owned delete cleanup

Additive. Nothing here was compiled or run locally; verification is the CI run. A store with no roots configured behaves exactly as 0.2.9.

- **`delete` owns file cleanup.** New `StoreConfig::artifact_root(path)` (repeatable) and `StoreConfig::scratch_root(path)` (the `<root>/<job_id>` layout `Runner::sweep_orphan_scratch` uses); both must be absolute. When either is set, `JobStore::delete` also removes the job's artifact files and its scratch directory. Only paths strictly inside a configured root are touched, with symlinks in the parent path resolved first; a recorded path outside every root (or with `..`, or relative) fails the delete with `path_outside_root` and deletes nothing. A symlink artifact or scratch directory is unlinked, never followed. A file also recorded by another job is left for it (`DeleteReport::shared`). `<artifact_root>/<job_id>` is removed when empty.
- **New `JobStore::delete_with(job_id, &DeleteOptions { keep_files })` -> `DeleteReport`** (`prune`, `removed`, `already_gone`, `kept`, `shared`, `scratch_removed`). `keep_files: true` removes rows only. `delete` is `delete_with(default)` and still returns `PruneReport`.
- **Crash-safe order.** `begin_delete` (validate, cancel a live job, write the mark in new crate-owned table `rk_job_deleting`), files removed, `complete_delete` (rows and mark removed in one transaction). New `JobStore::pending_deletions()` and `JobStore::finish_deletions()`: the recovery pass; call it at startup. The job row stays visible until the last step. `prune` still never deletes files.
- **Schema:** `CREATE TABLE IF NOT EXISTS rk_job_deleting` at open (reserved name, `DELETING_TABLE`). No existing column changed.
- **Tests:** `tests/delete_files.rs`.

## 0.3.0 (Breaking: `default-features = false` no longer includes the store)

Version bump for the feature split (7775d90f), which changed what `default-features = false` selects without a version change. Nothing here was compiled or run locally; verification is the CI run.

- **Breaking.** Before the split, a dependent that wrote `rightkit-jobs = { version = "0.2", default-features = false }` still got the persistent job layer (`JobStore`, `Runner`, `StagedPool`, `StoreConfig`, the `rusqlite`/`db` re-exports). The layer now sits behind the `store` feature, so that dependency loses those items and fails to compile. Migration: add `features = ["store"]` (`rightkit-jobs = { version = "0.3.0", default-features = false, features = ["store"] }`), or drop `default-features = false` to keep the default set (`store`, `service`, `supervisor`).
- Dependents that use the default features are unaffected.

## 0.2.9 (staged pool, delete and rejection, held jobs, per-job resume and sweep)

Additive only. No existing signature, wire frame, `JOB_METHODS` entry or schema column changed;
the new state uses the existing `jobs`, `rk_job_submissions` and events tables (UPDATE and INSERT
only, no `ALTER` or `CREATE`). A 0.2.8 database opens unchanged.

- **Staged pool (`pool` module).** `pool::StagedPool`, `pool::PoolConfig` (`new(limit)`,
  `handoff(n)`, `idle_after(d)`, `on_outcome(f)`), `pool::StagedHandler` (`new_lane`, `prepare`,
  `compute`, `idle`) and `pool::Staged` (`Done(Value)` or `Compute(P)`). Admission is FIFO and
  bounded by a limit closure read on every admission. Prepare runs on parallel threads; compute
  runs on one thread-affine lane through a bounded hand-off. Each job still runs through
  `Runner::run`, so locks, fencing and lifecycle are unchanged. Shutdown withdraws buffered work,
  which stays recoverable. Also exported at the root as `OutcomeFn`, `PoolConfig`, `Staged`,
  `StagedHandler`, `StagedPool`.
- **`JobHandler::run_shared(&self, job, ctl: &Arc<JobControl>)`** (provided method). `Runner`
  calls it; the default calls `run`, so existing handlers are unchanged. A handler that hands the
  control to another thread overrides it.
- **Delete and rejection.** `JobStore::delete(job_id) -> JobResult<PruneReport>`: a non-terminal
  job is cancelled first (hook sees `cancelled`), then its rows are removed in one transaction and
  its cancel token is set. Artifact files are not deleted. `JobStore::submit_rejected(&NewJob,
  &Ext, &JobError) -> JobResult<JobRecord>`: insert, `on_create`, move to `failed` with the error
  and a `rejected` event, in one transaction, so the job never runs.
- **Bounded total.** `PrunePolicy::keep_total(n)`: bounds the whole table to `n` rows, keeping
  active jobs; inside `SubmitOptions::prune` the incoming row counts.
- **Recovering open.** `JobStore::open_recovering(&DbOptions, StoreConfig, RecoveryPolicy) ->
  JobResult<(JobStore, Option<PathBuf>)>`: an unreadable database is moved aside and a fresh store
  is created; the second value is where the old file went. `RecoveryPolicy` is re-exported from
  `rightkit-db` at the root.
- **Per-job resume and sweep.** `Runner::resume_recoverable_each() -> JobResult<Vec<(String,
  JobResult<RunOutcome>)>>`: every recoverable job is attempted, oldest first, and each keeps its
  own result, so one failure does not stop the rest. `Runner::sweep_unclaimed(root) ->
  JobResult<Vec<PathBuf>>` removes child directories of `root` that hold no path claimed by a
  non-terminal job; `JobStore::claimed_paths() -> JobResult<Vec<PathBuf>>` lists those paths.
  `resume_recoverable` and `sweep_orphan_scratch` are unchanged.
- **Held state.** `StatusModel::hold(status)` and `StatusModel::is_held(&str)`: a held status is
  non-terminal and leaves only through `JobStore::resolve(job_id, &Resolution, to:
  Option<&str>) -> JobResult<JobRecord>`; any other move returns `illegal_transition`. Entered
  with `JobStore::hold_submission(job_id, &JobError, to)` and
  `hold_submission_fenced(job_id, generation, &JobError, to)`. The runner neither lists nor runs
  a held job (`run` returns `RunOutcome::Paused`). `cancel`, `delete` and single-flight supersede
  skip or refuse it. `Resolution` (`#[non_exhaustive]`): `Resubmit`, `Abandon(JobError)`,
  `Adopt(Value)`. `StoreHook::on_resolve` (provided) runs in the same transaction as the move; an
  error rolls the resolution back and the job stays held.

## 0.2.8 (additions for long-running desktop jobs)

- **`EngineSupervisor::shutdown()` never leaves an unreaped engine behind `Down`.** It now
  returns only after every engine process the supervisor spawned has been waited, including one a
  monitor thread was mid-reap on, one being booted, and a second concurrent caller (before, `Down`
  could be reported while the child was still a zombie, so `kill -0 <pid>` still succeeded).
  New, additive: `shutdown_detailed() -> ShutdownReport` (`outcome`: `Exited`, `Killed`,
  `NoEngine`, `AlreadyShuttingDown`, `Unconfirmed`; `pid`, `exit_status`, `reaped`, `unreaped`),
  `unreaped_children()`, and on `EngineClient` `child_exit()`, `exit_status()`, `is_reaped()`,
  `wait_reaped()`. If the stated bound passes with a process still unwaited the outcome is
  `Unconfirmed` and the terminal state is `Failed`, not `Stopped`. `kill -0` is not a liveness
  check for a child of the same process (a zombie answers it); use these.
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
