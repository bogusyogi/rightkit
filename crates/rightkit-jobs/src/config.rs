//! Store extension points: per-job extension columns, a pluggable status
//! vocabulary, a transactional hook, artifact table/wire names, and app-owned
//! schema. Everything defaults to the 0.1 behaviour, so `JobStore::open` is
//! unchanged.

use std::collections::BTreeSet;
use std::sync::Arc;

use rightkit_db::rusqlite::{self, Connection};
use serde_json::{Map, Value};

use crate::error::{JobError, JobResult};
use crate::store::{JobRecord, CANCELLED, COMPLETE, FAILED, QUEUED, RUNNING};

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
        for (a, b) in &self.transitions {
            if !self.non_terminal.contains(a) || !self.non_terminal.contains(b) {
                return bad(format!("transition {a} -> {b} names an undeclared status"));
            }
        }
        Ok(())
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
        check_ident(&self.artifacts.table)?;
        check_ident(&self.artifacts.id_column)?;
        if ["jobs", "job_events", "rk_job_submissions"].contains(&self.artifacts.table.as_str()) {
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
