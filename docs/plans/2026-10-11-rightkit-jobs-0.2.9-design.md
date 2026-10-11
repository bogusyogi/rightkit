# rightkit-jobs 0.2.9 design ruling: ScrapeRight's 15 items and GenRight row 154

Date: 11 October 2026. Design ruling by Sage from ScrapeRight's gap list (`scraperight/docs/handoff-2026-10-11/rightkit-gaps-jobs-search.md`) and GenRight's ledger row 154. Not yet implemented. Lead's decision on item 9: ruled by its test (a failed job with a label, retried as a new job).

Nothing was built or run; this rests on reading `crates/rightkit-jobs/src` at 0.2.8 (`Cargo.toml:3`), ScrapeRight's cited code and GenRight's `genright-gen`. I did not open the `rightkit-jobs/tests` files beyond a grep of `cancel_signal.rs`, nor CutRight's or PostRight's call sites beyond their imports.

## 1. Rulings

| # | Ruling | API or call |
|---|---|---|
| 1 | ACCEPT | `StagedPool` (below); the limit closure is called on every admission. |
| 2 | ACCEPT | `PoolConfig::handoff(n)`; a full buffer blocks the prepare thread, which keeps its slot. |
| 3 | ACCEPT | `StagedHandler::Lane`, built and used on one thread; `PoolConfig::idle_after(d)`. The 45 s value stays in ScrapeRight. |
| 4 | PRESENT for recovery order | `non_terminal_ids` is oldest-first (`store.rs:983-986`). Live FIFO comes with the pool. |
| 5 | PRESENT | Pass `ctl.cancel_token().flag()` (`store.rs:191`) to the child loop; tree kill is rightkit-process. A late error after cancel ends `cancelled` (`runner.rs:573-578`). |
| 6 | PRESENT | `JobStore::cancel` (`store.rs:1310`) returns unchanged for terminal jobs (`store.rs:1085`). |
| 7 | ACCEPT | `JobStore::delete(&self, job_id: &str) -> JobResult<PruneReport>`. |
| 8 | PRESENT by composition | `record` (`store.rs:931`) then `submit_with` (`store.rs:635`) with `request_id: None`. A shared helper would have to guess which ext columns carry over, and GenRight's `provider_job_id` must not. |
| 9 | PRESENT | Declare `ExtColumn::text("needs_auth")`, call `ctl.set_ext` (`runner.rs:136`), return `JobError::new("needs_auth", ..)`. Retry is item 8. The platform label is product data. |
| 10 | PRESENT | `ctl.add_artifact` after fetch; `ctl.verified_artifacts()` (`runner.rs:265`) on resume. |
| 11 | ACCEPT | `PrunePolicy::keep_total(self, n: usize) -> Self`. |
| 12 | ACCEPT | `JobStore::claimed_paths(&self) -> JobResult<Vec<PathBuf>>` and `Runner::sweep_unclaimed(&self, root: &Path) -> JobResult<Vec<PathBuf>>`. The off switch is not calling it. |
| 13 | ACCEPT | `JobStore::open_recovering(options: &DbOptions, cfg: StoreConfig, policy: RecoveryPolicy) -> JobResult<(JobStore, Option<PathBuf>)>`. |
| 14 | ACCEPT the atomic record; the entitlement check STAYS IN THE APP | `JobStore::submit_rejected(&self, n: &NewJob, ext: &Ext, error: &JobError) -> JobResult<JobRecord>`. |
| 15 | ACCEPT | `Runner::resume_recoverable_each(&self) -> JobResult<Vec<(String, JobResult<RunOutcome>)>>`. |

Semantics the lanes need:

- **7:** one transaction. A non-terminal job is first moved to `cancelled` (the hook fires), then its rows are deleted and its cancel token is set. The caller deletes files from the report.
- **11:** keeps `n` minus the non-terminal count of the newest terminal rows. Inside `SubmitOptions::prune` it counts the incoming row.
- **12:** claimed paths are artifact paths of non-terminal jobs. The sweep removes child directories of `root` that contain none.
- **13:** wraps `DbOptions::open_writer_recovering` (`rightkit-db/src/recover.rs:115`); today the store calls plain `open_writer` (`store.rs:519`).
- **14:** insert, `on_create`, move to `failed` with the error, and a `rejected` event, in one transaction. Submit-then-fail in two transactions would leave a runnable job after a crash.
- **15:** `resume_recoverable` and `job.recover` keep their current behaviour.

Pool, in a new `src/pool.rs`:

```rust
pub enum Staged<P> { Done(Value), Compute(P) }
pub trait StagedHandler: Send + Sync + 'static {
    type Prepared: Send + 'static;
    type Lane: 'static;
    fn new_lane(&self) -> Self::Lane;
    fn prepare(&self, job: &Job, ctl: &JobControl) -> JobResult<Staged<Self::Prepared>>;
    fn compute(&self, lane: &mut Self::Lane, job: &Job, work: Self::Prepared, ctl: &JobControl) -> JobResult<Value>;
    fn idle(&self, _lane: &mut Self::Lane) {}
}
impl PoolConfig {
    pub fn new(limit: impl Fn() -> usize + Send + Sync + 'static) -> Self;
    pub fn handoff(self, n: usize) -> Self;
    pub fn idle_after(self, d: Duration) -> Self;
    pub fn on_outcome(self, f: Arc<dyn Fn(&str, JobResult<RunOutcome>) + Send + Sync>) -> Self;
}
impl StagedPool {
    pub fn start<H: StagedHandler>(store: Arc<JobStore>, locks: JobLocks, handler: Arc<H>,
        cfg: PoolConfig, configure: impl FnOnce(Runner) -> Runner) -> JobResult<StagedPool>;
    pub fn enqueue(&self, job_id: &str) -> JobResult<()>;
    pub fn resume_recoverable(&self) -> JobResult<Vec<String>>;
    pub fn runner(&self) -> &Arc<Runner>;
    pub fn shutdown(self);
}
```

- The pool wraps `Runner`: each admitted job runs `runner.run` on its own thread, so lock, fencing and lifecycle are unchanged.
- This needs one provided method on `JobHandler`, called by `Runner::run_handler`: `fn run_shared(&self, job: &Job, ctl: &Arc<JobControl>) -> JobResult<Value> { self.run(job, ctl) }`.
- The pool's adapter overrides it: prepare, then a `try_send` loop into `sync_channel(handoff)` that heartbeats and checks `abort_reason` every 50 ms.
- The slot is released when the send lands; the thread then waits for the lane's reply, heartbeating until compute starts.
- The lane checks `abort_reason` before compute, calls `idle` once per idle period, then blocks.

## 2. Lanes

Signatures are frozen above and lanes do not build, so all four run in parallel. The lead builds once, in order A, B, C.

- **A:** `src/store.rs`, `src/config.rs`, `src/error.rs`, `tests/prune.rs`, new `tests/delete_reject.rs`, `tests/held.rs`. Items 7, 11, 13, 14, `claimed_paths`, and section 5.
- **B:** `src/runner.rs`, new `tests/resume_sweep.rs`. Item 15, `sweep_unclaimed`, `run_shared`, held-skip.
- **C:** `src/pool.rs`, new `tests/pool.rs`. Items 1-3 and live FIFO.
- **D:** `src/lib.rs` re-exports, `Cargo.toml`, README, CHANGELOG, `runtime-budget.json`, ledger rows 154 and 156.

## 3. Compatibility

- **Unchanged:** every existing signature, wire frame, `JOB_METHODS`, schema column, `resume_recoverable` and `job.recover`. New state goes in crate-owned tables only.
- **No new fields** on `ListFilter`, `NewJob`, `Artifact` (GenRight builds all three as full literals, `genright-gen/src/store.rs:271, 320, 473`), nor on `Transition`, `JobError`, `Job`, `Event`, `PruneReport`, `Submitted`, `SubmissionRecord`.
- **No new variants** on `Submission` (GenRight matches it, `jobs.rs:191-251`), `RunOutcome`, `AttemptPolicy`, `Priority`, `ColumnType`.
- **How to add instead:** new methods, builder fields on private-field structs (`PrunePolicy`, `StoreConfig`, `StatusModel`), provided trait methods, and new types marked `#[non_exhaustive]`.

## 4. Tests

- **A**
  - `delete_running_cancels_then_removes`: row gone, token set, later fenced write returns `job_not_found`, hook saw `cancelled`.
  - `keep_total_never_drops_active`: 201 terminal plus active rows leaves 200 total.
  - `rejected_submit_leaves_failed_row`: the row carries the message and never runs.
  - `unreadable_db_is_quarantined`: original bytes sit at the `.corrupt-` name.
  - `held_*`: see section 5.
- **B**
  - `one_failing_job_does_not_stop_the_rest`.
  - `claimed_dir_survives_sweep`.
  - `held_job_is_not_recoverable`.
  - `late_error_after_cancel_ends_cancelled`.
- **C**
  - `limit_one_never_overlaps`.
  - `raised_limit_seen_next_admission`.
  - `full_handoff_blocks_prepare`: no further `prepare` call starts.
  - `compute_runs_on_one_thread`.
  - `idle_fires_once_not_between_back_to_back`.
  - `recovered_order_equals_submission_order`.
  - `cancel_while_buffered_ends_cancelled`.
  - `stall_timeout_does_not_trip_while_waiting`.

## 5. GenRight row 154: folded into lane A

It shares the store transaction, hook and submissions table, so it belongs here.

```rust
impl StatusModel { pub fn hold(self, status: impl Into<String>) -> Self; pub fn is_held(&self, s: &str) -> bool; }
#[non_exhaustive] pub enum Resolution { Resubmit, Abandon(JobError), Adopt(Value) }
impl JobStore {
    pub fn hold_submission(&self, job_id: &str, error: &JobError, to: &str) -> JobResult<Option<JobRecord>>;
    pub fn hold_submission_fenced(&self, job_id: &str, generation: i64, error: &JobError, to: &str) -> JobResult<Option<JobRecord>>;
    pub fn resolve(&self, job_id: &str, r: &Resolution, to: Option<&str>) -> JobResult<JobRecord>;
}
// StoreHook, provided:
fn on_resolve(&self, _conn: &Connection, _job: &JobRecord, _attempt: i64, _r: &Resolution) -> JobResult<()> { Ok(()) }
```

- A held status is non-terminal and leaves only through `resolve`; anything else returns `illegal_transition`.
- `cancel`, `delete` and single-flight supersede skip or refuse held jobs.
- `Runner` neither lists nor runs them, and `run` returns `Paused`.
- `resolve` runs in one transaction with `on_resolve` and `on_transition`, so the ledger keeps or releases the reservation atomically. Today GenRight reconciles in a separate step (`jobs.rs:376-394`).
- `Resubmit` marks the submission failed, clears the external id and moves to `to`. `Abandon` ends `failed`. `Adopt` ends `complete`.
- Tests: `held_survives_cancel_and_recovery`, `resolve_resubmit_counts_one_new_attempt`, `resolve_hook_error_rolls_back`, `resolve_not_held_is_refused`.
- `HttpError::request_may_have_been_delivered()` belongs to rightkit-http and is separate.

## 6. Risks and least-sure decisions

**Risks**
- Unverified: the field names of rightkit-db's `OpenOutcome`.
- The pool adds a continuous thread, so the runtime budget must be re-declared.
- GenRight must drop `allow(OUTCOME_UNKNOWN, SUBMITTED)` when it adopts `hold`.
- Deleting a running job does not wait for its handler to stop.

**Least sure**
1. **Item 9.** I ruled by its test (a failed job with a label, retry as a new job), not its title ("retryable pause"). If ScrapeRight wants an in-place resume after sign-in, it becomes a held status with `Resolution::Resubmit`.
2. **Pool over `Runner` via `run_shared`.** It holds one thread per buffered job. The alternative is a stage-aware runner, which would rewrite `run_with` and put the other three consumers at risk.
