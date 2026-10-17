//! Store extension points: per-job extension columns, a pluggable status
//! vocabulary, a transactional hook, artifact table/wire names, and app-owned
//! schema. Everything defaults to the 0.1 behaviour, so `JobStore::open` is
//! unchanged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rightkit_db::rusqlite::{self, Connection};
use serde_json::{Map, Value};

use crate::error::{JobError, JobResult};
use crate::store::{JobRecord, Recovery, Resolution, CANCELLED, COMPLETE, FAILED, QUEUED, RUNNING};

/// Name of the job table unless [`StoreConfig::jobs_table`] names another.
pub const DEFAULT_JOBS_TABLE: &str = "jobs";

/// Extension values of one job, keyed by declared column name.
pub type Ext = Map<String, Value>;

/// Core `jobs` columns owned by the crate; extension columns may not reuse them.
pub(crate) const CORE_JOB_COLUMNS: [&str; 13] = [
    "job_id",
    "request_id",
    "kind",
    "request_json",
    "status",
    "attempts",
    "error_json",
    "result_json",
    "acked_by",
    "acked_at",
    "created_at",
    "updated_at",
    "attempt_gen",
];

/// SQLite storage class of an extension column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Text,
    Integer,
    Real,
    /// Any JSON value, stored as TEXT and parsed on read.
    Json,
}

impl ColumnType {
    pub(crate) fn sql(self) -> &'static str {
        match self {
            ColumnType::Text | ColumnType::Json => "TEXT",
            ColumnType::Integer => "INTEGER",
            ColumnType::Real => "REAL",
        }
    }
}

/// An app-declared column on the `jobs` table (e.g. GenRight's `provider`,
/// `model`, `provider_job_id`, `est_usd`, `actual_usd`). Values travel in
/// [`Ext`] and appear as top-level fields of [`JobRecord::to_value`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExtColumn {
    pub name: String,
    pub ty: ColumnType,
    pub not_null: bool,
    /// SQL default (also used by `ALTER TABLE ADD COLUMN` on an existing database).
    pub default: Option<Value>,
}

impl ExtColumn {
    pub fn new(name: impl Into<String>, ty: ColumnType) -> Self {
        Self {
            name: name.into(),
            ty,
            not_null: false,
            default: None,
        }
    }
    pub fn text(name: impl Into<String>) -> Self {
        Self::new(name, ColumnType::Text)
    }
    pub fn integer(name: impl Into<String>) -> Self {
        Self::new(name, ColumnType::Integer)
    }
    pub fn real(name: impl Into<String>) -> Self {
        Self::new(name, ColumnType::Real)
    }
    pub fn json(name: impl Into<String>) -> Self {
        Self::new(name, ColumnType::Json)
    }
    pub fn not_null(mut self) -> Self {
        self.not_null = true;
        self
    }
    pub fn default(mut self, v: impl Into<Value>) -> Self {
        self.default = Some(v.into());
        self
    }
    /// A NOT NULL column without a default must be supplied on every submit.
    pub fn required(&self) -> bool {
        self.not_null && self.default.as_ref().is_none_or(Value::is_null)
    }

    pub(crate) fn ddl(&self) -> JobResult<String> {
        let mut s = format!("{} {}", self.name, self.ty.sql());
        if self.not_null {
            s.push_str(" NOT NULL");
        }
        if let Some(d) = self.default.as_ref().filter(|d| !d.is_null()) {
            s.push_str(" DEFAULT ");
            s.push_str(&sql_literal(self.ty, d)?);
        }
        Ok(s)
    }

    pub(crate) fn encode(&self, v: &Value) -> JobResult<rusqlite::types::Value> {
        use rusqlite::types::Value as S;
        let bad = || {
            JobError::bad_input(format!(
                "ext.{} must be {}",
                self.name,
                match self.ty {
                    ColumnType::Text => "a string",
                    ColumnType::Integer => "an integer",
                    ColumnType::Real => "a number",
                    ColumnType::Json => "JSON",
                }
            ))
        };
        if v.is_null() {
            if self.not_null {
                return Err(JobError::bad_input(format!(
                    "ext.{} may not be null",
                    self.name
                )));
            }
            return Ok(S::Null);
        }
        Ok(match self.ty {
            ColumnType::Text => S::Text(v.as_str().ok_or_else(bad)?.to_string()),
            ColumnType::Integer => S::Integer(v.as_i64().ok_or_else(bad)?),
            ColumnType::Real => S::Real(v.as_f64().ok_or_else(bad)?),
            ColumnType::Json => S::Text(v.to_string()),
        })
    }

    pub(crate) fn decode(&self, v: rusqlite::types::Value) -> Value {
        use rusqlite::types::Value as S;
        match v {
            S::Null => Value::Null,
            S::Integer(i) if self.ty == ColumnType::Real => Value::from(i as f64),
            S::Integer(i) => Value::from(i),
            S::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
            S::Text(t) if self.ty == ColumnType::Json => {
                serde_json::from_str(&t).unwrap_or(Value::String(t))
            }
            S::Text(t) => Value::String(t),
            S::Blob(_) => Value::Null,
        }
    }
}

fn sql_literal(ty: ColumnType, v: &Value) -> JobResult<String> {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
    Ok(match (ty, v) {
        (ColumnType::Json, v) => quote(&v.to_string()),
        (ColumnType::Text, Value::String(s)) => quote(s),
        (ColumnType::Integer, v) if v.is_i64() => v.to_string(),
        (ColumnType::Real, v) if v.is_number() => v.to_string(),
        _ => {
            return Err(JobError::new(
                "config",
                "extension column default does not match its type",
            ))
        }
    })
}

/// SQL identifiers the crate interpolates must be plain lower-snake names.
pub(crate) fn check_ident(name: &str) -> JobResult<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(JobError::new(
            "config",
            format!("bad SQL identifier {name:?} (use [a-z_][a-z0-9_]*)"),
        ))
    }
}

/// App-declared status vocabulary. Terminal statuses are always
/// `complete`/`failed`/`cancelled` (so [`crate::Job::is_terminal`] stays
/// valid); the app names its non-terminal statuses and which moves between
/// them are legal. Any non-terminal status may move to any terminal one;
/// terminal statuses never move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    initial: String,
    non_terminal: Vec<String>,
    run_status: Option<String>,
    transitions: BTreeSet<(String, String)>,
    held: BTreeSet<String>,
}

impl Default for StatusModel {
    /// 0.1 vocabulary: `queued -> running`; the runner marks a job `running`.
    fn default() -> Self {
        Self::new(QUEUED, [QUEUED, RUNNING])
            .allow(QUEUED, RUNNING)
            .run_status(Some(RUNNING))
    }
}

impl StatusModel {
    /// `initial` is the status of a new job and must be one of `non_terminal`.
    /// No moves between non-terminal statuses are allowed until [`StatusModel::allow`].
    pub fn new<I, S>(initial: impl Into<String>, non_terminal: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            initial: initial.into(),
            non_terminal: non_terminal.into_iter().map(Into::into).collect(),
            run_status: None,
            transitions: BTreeSet::new(),
            held: BTreeSet::new(),
        }
    }
    /// Permit `from -> to` between non-terminal statuses.
    pub fn allow(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.transitions.insert((from.into(), to.into()));
        self
    }
    /// Status the [`crate::Runner`] sets when it starts a run (`None`: leave it).
    pub fn run_status(mut self, s: Option<&str>) -> Self {
        self.run_status = s.map(Into::into);
        self
    }
    /// Declare `status` (one of the non-terminal statuses) a held status. A held job is
    /// non-terminal but waits for a person or an outside check: it leaves its status only
    /// through [`crate::JobStore::resolve`] (every other move is refused with
    /// `illegal_transition`), the runner neither lists nor runs it, and `cancel`, `delete` and
    /// single-flight supersede skip or refuse it. A job enters one through
    /// [`crate::JobStore::hold_submission`].
    pub fn hold(mut self, status: impl Into<String>) -> Self {
        self.held.insert(status.into());
        self
    }
    /// Whether `s` is a held status (see [`StatusModel::hold`]).
    pub fn is_held(&self, s: &str) -> bool {
        self.held.contains(s)
    }
    pub fn initial(&self) -> &str {
        &self.initial
    }
    pub fn non_terminal(&self) -> &[String] {
        &self.non_terminal
    }
    pub fn run_status_name(&self) -> Option<&str> {
        self.run_status.as_deref()
    }
    pub fn is_terminal(status: &str) -> bool {
        matches!(status, COMPLETE | FAILED | CANCELLED)
    }
    pub fn is_known(&self, status: &str) -> bool {
        Self::is_terminal(status) || self.non_terminal.iter().any(|s| s == status)
    }
    /// Staying in the same status is not a transition and is always allowed
    /// for non-terminal jobs.
    pub fn can_move(&self, from: &str, to: &str) -> bool {
        if Self::is_terminal(from) || !self.is_known(from) || !self.is_known(to) {
            return false;
        }
        // A held status is left only through `JobStore::resolve`.
        if from != to && self.is_held(from) {
            return false;
        }
        from == to
            || Self::is_terminal(to)
            || self
                .transitions
                .contains(&(from.to_string(), to.to_string()))
    }
    pub(crate) fn validate(&self) -> JobResult<()> {
        let bad = |m: String| Err(JobError::new("config", m));
        if self.non_terminal.is_empty() {
            return bad("status model needs at least one non-terminal status".into());
        }
        for s in &self.non_terminal {
            if Self::is_terminal(s) || s.is_empty() || s.contains('\'') {
                return bad(format!("bad non-terminal status {s:?}"));
            }
        }
        if !self.non_terminal.contains(&self.initial) {
            return bad(format!(
                "initial status {:?} is not non-terminal",
                self.initial
            ));
        }
        if let Some(r) = &self.run_status {
            if !self.non_terminal.contains(r) {
                return bad(format!("run status {r:?} is not non-terminal"));
            }
        }
        for h in &self.held {
            if !self.non_terminal.contains(h) {
                return bad(format!("held status {h:?} is not non-terminal"));
            }
            if *h == self.initial || self.run_status.as_deref() == Some(h.as_str()) {
                return bad(format!("held status {h:?} is the initial or run status"));
            }
        }
        for (a, b) in &self.transitions {
            if !self.non_terminal.contains(a) || !self.non_terminal.contains(b) {
                return bad(format!("transition {a} -> {b} names an undeclared status"));
            }
        }
        Ok(())
    }
    /// Like [`StatusModel::sql_set`] without the held statuses (the jobs a runner may run).
    pub(crate) fn sql_set_unheld(&self) -> String {
        let items: Vec<String> = self
            .non_terminal
            .iter()
            .filter(|s| !self.held.contains(*s))
            .map(|s| format!("'{s}'"))
            .collect();
        format!("({})", items.join(","))
    }
    /// `('a','b')` for SQL `IN` (statuses are validated to contain no quote).
    pub(crate) fn sql_set(&self) -> String {
        let items: Vec<String> = self.non_terminal.iter().map(|s| format!("'{s}'")).collect();
        format!("({})", items.join(","))
    }
}

/// Artifact table and wire names. Default `artifacts(artifact_id)` with wire
/// field `artifact_id`; GenRight uses `assets(asset_id)` / `asset_id`. The
/// other columns (`job_id, path, sha256, bytes, mime, created_at`) are fixed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactNames {
    pub table: String,
    pub id_column: String,
    pub wire_id: String,
}

impl Default for ArtifactNames {
    fn default() -> Self {
        Self::new("artifacts", "artifact_id", "artifact_id")
    }
}

impl ArtifactNames {
    pub fn new(
        table: impl Into<String>,
        id_column: impl Into<String>,
        wire_id: impl Into<String>,
    ) -> Self {
        Self {
            table: table.into(),
            id_column: id_column.into(),
            wire_id: wire_id.into(),
        }
    }
}

/// App callback run inside the SAME SQLite write transaction as the store
/// change it observes. Returning `Err` rolls the whole change back and the
/// error is returned to the caller unchanged (e.g. GenRight's `spend_cap`).
///
/// `conn` is the open transaction: read and write app-owned tables through it.
/// Never call back into the [`crate::JobStore`] from a hook (it would deadlock).
pub trait StoreHook: Send + Sync {
    /// A new job row was inserted (not called for a deduplicated submit).
    fn on_create(&self, _conn: &Connection, _job: &JobRecord) -> JobResult<()> {
        Ok(())
    }
    /// Attempt `attempt` was counted for `job` (`job.job.attempts == attempt`):
    /// a runner attempt or a [`crate::Submission::Submit`] that is not a replay.
    fn on_attempt(&self, _conn: &Connection, _job: &JobRecord, _attempt: i64) -> JobResult<()> {
        Ok(())
    }
    /// The job's status changed from `before` to `after`.
    fn on_transition(
        &self,
        _conn: &Connection,
        _before: &JobRecord,
        _after: &JobRecord,
    ) -> JobResult<()> {
        Ok(())
    }
    /// A held job was resolved by [`crate::JobStore::resolve`], in the same transaction as the
    /// move itself. Called after `on_transition`, with `job` as it is after the resolution and
    /// `attempt` the submission attempt being resolved (`job.job.attempts`). An error rolls the
    /// whole resolution back and the job stays held.
    fn on_resolve(
        &self,
        _conn: &Connection,
        _job: &JobRecord,
        _attempt: i64,
        _r: &Resolution,
    ) -> JobResult<()> {
        Ok(())
    }
    /// A finished job was inserted by [`crate::JobStore::import_terminal`] (or its `_with` and
    /// `_batch` forms), in the same transaction as the insert and its artifact rows; `job` is the
    /// stored record, already terminal. An error rolls back the whole import (every record of a
    /// batch). Not called when an idempotent import finds an identical record already stored.
    ///
    /// An import calls this and none of `on_create`, `on_attempt` or `on_transition`: no job was
    /// created to run, no attempt was counted now, and no status changed. A ledger that keeps a
    /// mirror of finished jobs records them here.
    fn on_import(&self, _conn: &Connection, _job: &JobRecord) -> JobResult<()> {
        Ok(())
    }
}

/// How [`crate::JobControl::progress`] events are written. The default is today's behaviour:
/// every event is its own fully durable write. State transitions, results, runner lifecycle
/// events (`started`, `complete`, `paused`, `failed`, `cancelled`) and `Recovery::Never` markers
/// are always fully durable, whatever this says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressPolicy {
    /// Write progress events with `synchronous=NORMAL` (WAL only; ignored on a rollback
    /// journal or when the store was opened with `DurabilityProfile::Normal`). A power loss may
    /// lose the last progress events, never a transition: the next fully durable commit syncs
    /// the log they sit in.
    pub relaxed_durability: bool,
    /// Keep only the latest progress fraction per job within this interval (see
    /// [`crate::JobStore::progress_fenced`]). `None` writes every event.
    pub coalesce: Option<Duration>,
}

impl ProgressPolicy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn relaxed_durability(mut self, yes: bool) -> Self {
        self.relaxed_durability = yes;
        self
    }
    pub fn coalesce(mut self, interval: Duration) -> Self {
        self.coalesce = Some(interval);
        self
    }
}

/// How a [`crate::JobStore`] maps onto its database. `StoreConfig::default()`
/// is exactly the 0.1 store.
#[derive(Clone)]
pub struct StoreConfig {
    pub(crate) columns: Vec<ExtColumn>,
    pub(crate) statuses: StatusModel,
    pub(crate) artifacts: ArtifactNames,
    pub(crate) external_id: Option<String>,
    pub(crate) hook: Option<Arc<dyn StoreHook>>,
    pub(crate) app_schema: Vec<String>,
    pub(crate) user_version: Option<i64>,
    pub(crate) include_retryable: bool,
    pub(crate) progress: ProgressPolicy,
    pub(crate) artifact_roots: Vec<PathBuf>,
    pub(crate) scratch_root: Option<PathBuf>,
    pub(crate) work_roots: Vec<PathBuf>,
    pub(crate) kind_recovery: Vec<(String, Recovery)>,
    pub(crate) jobs_table: String,
    pub(crate) unique: Vec<Vec<String>>,
    pub(crate) revision: Option<String>,
    pub(crate) due: Option<String>,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            columns: vec![],
            statuses: StatusModel::default(),
            artifacts: ArtifactNames::default(),
            external_id: None,
            hook: None,
            app_schema: vec![],
            user_version: Some(1),
            include_retryable: true,
            progress: ProgressPolicy::default(),
            artifact_roots: vec![],
            scratch_root: None,
            work_roots: vec![],
            kind_recovery: vec![],
            jobs_table: DEFAULT_JOBS_TABLE.to_string(),
            unique: vec![],
            revision: None,
            due: None,
        }
    }
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreConfig")
            .field("columns", &self.columns)
            .field("statuses", &self.statuses)
            .field("artifacts", &self.artifacts)
            .field("external_id", &self.external_id)
            .field("hook", &self.hook.is_some())
            .field("app_schema", &self.app_schema.len())
            .field("user_version", &self.user_version)
            .field("include_retryable", &self.include_retryable)
            .field("progress", &self.progress)
            .field("artifact_roots", &self.artifact_roots)
            .field("scratch_root", &self.scratch_root)
            .field("work_roots", &self.work_roots)
            .field("kind_recovery", &self.kind_recovery)
            .field("jobs_table", &self.jobs_table)
            .field("unique", &self.unique)
            .field("revision", &self.revision)
            .field("due", &self.due)
            .finish()
    }
}

impl StoreConfig {
    pub fn new() -> Self {
        Self::default()
    }
    /// Declare an extension column on `jobs`. Missing columns are added to an
    /// existing database at open (a NOT NULL one needs a default to be added).
    pub fn column(mut self, c: ExtColumn) -> Self {
        self.columns.push(c);
        self
    }
    pub fn statuses(mut self, m: StatusModel) -> Self {
        self.statuses = m;
        self
    }
    pub fn artifacts(mut self, a: ArtifactNames) -> Self {
        self.artifacts = a;
        self
    }
    /// Name the (declared, nullable TEXT) extension column holding the
    /// external system's id for the current attempt (GenRight:
    /// `provider_job_id`). Enables [`crate::JobStore::begin_submission`].
    pub fn external_id(mut self, column: impl Into<String>) -> Self {
        self.external_id = Some(column.into());
        self
    }
    pub fn hook(mut self, h: Arc<dyn StoreHook>) -> Self {
        self.hook = Some(h);
        self
    }
    /// Idempotent app DDL (e.g. `CREATE TABLE IF NOT EXISTS ledger ...`) run at
    /// open. The crate never reads, writes, or drops app-owned tables.
    pub fn app_schema(mut self, sql: impl Into<String>) -> Self {
        self.app_schema.push(sql.into());
        self
    }
    /// `PRAGMA user_version` the store writes at open (default `Some(1)`).
    /// Pass `None` when the app manages `user_version` itself.
    pub fn user_version(mut self, v: Option<i64>) -> Self {
        self.user_version = v;
        self
    }
    /// Whether stored and served job errors carry `retryable` (default `true`).
    /// `false` keeps an app's `{code, message}` error shape: the key is never
    /// written, and is removed from errors already stored.
    pub fn include_retryable(mut self, yes: bool) -> Self {
        self.include_retryable = yes;
        self
    }
    /// Durability and coalescing of progress events (default: every event fully durable).
    pub fn progress_policy(mut self, p: ProgressPolicy) -> Self {
        self.progress = p;
        self
    }
    /// A directory the store may delete artifact files from (repeatable). With any root
    /// configured, `JobStore::delete` removes a job's artifact files, but only paths strictly
    /// inside a configured root (symlinks resolved); a recorded path outside every root makes
    /// the delete fail with `path_outside_root` and delete nothing. Must be absolute.
    pub fn artifact_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.artifact_roots.push(root.into());
        self
    }
    /// The directory holding per-job scratch directories (`<root>/<job_id>`, the layout
    /// `Runner::sweep_orphan_scratch` uses). `JobStore::delete` removes the job's directory.
    /// Artifacts recorded inside it are also deletable. Must be absolute.
    pub fn scratch_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.scratch_root = Some(root.into());
        self
    }
    /// A directory holding per-request work directories (`<root>/<request_id>`, for example an
    /// app's `<output>/_work/<request id>`), repeatable. The store treats `<root>/<request_id>`
    /// of every job that has a `request_id` as that job's own:
    /// - [`crate::JobStore::claimed_paths_with`] claims it (for [`crate::ClaimScope::NonTerminal`]
    ///   the directories of non-terminal jobs, for [`crate::ClaimScope::All`] those of every job
    ///   still in the store), so `Runner::sweep_unclaimed_with` over the root keeps it;
    /// - [`crate::JobStore::delete`] removes it with the job, and files recorded inside it are
    ///   deletable like those inside an artifact root. A request id that is not a single plain
    ///   path component gets no directory. `prune` never touches files.
    ///
    /// Nothing changes for a store without a work root. Must be absolute.
    pub fn work_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.work_roots.push(root.into());
        self
    }
    /// Recovery class for every job of `kind` (`NewJob::kind`), set once at configuration instead
    /// of at each submit. Use `Recovery::Never` for kinds whose effects must not repeat after a
    /// crash (uninstall, cleanup apply, upgrade): a job that began an attempt and lost its runner
    /// is failed as `interrupted` and never handed to a handler again. It applies when the submit
    /// leaves `SubmitOptions::recovery` at its default (`Resume`), so jobs submitted through the
    /// engine contract, which carries no class, are covered; an explicit `Never` is kept. The
    /// last call for a kind wins. A kind that is not named behaves as before.
    pub fn recovery_for_kind(mut self, kind: impl Into<String>, class: Recovery) -> Self {
        let kind = kind.into();
        self.kind_recovery.retain(|(k, _)| *k != kind);
        self.kind_recovery.push((kind, class));
        self
    }
    /// The class set by [`StoreConfig::recovery_for_kind`] for `kind`, if any.
    pub fn kind_recovery(&self, kind: &str) -> Option<Recovery> {
        self.kind_recovery
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, c)| *c)
    }
    /// Name the job table (default `jobs`) so an app that already owns a table of its own can
    /// keep its name (for example `publish_jobs`). The crate-owned side tables (`job_events`,
    /// `rk_job_meta`, `rk_job_submissions`) are keyed by job id and keep their names. A plain
    /// lower-snake identifier; it may not be one of the crate's own table names or the artifact
    /// table. An existing table of that name is adopted additively, as `jobs` always was.
    pub fn jobs_table(mut self, name: impl Into<String>) -> Self {
        self.jobs_table = name.into();
        self
    }
    /// An extra UNIQUE constraint over `columns` (core columns such as `kind` or declared
    /// extension columns), repeatable. Created at schema init as a unique index
    /// (`CREATE UNIQUE INDEX IF NOT EXISTS`), so it also lands on an existing table. A second
    /// row with the same values is refused by SQLite as a constraint error; `request_id` stays
    /// unique as before.
    pub fn unique<I, S>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.unique
            .push(columns.into_iter().map(Into::into).collect());
        self
    }
    /// Keep a row revision in `column` (declared here as `INTEGER NOT NULL DEFAULT 0`; do not
    /// declare it again). It is 0 on insert and rises by one on every write the crate makes to
    /// the job row, so a caller can pass the revision it read as [`crate::Expect::revision`]
    /// and get a `revision_conflict` error ([`crate::JobError::revision_conflict`]) instead of
    /// overwriting a newer write. Values are read from [`crate::JobRecord::ext`]; writing the
    /// column through `Ext` or `Transition` is refused.
    pub fn row_revision(mut self, column: impl Into<String>) -> Self {
        let name = column.into();
        if self.column_named(&name).is_none() {
            self.columns
                .push(ExtColumn::integer(name.clone()).not_null().default(0));
        }
        self.revision = Some(name);
        self
    }
    /// Name the declared extension column (TEXT or INTEGER) holding when a job is next due
    /// (for example `next_attempt_at`, an ISO-8601 UTC string or epoch seconds). Enables
    /// [`crate::JobStore::due`] and [`crate::JobTable::due`]: jobs whose column is not NULL and
    /// is `<=` an injected `now`, oldest due first.
    pub fn due_column(mut self, column: impl Into<String>) -> Self {
        self.due = Some(column.into());
        self
    }
    /// The job table name (see [`StoreConfig::jobs_table`]).
    pub fn jobs_table_name(&self) -> &str {
        &self.jobs_table
    }
    /// The row revision column, if [`StoreConfig::row_revision`] set one.
    pub fn revision_column(&self) -> Option<&str> {
        self.revision.as_deref()
    }
    /// The due-time column, if [`StoreConfig::due_column`] set one.
    pub fn due_column_name(&self) -> Option<&str> {
        self.due.as_deref()
    }
    pub fn work_roots(&self) -> &[PathBuf] {
        &self.work_roots
    }
    pub fn artifact_roots(&self) -> &[PathBuf] {
        &self.artifact_roots
    }
    pub fn scratch_root_path(&self) -> Option<&Path> {
        self.scratch_root.as_deref()
    }
    pub fn progress(&self) -> &ProgressPolicy {
        &self.progress
    }
    pub fn includes_retryable(&self) -> bool {
        self.include_retryable
    }
    pub fn columns(&self) -> &[ExtColumn] {
        &self.columns
    }
    pub fn status_model(&self) -> &StatusModel {
        &self.statuses
    }
    pub fn artifact_names(&self) -> &ArtifactNames {
        &self.artifacts
    }
    pub fn external_id_column(&self) -> Option<&str> {
        self.external_id.as_deref()
    }

    pub(crate) fn column_named(&self, name: &str) -> Option<&ExtColumn> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub(crate) fn validate(&self) -> JobResult<()> {
        self.statuses.validate()?;
        let mut seen = BTreeSet::new();
        for c in &self.columns {
            check_ident(&c.name)?;
            let wire_reserved = [
                "request",
                "error",
                "result",
                "deduplicated",
                "recoverable",
                "timed_out",
            ];
            if CORE_JOB_COLUMNS.contains(&c.name.as_str())
                || wire_reserved.contains(&c.name.as_str())
                || !seen.insert(c.name.clone())
            {
                return Err(JobError::new(
                    "config",
                    format!("extension column {} collides with another column", c.name),
                ));
            }
            c.ddl()?;
        }
        for r in self
            .artifact_roots
            .iter()
            .chain(self.scratch_root.iter())
            .chain(self.work_roots.iter())
        {
            if !r.is_absolute() {
                return Err(JobError::new(
                    "config",
                    format!(
                        "artifact, scratch and work roots must be absolute: {}",
                        r.display()
                    ),
                ));
            }
        }
        check_ident(&self.artifacts.table)?;
        check_ident(&self.artifacts.id_column)?;
        check_ident(&self.jobs_table)?;
        if [
            "job_events",
            "rk_job_submissions",
            "rk_job_meta",
            "rk_job_deleting",
        ]
        .contains(&self.jobs_table.as_str())
            || self.jobs_table == self.artifacts.table
        {
            return Err(JobError::new("config", "job table name is reserved"));
        }
        if let Some(r) = &self.revision {
            match self.column_named(r) {
                Some(c) if c.ty == ColumnType::Integer && c.not_null && !c.required() => {}
                _ => {
                    return Err(JobError::new(
                        "config",
                        format!("revision column {r} must be INTEGER NOT NULL with a default"),
                    ))
                }
            }
        }
        if let Some(d) = &self.due {
            match self.column_named(d) {
                Some(c) if matches!(c.ty, ColumnType::Text | ColumnType::Integer) => {}
                _ => {
                    return Err(JobError::new(
                        "config",
                        format!("due column {d} must be a declared text or integer column"),
                    ))
                }
            }
        }
        for u in &self.unique {
            if u.is_empty() {
                return Err(JobError::new("config", "unique constraint names no column"));
            }
            for col in u {
                check_ident(col)?;
                if !CORE_JOB_COLUMNS.contains(&col.as_str()) && self.column_named(col).is_none() {
                    return Err(JobError::new(
                        "config",
                        format!("unique constraint names unknown column {col}"),
                    ));
                }
            }
        }
        if [
            "jobs",
            "job_events",
            "rk_job_submissions",
            "rk_job_meta",
            "rk_job_deleting",
        ]
        .contains(&self.artifacts.table.as_str())
        {
            return Err(JobError::new("config", "artifact table name is reserved"));
        }
        if self.artifacts.wire_id.is_empty() {
            return Err(JobError::new("config", "artifact wire id is empty"));
        }
        if let Some(x) = &self.external_id {
            match self.column_named(x) {
                Some(c) if c.ty == ColumnType::Text && !c.not_null => {}
                _ => {
                    return Err(JobError::new(
                        "config",
                        format!("external id {x} must be a declared nullable text column"),
                    ))
                }
            }
        }
        Ok(())
    }
}
