//! The job table without the store: every operation runs on a connection or transaction the
//! caller supplies, so a job write can commit together with the app's own writes (a quota row, a
//! rate-limit row) or roll back with them.
//!
//! [`JobTable`] holds only the configuration (table name, columns, status model, hook). It owns
//! no connection and takes no lock. [`JobStore`](super::JobStore) is built on the same code and
//! keeps its own mutex-guarded connection; the two read and write the same rows.
//!
//! What stays with `JobStore` because it is process state, not table state: cancel tokens,
//! coalesced progress ticks, write-behind of refused writes, delivery pins and pruning. A
//! transaction the caller rolls back leaves none of it behind, and a transaction it commits
//! triggers none of it: after a commit that cancelled or superseded a running job, the caller
//! sets that job's [`CancelToken`](super::CancelToken) itself if it holds one.

use rightkit_db::rusqlite::{self, params, params_from_iter, Connection, OptionalExtension, Row};
use serde_json::Value;

use super::{
    add_event_on, check_fence, init_schema as init_core_schema, now, row, Adoption, Job, JobRecord,
    LifecycleEvent, NewJob, PruneReport, Recovery, Resolution, Submission, SubmitOptions,
    Submitted, Transition, CANCELLED, COLS, COMPLETE, FAILED, NCORE,
};
use crate::config::{Ext, StatusModel, StoreConfig};
use crate::error::{JobError, JobResult};

/// Preconditions of one write: the attempt generation it belongs to and/or the row revision it
/// was read at. `Expect::default()` (or [`Expect::new`]) checks nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expect {
    /// Reject with `stale_attempt` (nothing written) unless the job's attempt generation is
    /// exactly this.
    pub fence: Option<i64>,
    /// Reject with `revision_conflict` (nothing written) unless the job row's revision is
    /// exactly this. Needs [`StoreConfig::row_revision`].
    pub revision: Option<i64>,
}

impl Expect {
    pub fn new() -> Self {
        Self::default()
    }
    /// Require the attempt generation `generation`.
    pub fn fenced(mut self, generation: i64) -> Self {
        self.fence = Some(generation);
        self
    }
    /// Require the row revision `revision` (compare-and-swap).
    pub fn revision(mut self, revision: i64) -> Self {
        self.revision = Some(revision);
        self
    }
    pub(crate) fn fence_only(fence: Option<i64>) -> Self {
        Self {
            fence,
            revision: None,
        }
    }
}

/// Runs between the preconditions and the write of an operation, on the same connection. The
/// store uses it to land its coalesced progress tick in the same commit; a caller has no use
/// for it.
pub(super) type Pre<'a> = &'a dyn Fn(&Connection, &str) -> JobResult<()>;

/// Run `f` inside a savepoint on `conn`: on an autocommit connection the savepoint is the
/// transaction (released = committed), inside a caller transaction it nests. An `Err` rolls
/// every write of the operation back, so a rejecting hook never leaves a committed job.
pub(super) fn atomic<T>(conn: &Connection, f: impl FnOnce() -> JobResult<T>) -> JobResult<T> {
    conn.execute_batch("SAVEPOINT rk_job_op")?;
    match f() {
        Ok(v) => match conn.execute_batch("RELEASE rk_job_op") {
            Ok(()) => Ok(v),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK TO rk_job_op; RELEASE rk_job_op");
                Err(e.into())
            }
        },
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO rk_job_op; RELEASE rk_job_op");
            Err(e)
        }
    }
}

pub(super) fn nop(_: &Connection, _: &str) -> JobResult<()> {
    Ok(())
}

/// The job table's operations on a caller-supplied connection or transaction. Build one from a
/// [`StoreConfig`] (`JobTable::new`) or take the one a store uses ([`JobStore::table`]).
/// Pass a `&rusqlite::Transaction` (it derefs to `&Connection`) to commit with other writes.
///
/// Atomicity: every public operation runs in a savepoint, so an operation that fails leaves
/// none of its writes behind, on an autocommit connection (where the savepoint is the
/// transaction) as well as inside a caller transaction. Take the transaction with
/// `TransactionBehavior::Immediate` (as `rightkit_db::write_txn` does) so reads and writes of
/// one operation see one snapshot; revision checks are additionally guarded in the `UPDATE`
/// itself, so a stale check is refused even on a deferred transaction.
///
/// [`JobStore::table`]: super::JobStore::table
#[derive(Clone)]
pub struct JobTable {
    cfg: StoreConfig,
    select: String,
}

impl JobTable {
    /// Validate `cfg` and build the table operations. Creates nothing: call
    /// [`JobTable::init_schema`] on the connection first.
    pub fn new(cfg: &StoreConfig) -> JobResult<Self> {
        cfg.validate()?;
        Ok(Self::from_validated(cfg))
    }

    pub(super) fn from_validated(cfg: &StoreConfig) -> Self {
        let mut select = COLS.to_string();
        for c in &cfg.columns {
            select.push_str(", ");
            select.push_str(&c.name);
        }
        Self {
            cfg: cfg.clone(),
            select,
        }
    }

    /// The job table name.
    pub fn name(&self) -> &str {
        &self.cfg.jobs_table
    }
    pub fn config(&self) -> &StoreConfig {
        &self.cfg
    }
    pub(super) fn select(&self) -> &str {
        &self.select
    }
    /// The revision of `rec` (0 for a row that has never been written), when the table keeps one.
    pub fn revision_of(&self, rec: &JobRecord) -> Option<i64> {
        let col = self.cfg.revision.as_deref()?;
        Some(rec.ext.get(col).and_then(Value::as_i64).unwrap_or(0))
    }
    /// `, <rev>=<rev>+1` for an `UPDATE ... SET` list, or nothing without a revision column.
    pub(super) fn rev_bump(&self) -> String {
        match self.cfg.revision.as_deref() {
            Some(c) => format!(", {c}={c}+1"),
            None => String::new(),
        }
    }

    /// Create or additively adopt the schema on `conn`: the job table (named by
    /// [`StoreConfig::jobs_table`], with the declared extension columns), its unique
    /// constraints and indexes, the crate-owned side tables, then the app DDL of
    /// [`StoreConfig::app_schema`] in order (so app tables with foreign keys to the job table
    /// can be declared there). Idempotent; never drops or rewrites data. Does not set
    /// `user_version`.
    pub fn init_schema(&self, conn: &Connection) -> JobResult<Adoption> {
        let adoption = init_core_schema(conn, &self.cfg)??;
        for sql in &self.cfg.app_schema {
            conn.execute_batch(sql)?;
        }
        Ok(adoption)
    }

    // ------------------------------------------------------------------ rows ----

    pub(super) fn record_row(&self, r: &Row) -> rusqlite::Result<JobRecord> {
        let mut job = row(r)?;
        if !self.cfg.includes_retryable() {
            if let Some(Value::Object(o)) = &mut job.error {
                o.remove("retryable");
            }
        }
        let mut ext = Ext::new();
        for (i, c) in self.cfg.columns.iter().enumerate() {
            let v: rusqlite::types::Value = r.get(NCORE + i)?;
            ext.insert(c.name.clone(), c.decode(v));
        }
        Ok(JobRecord { job, ext })
    }

    pub(super) fn load(&self, conn: &Connection, job_id: &str) -> JobResult<JobRecord> {
        conn.query_row(
            &format!(
                "SELECT {} FROM {} WHERE job_id=?1",
                self.select,
                self.name()
            ),
            [job_id],
            |r| self.record_row(r),
        )
        .optional()?
        .ok_or_else(|| JobError::new("job_not_found", format!("no such job: {job_id}")))
    }

    /// The job `job_id`, or `job_not_found`.
    pub fn record(&self, conn: &Connection, job_id: &str) -> JobResult<JobRecord> {
        self.load(conn, job_id)
    }

    /// [`JobTable::record`] without the extension columns.
    pub fn get(&self, conn: &Connection, job_id: &str) -> JobResult<Job> {
        self.load(conn, job_id).map(|r| r.job)
    }

    /// The job stored for `request_id`, if any.
    pub fn find_by_request(
        &self,
        conn: &Connection,
        request_id: &str,
    ) -> JobResult<Option<JobRecord>> {
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM {} WHERE request_id=?1",
                    self.select,
                    self.name()
                ),
                [request_id],
                |r| self.record_row(r),
            )
            .optional()?)
    }

    /// Jobs that are due at `now`: the due column (see [`StoreConfig::due_column`]) is not NULL
    /// and `<= now`, in a non-terminal, non-held status, oldest due first (ties by creation).
    /// `now` is injected and compared as the column's own type: an ISO-8601 UTC string for a
    /// TEXT column, epoch seconds for an INTEGER one. At most `limit` rows. Changes nothing;
    /// claiming a job is a separate [`JobTable::transition`] that moves it or clears its due
    /// time.
    pub fn due(&self, conn: &Connection, now: &Value, limit: usize) -> JobResult<Vec<JobRecord>> {
        self.due_with(conn, now, limit, None)
    }

    /// [`JobTable::due`] limited to the named statuses (each a known, non-terminal, non-held
    /// status of the model; `bad_input` otherwise).
    pub fn due_with(
        &self,
        conn: &Connection,
        now: &Value,
        limit: usize,
        statuses: Option<&[&str]>,
    ) -> JobResult<Vec<JobRecord>> {
        let col = self.cfg.due.as_deref().ok_or_else(|| {
            JobError::new(
                "not_configured",
                "store has no due column (StoreConfig::due_column)",
            )
        })?;
        let c = self
            .cfg
            .column_named(col)
            .ok_or_else(|| JobError::new("config", format!("due column {col} is not declared")))?;
        if now.is_null() {
            return Err(JobError::bad_input("now must not be null"));
        }
        let now_v = c.encode(now)?;
        let model = &self.cfg.statuses;
        let set = match statuses {
            None => model.sql_set_unheld(),
            Some(list) => {
                let mut items = Vec::new();
                for s in list {
                    if !model.is_known(s) || StatusModel::is_terminal(s) || model.is_held(s) {
                        return Err(JobError::bad_input(format!(
                            "{s} is not a runnable status of this model"
                        )));
                    }
                    items.push(format!("'{s}'"));
                }
                if items.is_empty() {
                    return Ok(vec![]);
                }
                format!("({})", items.join(","))
            }
        };
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut st = conn.prepare(&format!(
            "SELECT {sel} FROM {t} WHERE {col} IS NOT NULL AND {col} <= ?1 AND status IN {set} \
             ORDER BY {col}, created_at, rowid LIMIT ?2",
            sel = self.select,
            t = self.name(),
        ))?;
        let rows = st
            .query_map(params![now_v, limit], |r| self.record_row(r))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --------------------------------------------------------------- guards ----

    fn check_revision(&self, before: &JobRecord, expect: &Expect) -> JobResult<()> {
        let Some(want) = expect.revision else {
            return Ok(());
        };
        let have = self.revision_of(before).ok_or_else(|| {
            JobError::bad_input("a revision check needs StoreConfig::row_revision")
        })?;
        if have == want {
            Ok(())
        } else {
            Err(JobError::revision_conflict(&before.job.job_id, want, have))
        }
    }

    fn check_expect(&self, conn: &Connection, job_id: &str, expect: &Expect) -> JobResult<()> {
        check_fence(self.name(), conn, job_id, expect.fence)
    }

    pub(super) fn external_col(&self) -> JobResult<&str> {
        self.cfg.external_id.as_deref().ok_or_else(|| {
            JobError::new(
                "not_configured",
                "store has no external id column (StoreConfig::external_id)",
            )
        })
    }

    pub(super) fn refuse_held(&self, rec: &JobRecord) -> JobResult<()> {
        if self.cfg.statuses.is_held(&rec.job.status) {
            return Err(JobError::new(
                "illegal_transition",
                format!(
                    "job {} is held in {}; only resolve can move it",
                    rec.job.job_id, rec.job.status
                ),
            ));
        }
        Ok(())
    }

    // ---------------------------------------------------------------- insert ----

    /// Validate and encode the ext values of a new job: `(column names, encoded values)`.
    pub(super) fn encode_new_ext(
        &self,
        ext: &Ext,
    ) -> JobResult<(Vec<String>, Vec<rusqlite::types::Value>)> {
        let mut names = Vec::new();
        let mut vals: Vec<rusqlite::types::Value> = Vec::new();
        for (k, v) in ext {
            if self.cfg.revision.as_deref() == Some(k.as_str()) {
                return Err(JobError::bad_input(format!(
                    "ext.{k} is the row revision and is managed by the store"
                )));
            }
            let c = self
                .cfg
                .column_named(k)
                .ok_or_else(|| JobError::bad_input(format!("unknown ext column: {k}")))?;
            names.push(c.name.clone());
            vals.push(c.encode(v)?);
        }
        if let Some(c) = self
            .cfg
            .columns
            .iter()
            .find(|c| c.required() && !ext.contains_key(&c.name))
        {
            return Err(JobError::bad_input(format!("ext.{} required", c.name)));
        }
        Ok((names, vals))
    }

    /// Insert a job in the initial status on `tx` and run `StoreHook::on_create`.
    pub(super) fn insert_new(
        &self,
        tx: &Connection,
        n: &NewJob,
        names: &[String],
        vals: Vec<rusqlite::types::Value>,
        preset_id: Option<&str>,
    ) -> JobResult<(String, JobRecord)> {
        let id = preset_id
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let t = now();
        let mut cols =
            "job_id, request_id, kind, request_json, status, attempts, created_at, updated_at"
                .to_string();
        let mut ph = "?1,?2,?3,?4,?5,0,?6,?6".to_string();
        for (i, name) in names.iter().enumerate() {
            cols.push_str(&format!(", {name}"));
            ph.push_str(&format!(",?{}", 7 + i));
        }
        let mut all: Vec<rusqlite::types::Value> = vec![
            id.clone().into(),
            n.request_id.clone().into(),
            n.kind.clone().into(),
            n.request.to_string().into(),
            self.cfg.statuses.initial().to_string().into(),
            t.into(),
        ];
        all.extend(vals);
        tx.execute(
            &format!("INSERT INTO {} ({cols}) VALUES ({ph})", self.name()),
            params_from_iter(all),
        )?;
        let rec = self.load(tx, &id)?;
        if let Some(h) = &self.cfg.hook {
            h.on_create(tx, &rec)?;
        }
        Ok((id, rec))
    }

    /// The part of a submit that follows the request-id and prune steps: insert the job, store
    /// its recovery class and single-flight key, and supersede the older jobs of the same key.
    /// Returns the new record and the ids it superseded.
    pub(super) fn insert_flow(
        &self,
        tx: &Connection,
        n: &NewJob,
        opts: &SubmitOptions,
        preset_id: Option<&str>,
        names: &[String],
        vals: Vec<rusqlite::types::Value>,
    ) -> JobResult<(JobRecord, Vec<String>)> {
        let (id, rec) = self.insert_new(tx, n, names, vals, preset_id)?;
        let class = match opts.recovery {
            Recovery::Resume => self.cfg.kind_recovery(&n.kind).unwrap_or(Recovery::Resume),
            other => other,
        };
        if class != Recovery::default() || opts.single_flight.is_some() {
            tx.execute(
                "INSERT INTO rk_job_meta (job_id, recovery, flight_key) VALUES (?1,?2,?3)",
                params![id, class.as_str(), opts.single_flight],
            )?;
        }
        let mut superseded = Vec::new();
        if let Some(key) = &opts.single_flight {
            let old: Vec<String> = {
                let mut st = tx.prepare(&format!(
                    "SELECT j.job_id FROM rk_job_meta m JOIN {} j ON j.job_id=m.job_id \
                     WHERE m.flight_key=?1 AND m.job_id<>?2 AND j.status IN {} \
                     ORDER BY j.created_at, j.rowid",
                    self.name(),
                    // A held job waits for a person: a newer submit never replaces it.
                    self.cfg.statuses.sql_set_unheld()
                ))?;
                let rows = st
                    .query_map(params![key, id], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };
            for old_id in old {
                let before = self.load(tx, &old_id)?;
                let err = JobError::superseded(&id);
                let t = Transition::to(CANCELLED)
                    .error(Some(err.to_json_with(self.cfg.includes_retryable())));
                self.exec(tx, &before, &t)?;
                add_event_on(
                    self.name(),
                    tx,
                    &old_id,
                    None,
                    "superseded",
                    None,
                    None,
                    Some(&err.message),
                )?;
                superseded.push(old_id);
            }
        }
        Ok((rec, superseded))
    }

    /// Insert a job in the initial status, or return the existing one for the same
    /// `request_id` (`deduplicated`), on the caller's connection. `StoreHook::on_create` runs
    /// on it, and its error is returned for the caller to roll back. The recovery class and
    /// single-flight key of `opts` apply as in [`JobStore::submit_with`](super::JobStore::submit_with);
    /// a superseded job is cancelled in the table (the caller sets any in-process cancel token
    /// after its commit). `opts.prune` is refused (`bad_input`): prune through the store.
    pub fn enqueue(
        &self,
        conn: &Connection,
        n: &NewJob,
        ext: &Ext,
        opts: &SubmitOptions,
    ) -> JobResult<Submitted> {
        atomic(conn, || {
            if opts.prune.is_some() {
                return Err(JobError::bad_input(
                    "prune is not available on a caller connection; call JobStore::prune",
                ));
            }
            if opts.single_flight.as_deref() == Some("") {
                return Err(JobError::bad_input("single-flight key is empty"));
            }
            let (names, vals) = self.encode_new_ext(ext)?;
            if let Some(rid) = &n.request_id {
                if let Some(j) = self.find_by_request(conn, rid)? {
                    return Ok(Submitted {
                        record: j,
                        deduplicated: true,
                        superseded: vec![],
                        pruned: PruneReport::default(),
                    });
                }
            }
            let (record, superseded) = self.insert_flow(conn, n, opts, None, &names, vals)?;
            Ok(Submitted {
                record,
                deduplicated: false,
                superseded,
                pruned: PruneReport::default(),
            })
        })
    }

    // ------------------------------------------------------------ transitions ----

    pub(super) fn exec(
        &self,
        conn: &Connection,
        before: &JobRecord,
        t: &Transition,
    ) -> JobResult<JobRecord> {
        self.exec_in(conn, before, t, false)
    }

    /// Apply `t` to `before` on `conn` and fire hooks. `force` skips the status model's move
    /// check (hold and resolve, which check their own preconditions). With a revision column the
    /// row's revision rises by one, and the `UPDATE` only matches the revision `before` was read
    /// at.
    pub(super) fn exec_in(
        &self,
        conn: &Connection,
        before: &JobRecord,
        t: &Transition,
        force: bool,
    ) -> JobResult<JobRecord> {
        let model = &self.cfg.statuses;
        if let Some(to) = &t.to {
            if !force && !model.can_move(&before.job.status, to) {
                return Err(JobError::new(
                    "illegal_transition",
                    format!(
                        "job {} cannot move from {} to {to}",
                        before.job.job_id, before.job.status
                    ),
                ));
            }
        }
        let mut sets = vec!["updated_at=?2".to_string()];
        let mut vals: Vec<rusqlite::types::Value> =
            vec![before.job.job_id.clone().into(), now().into()];
        let mut bind = |sets: &mut Vec<String>, col: &str, v: rusqlite::types::Value| {
            vals.push(v);
            sets.push(format!("{col}=?{}", vals.len()));
        };
        if let Some(to) = &t.to {
            bind(&mut sets, "status", to.clone().into());
        }
        if t.count_attempt {
            sets.push("attempts=attempts+1".into());
        }
        for (k, v) in &t.ext {
            if self.cfg.revision.as_deref() == Some(k.as_str()) {
                return Err(JobError::bad_input(format!(
                    "ext.{k} is the row revision and is managed by the store"
                )));
            }
            let c = self
                .cfg
                .column_named(k)
                .ok_or_else(|| JobError::bad_input(format!("unknown ext column: {k}")))?;
            bind(&mut sets, &c.name, c.encode(v)?);
        }
        if let Some(e) = &t.error {
            bind(
                &mut sets,
                "error_json",
                e.as_ref().map(|v| v.to_string()).into(),
            );
        }
        if let Some(r) = &t.result {
            bind(&mut sets, "result_json", r.to_string().into());
        }
        let mut guard = String::new();
        let mut guarded = None;
        if let (Some(col), Some(have)) = (self.cfg.revision.as_deref(), self.revision_of(before)) {
            sets.push(format!("{col}={col}+1"));
            vals.push(have.into());
            guard = format!(" AND {col}=?{}", vals.len());
            guarded = Some(have);
        }
        let changed = conn.execute(
            &format!(
                "UPDATE {} SET {} WHERE job_id=?1{guard}",
                self.name(),
                sets.join(", ")
            ),
            params_from_iter(vals),
        )?;
        if let Some(have) = guarded {
            if changed != 1 {
                let actual = self
                    .load(conn, &before.job.job_id)
                    .ok()
                    .and_then(|r| self.revision_of(&r))
                    .unwrap_or(have);
                return Err(JobError::revision_conflict(
                    &before.job.job_id,
                    have,
                    actual,
                ));
            }
        }
        let after = self.load(conn, &before.job.job_id)?;
        if let Some(h) = &self.cfg.hook {
            if t.count_attempt {
                h.on_attempt(conn, &after, after.job.attempts)?;
            }
            if after.job.status != before.job.status {
                h.on_transition(conn, before, &after)?;
            }
        }
        Ok(after)
    }

    /// The shared body of every transition: preconditions, `pre`, the write and an optional
    /// lifecycle event. `Ok(None)` when the job is already terminal.
    pub(super) fn transition_on(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: &Expect,
        t: &Transition,
        event: Option<&LifecycleEvent>,
        pre: Pre<'_>,
    ) -> JobResult<Option<(JobRecord, Option<super::Event>)>> {
        self.check_expect(conn, job_id, expect)?;
        let before = self.load(conn, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        self.check_revision(&before, expect)?;
        pre(conn, job_id)?;
        let after = self.exec(conn, &before, t)?;
        let ev = match event {
            Some(e) => Some(add_event_on(
                self.name(),
                conn,
                job_id,
                None,
                &e.stage,
                None,
                e.progress,
                e.message.as_deref(),
            )?),
            None => None,
        };
        Ok(Some((after, ev)))
    }

    /// Validate and apply `t` to `job_id` with the hook, on the caller's connection.
    /// `Ok(None)` when the job is already terminal (nothing changes);
    /// `Err(illegal_transition)` for a move the status model forbids; `stale_attempt` or
    /// `revision_conflict` when `expect` does not hold; a hook error is returned for the
    /// caller to roll back.
    pub fn transition(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        t: &Transition,
    ) -> JobResult<Option<JobRecord>> {
        atomic(conn, || {
            Ok(self
                .transition_on(conn, job_id, &expect, t, None, &nop)?
                .map(|(after, _)| after))
        })
    }

    /// Set extension columns without changing status. Unlike [`JobTable::transition`] it
    /// also writes a terminal job's columns (a final cost, a last outcome note).
    pub fn set_ext(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        ext: &Ext,
    ) -> JobResult<JobRecord> {
        atomic(conn, || {
            let t = Transition {
                ext: ext.clone(),
                ..Transition::default()
            };
            self.check_expect(conn, job_id, &expect)?;
            let before = self.load(conn, job_id)?;
            self.check_revision(&before, &expect)?;
            self.exec(conn, &before, &t)
        })
    }

    /// Complete with a result. `Ok(false)` if the job is already terminal.
    pub fn complete(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        result: &Value,
    ) -> JobResult<bool> {
        Ok(self
            .transition(
                conn,
                job_id,
                expect,
                &Transition::to(COMPLETE).result(result.clone()).error(None),
            )?
            .is_some())
    }

    /// Fail with `error`. `Ok(false)` if the job is already terminal.
    pub fn fail(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        error: &JobError,
    ) -> JobResult<bool> {
        Ok(self
            .transition(
                conn,
                job_id,
                expect,
                &Transition::to(FAILED)
                    .error(Some(error.to_json_with(self.cfg.includes_retryable()))),
            )?
            .is_some())
    }

    // ------------------------------------------------- external submissions ----

    /// [`JobStore::begin_submission`](super::JobStore::begin_submission) on the caller's
    /// connection: decide how to drive the job's external submission so a crash or restart
    /// never double-submits. `Ok(None)` when the job is terminal.
    pub fn begin_submission(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        max_attempts: i64,
    ) -> JobResult<Option<Submission>> {
        atomic(conn, || {
            let col = self.external_col()?.to_string();
            self.check_expect(conn, job_id, &expect)?;
            let before = self.load(conn, job_id)?;
            if before.job.is_terminal() {
                return Ok(None);
            }
            self.check_revision(&before, &expect)?;
            // Counting an attempt on a held job would reserve spend for a submission nobody may make.
            self.refuse_held(&before)?;
            if let Some(x) = before.ext_str(&col) {
                return Ok(Some(Submission::Resume {
                    external_id: x.to_string(),
                    attempt: before.job.attempts,
                }));
            }
            let pending: Option<String> = conn
                .query_row(
                    "SELECT idempotency_key FROM rk_job_submissions WHERE job_id=?1 AND attempt=?2 AND state='pending'",
                    params![job_id, before.job.attempts],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(key) = pending {
                return Ok(Some(Submission::Submit {
                    attempt: before.job.attempts,
                    idempotency_key: key,
                    replay: true,
                }));
            }
            if before.job.attempts >= max_attempts.max(1) {
                return Ok(Some(Submission::Exhausted {
                    attempts: before.job.attempts,
                }));
            }
            let after = self.exec(conn, &before, &Transition::keep().count_attempt())?;
            let attempt = after.job.attempts;
            let key = format!("{job_id}:{attempt}");
            let t = now();
            conn.execute(
                "INSERT OR REPLACE INTO rk_job_submissions (job_id, attempt, idempotency_key, state, created_at, updated_at) VALUES (?1,?2,?3,'pending',?4,?4)",
                params![job_id, attempt, key, t],
            )?;
            Ok(Some(Submission::Submit {
                attempt,
                idempotency_key: key,
                replay: false,
            }))
        })
    }

    /// [`JobStore::accept_submission`](super::JobStore::accept_submission) on the caller's
    /// connection: store the external id of the current attempt (and optionally move).
    pub fn accept_submission(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        external_id: &str,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        atomic(conn, || {
            let col = self.external_col()?.to_string();
            self.check_expect(conn, job_id, &expect)?;
            let before = self.load(conn, job_id)?;
            if before.job.is_terminal() {
                return Ok(None);
            }
            self.check_revision(&before, &expect)?;
            let t = Transition {
                to: to.map(Into::into),
                ..Transition::default()
            }
            .set(col, external_id);
            let after = self.exec(conn, &before, &t)?;
            conn.execute(
                "UPDATE rk_job_submissions SET state='accepted', external_id=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
                params![job_id, after.job.attempts, external_id, now()],
            )?;
            Ok(Some(after))
        })
    }

    /// [`JobStore::fail_submission`](super::JobStore::fail_submission) on the caller's
    /// connection: the current attempt is confirmed failed; forget the external id, record the
    /// error and optionally move.
    pub fn fail_submission(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        error: &JobError,
        to: Option<&str>,
    ) -> JobResult<Option<JobRecord>> {
        atomic(conn, || {
            let col = self.external_col()?.to_string();
            self.check_expect(conn, job_id, &expect)?;
            let before = self.load(conn, job_id)?;
            if before.job.is_terminal() {
                return Ok(None);
            }
            self.check_revision(&before, &expect)?;
            let t = Transition {
                to: to.map(Into::into),
                ..Transition::default()
            }
            .set(col, Value::Null)
            .error(Some(error.to_json_with(self.cfg.includes_retryable())));
            let after = self.exec(conn, &before, &t)?;
            conn.execute(
                "UPDATE rk_job_submissions SET state='failed', error_json=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
                params![job_id, before.job.attempts, error.to_json_with(self.cfg.includes_retryable()).to_string(), now()],
            )?;
            Ok(Some(after))
        })
    }

    pub(super) fn hold_submission_on(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: &Expect,
        error: &JobError,
        to: &str,
        pre: Pre<'_>,
    ) -> JobResult<Option<JobRecord>> {
        self.external_col()?;
        if !self.cfg.statuses.is_held(to) {
            return Err(JobError::bad_input(format!("{to} is not a held status")));
        }
        self.check_expect(conn, job_id, expect)?;
        let before = self.load(conn, job_id)?;
        if before.job.is_terminal() {
            return Ok(None);
        }
        self.check_revision(&before, expect)?;
        self.refuse_held(&before)?;
        pre(conn, job_id)?;
        let ej = error.to_json_with(self.cfg.includes_retryable());
        let t = Transition::to(to).error(Some(ej.clone()));
        let after = self.exec_in(conn, &before, &t, true)?;
        conn.execute(
            "UPDATE rk_job_submissions SET error_json=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
            params![job_id, before.job.attempts, ej.to_string(), now()],
        )?;
        add_event_on(
            self.name(),
            conn,
            job_id,
            None,
            "held",
            None,
            None,
            Some(&error.message),
        )?;
        Ok(Some(after))
    }

    /// [`JobStore::hold_submission`](super::JobStore::hold_submission) on the caller's
    /// connection: record `error` and park the job in the held status `to`, in the caller's
    /// transaction with `StoreHook::on_transition` and a `held` event. `Ok(None)` when the job
    /// is terminal; `illegal_transition` when it is already held.
    pub fn hold_submission(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        error: &JobError,
        to: &str,
    ) -> JobResult<Option<JobRecord>> {
        atomic(conn, || {
            self.hold_submission_on(conn, job_id, &expect, error, to, &nop)
        })
    }

    pub(super) fn resolve_on(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: &Expect,
        r: &Resolution,
        to: Option<&str>,
        pre: Pre<'_>,
    ) -> JobResult<JobRecord> {
        let model = &self.cfg.statuses;
        if matches!(r, Resolution::Resubmit) {
            match to {
                Some(t)
                    if model.is_known(t) && !StatusModel::is_terminal(t) && !model.is_held(t) => {}
                _ => {
                    return Err(JobError::bad_input(
                        "resubmit needs a known, non-terminal, non-held target status",
                    ))
                }
            }
        }
        self.check_expect(conn, job_id, expect)?;
        let before = self.load(conn, job_id)?;
        if !model.is_held(&before.job.status) {
            return Err(JobError::new(
                "illegal_transition",
                format!(
                    "job {job_id} is {}, not held; only a held job can be resolved",
                    before.job.status
                ),
            ));
        }
        self.check_revision(&before, expect)?;
        pre(conn, job_id)?;
        let includes = self.cfg.includes_retryable();
        let (t, sub_state, label) = match r {
            Resolution::Resubmit => {
                let mut t = Transition::to(to.unwrap_or_default());
                if let Some(col) = self.cfg.external_id.as_deref() {
                    t = t.set(col, Value::Null);
                }
                (t, "failed", "resubmit")
            }
            Resolution::Abandon(e) => (
                Transition::to(FAILED).error(Some(e.to_json_with(includes))),
                "failed",
                "abandon",
            ),
            Resolution::Adopt(v) => (
                Transition::to(COMPLETE).result(v.clone()).error(None),
                "accepted",
                "adopt",
            ),
        };
        let after = self.exec_in(conn, &before, &t, true)?;
        if self.cfg.external_id.is_some() {
            conn.execute(
                "UPDATE rk_job_submissions SET state=?3, updated_at=?4 WHERE job_id=?1 AND attempt=?2",
                params![job_id, before.job.attempts, sub_state, now()],
            )?;
        }
        add_event_on(
            self.name(),
            conn,
            job_id,
            None,
            "resolved",
            None,
            None,
            Some(label),
        )?;
        if let Some(h) = &self.cfg.hook {
            h.on_resolve(conn, &after, after.job.attempts, r)?;
        }
        Ok(after)
    }

    /// [`JobStore::resolve`](super::JobStore::resolve) on the caller's connection: settle a
    /// held job in the caller's transaction with `StoreHook::on_transition`,
    /// `StoreHook::on_resolve` and a `resolved` event. `illegal_transition` unless the job is
    /// in a held status.
    pub fn resolve(
        &self,
        conn: &Connection,
        job_id: &str,
        expect: Expect,
        r: &Resolution,
        to: Option<&str>,
    ) -> JobResult<JobRecord> {
        atomic(conn, || self.resolve_on(conn, job_id, &expect, r, to, &nop))
    }
}
