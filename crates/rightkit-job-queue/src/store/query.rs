//! Typed queries over the job table on a caller's connection: [`JobTable::query`] and
//! [`JobTable::count`].
//!
//! A [`JobQuery`] is a closed vocabulary, not SQL. Every identifier that reaches the statement is
//! a column name the [`StoreConfig`](crate::StoreConfig) declared (validated when it was built)
//! or a fixed crate column; every value is a bound parameter. The filters are in the `WHERE`
//! clause, so they apply before the `LIMIT`: a page holds the first `limit` rows that match,
//! never the first `limit` rows of the table that happen to match.
//!
//! Order is total: the sort key, then `job_id` in the same direction. `job_id` is the table's
//! primary key, so two rows never tie, and a [`JobCursor`] (the sort key and `job_id` of the last
//! row of a page) names a position that no later write to another row can move. The key is read
//! when the row is read, so a row whose own sort key changes between two pages may be seen in
//! its old or new place, as with [`JobStore::list_page`](super::JobStore::list_page).
//!
//! A cursor carries a fingerprint of the table, the filters and the order it was issued for.
//! Passing it to a different query fails with `cursor_mismatch`. The `limit` is not part of the
//! fingerprint, so it may change between pages.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use rightkit_db::rusqlite::types::Value as SqlValue;
use rightkit_db::rusqlite::{params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{JobRecord, JobTable, NCORE};
use crate::config::ColumnType;
use crate::error::{JobError, JobResult, CURSOR_MISMATCH, INVALID_CURSOR};

/// Page size of a [`JobQuery`] that sets none.
pub const DEFAULT_QUERY_LIMIT: usize = 50;
/// Largest page size [`JobTable::query`] accepts; a larger `limit` is refused (`bad_input`),
/// never clamped. Read past one page with the cursor.
pub const MAX_QUERY_LIMIT: usize = 10_000;

/// Cursor wire version.
const VERSION: &str = "rq1";
// No fixed length cap. A cursor hex-encodes the sort key and the `job_id` of one row, and a
// declared TEXT key or a job id has no length bound of its own, so any cap shorter than the
// longest row the table accepts would refuse a cursor this crate issued (the page walk would
// die at the first long key). The decoder accepts exactly the shape `JobCursor::at` issues:
// four fields, a hex fingerprint of fixed width and hex-encoded key and id. Decoding is linear
// in a string the caller already holds.

/// The order of a [`JobQuery`]: a fixed set, never free text. Ties break by `job_id` in the same
/// direction as the key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobOrder {
    /// Oldest first by `created_at`.
    CreatedAsc,
    /// Newest first by `created_at` (the default).
    CreatedDesc,
    /// Oldest due first by the store's [`StoreConfig::due_column`](crate::StoreConfig::due_column).
    /// Rows whose due column is NULL are not in the result (as in [`JobTable::due`]); the count
    /// leaves them out too, so the pages add up to the count.
    DueAsc,
    /// Ascending by `COALESCE(column, fallback)`. `column` is a declared TEXT, INTEGER or REAL
    /// extension column (it may be nullable). `fallback` is a declared NOT NULL extension
    /// column of the same type, or the core `created_at` when `column` is TEXT, so the key is
    /// never NULL.
    CoalesceAsc { column: String, fallback: String },
    /// [`JobOrder::CoalesceAsc`], descending (newest first for a timestamp).
    CoalesceDesc { column: String, fallback: String },
}

impl JobOrder {
    /// `COALESCE(column, fallback)` ascending.
    pub fn coalesce_asc(column: impl Into<String>, fallback: impl Into<String>) -> Self {
        JobOrder::CoalesceAsc {
            column: column.into(),
            fallback: fallback.into(),
        }
    }
    /// `COALESCE(column, fallback)` descending.
    pub fn coalesce_desc(column: impl Into<String>, fallback: impl Into<String>) -> Self {
        JobOrder::CoalesceDesc {
            column: column.into(),
            fallback: fallback.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Filter {
    Eq(String, Value),
    IsNull(String),
    IsNotNull(String),
}

/// A typed read of the job table: filters, an order, a page size and optionally the cursor of
/// the previous page. Build it with the methods below; everything is checked against the
/// table's [`StoreConfig`](crate::StoreConfig) when it runs. All filters combine with AND.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct JobQuery {
    statuses: Option<BTreeSet<String>>,
    filters: Vec<Filter>,
    attempts_below: Option<i64>,
    attempts_at_least: Option<i64>,
    order: JobOrder,
    limit: usize,
    after: Option<JobCursor>,
}

impl Default for JobQuery {
    fn default() -> Self {
        Self {
            statuses: None,
            filters: Vec::new(),
            attempts_below: None,
            attempts_at_least: None,
            order: JobOrder::CreatedDesc,
            limit: DEFAULT_QUERY_LIMIT,
            after: None,
        }
    }
}

impl JobQuery {
    pub fn new() -> Self {
        Self::default()
    }
    /// Only jobs in one of these statuses (`status IN (...)`); repeated calls add to the set.
    /// Each must be a status of the table's model (`bad_input` when the query runs otherwise).
    /// An empty set matches nothing.
    pub fn status_in<I, S>(mut self, statuses: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.statuses
            .get_or_insert_with(BTreeSet::new)
            .extend(statuses.into_iter().map(Into::into));
        self
    }
    /// Only jobs whose declared extension column `column` equals `value`, compared as the
    /// column's own type (a TEXT column against a string, INTEGER against an integer, REAL
    /// against a number). A JSON column cannot be compared for equality, and `null` is refused
    /// (`bad_input`): use [`JobQuery::ext_null`].
    pub fn ext_eq(mut self, column: impl Into<String>, value: impl Into<Value>) -> Self {
        self.filters.push(Filter::Eq(column.into(), value.into()));
        self
    }
    /// Only jobs whose declared extension column `column` is NULL.
    pub fn ext_null(mut self, column: impl Into<String>) -> Self {
        self.filters.push(Filter::IsNull(column.into()));
        self
    }
    /// Only jobs whose declared extension column `column` is not NULL.
    pub fn ext_not_null(mut self, column: impl Into<String>) -> Self {
        self.filters.push(Filter::IsNotNull(column.into()));
        self
    }
    /// Only jobs that counted fewer than `n` attempts (`attempts < n`).
    pub fn attempts_below(mut self, n: i64) -> Self {
        self.attempts_below = Some(n);
        self
    }
    /// Only jobs that counted at least `n` attempts (`attempts >= n`).
    pub fn attempts_at_least(mut self, n: i64) -> Self {
        self.attempts_at_least = Some(n);
        self
    }
    /// The order of the result (default [`JobOrder::CreatedDesc`]).
    pub fn order(mut self, order: JobOrder) -> Self {
        self.order = order;
        self
    }
    /// The page size, `1..=`[`MAX_QUERY_LIMIT`] (default [`DEFAULT_QUERY_LIMIT`]). A value
    /// outside the range is refused when the query runs. [`JobTable::count`] ignores it.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
    /// Start after the position of `cursor`, which a previous page of this same query gave.
    /// [`JobTable::count`] ignores it.
    pub fn after(mut self, cursor: JobCursor) -> Self {
        self.after = Some(cursor);
        self
    }
}

/// One page of a [`JobQuery`]: the rows and where the next page starts.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct JobPage {
    /// Up to `limit` rows, in the query's order.
    pub records: Vec<JobRecord>,
    /// Pass to [`JobQuery::after`] on the same query to read the next page. `None` when this
    /// page is the last: no further row matched when the page was read.
    pub next: Option<JobCursor>,
}

/// An opaque position in a [`JobQuery`] result (see [`JobTable::query`]).
///
/// A string: [`JobCursor::as_str`] / `Display` give it, [`JobCursor::parse`] / `FromStr` read it
/// back, and with `serde` it is a plain JSON string. It encodes the sort key and `job_id` of the
/// last row of a page and a fingerprint of the table, filters and order it was issued for, so a
/// query that differs in any of them refuses it with `cursor_mismatch`
/// ([`crate::CURSOR_MISMATCH`]). Not a secret and not signed. The text format is not a
/// contract; treat it as opaque. Cursors do not expire.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobCursor(String);

#[derive(Debug, Clone, PartialEq)]
enum KeyVal {
    Int(i64),
    Real(f64),
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyTy {
    Int,
    Real,
    Text,
}

impl KeyTy {
    fn of(ty: ColumnType) -> Option<KeyTy> {
        match ty {
            ColumnType::Integer => Some(KeyTy::Int),
            ColumnType::Real => Some(KeyTy::Real),
            ColumnType::Text => Some(KeyTy::Text),
            ColumnType::Json => None,
        }
    }
}

impl KeyVal {
    fn ty(&self) -> KeyTy {
        match self {
            KeyVal::Int(_) => KeyTy::Int,
            KeyVal::Real(_) => KeyTy::Real,
            KeyVal::Text(_) => KeyTy::Text,
        }
    }
    fn to_sql(&self) -> SqlValue {
        match self {
            KeyVal::Int(i) => SqlValue::Integer(*i),
            KeyVal::Real(f) => SqlValue::Real(*f),
            KeyVal::Text(t) => SqlValue::Text(t.clone()),
        }
    }
    fn encode(&self) -> String {
        match self {
            KeyVal::Int(i) => format!("i{i}"),
            KeyVal::Real(f) => format!("r{:016x}", f.to_bits()),
            KeyVal::Text(t) => format!("t{}", to_hex(t)),
        }
    }
    fn decode(s: &str) -> Option<KeyVal> {
        let (tag, rest) = (s.get(..1)?, s.get(1..)?);
        match tag {
            "i" => rest.parse().ok().map(KeyVal::Int),
            "r" if rest.len() == 16 => u64::from_str_radix(rest, 16)
                .ok()
                .map(|b| KeyVal::Real(f64::from_bits(b))),
            "t" => from_hex(rest).map(KeyVal::Text),
            _ => None,
        }
    }
    fn from_sql(v: SqlValue, ty: KeyTy) -> JobResult<KeyVal> {
        match (ty, v) {
            (KeyTy::Int, SqlValue::Integer(i)) => Ok(KeyVal::Int(i)),
            (KeyTy::Real, SqlValue::Real(f)) => Ok(KeyVal::Real(f)),
            (KeyTy::Real, SqlValue::Integer(i)) => Ok(KeyVal::Real(i as f64)),
            (KeyTy::Text, SqlValue::Text(t)) => Ok(KeyVal::Text(t)),
            _ => Err(JobError::new(
                "schema",
                "a row's sort key is NULL or not of its column's declared type",
            )),
        }
    }
}

struct Pos {
    fp: String,
    key: KeyVal,
    job_id: String,
}

impl JobCursor {
    fn at(fp: &str, key: &KeyVal, job_id: &str) -> Self {
        JobCursor(format!(
            "{VERSION}.{fp}.{}.{}",
            key.encode(),
            to_hex(job_id)
        ))
    }

    /// The cursor as a string (stable for the life of the position; opaque).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Read a cursor back from the string [`JobCursor::as_str`] gave. Checks the shape only; the
    /// query is checked when the cursor is used. A malformed string fails with `invalid_cursor`
    /// ([`crate::INVALID_CURSOR`]).
    pub fn parse(s: &str) -> JobResult<JobCursor> {
        let c = JobCursor(s.to_string());
        c.decode()?;
        Ok(c)
    }

    fn decode(&self) -> JobResult<Pos> {
        let bad = |why: &str| JobError::new(INVALID_CURSOR, format!("not a query cursor: {why}"));
        let s = self.0.as_str();
        let parts: Vec<&str> = s.split('.').collect();
        let &[version, fp, key, id] = parts.as_slice() else {
            return Err(bad("wrong number of fields"));
        };
        if version != VERSION {
            return Err(bad("unknown version"));
        }
        if fp.len() != 32 || !fp.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad("bad fingerprint"));
        }
        Ok(Pos {
            fp: fp.to_string(),
            key: KeyVal::decode(key).ok_or_else(|| bad("bad sort key"))?,
            job_id: from_hex(id).ok_or_else(|| bad("bad job id"))?,
        })
    }
}

impl fmt::Display for JobCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for JobCursor {
    type Err = JobError;
    fn from_str(s: &str) -> JobResult<JobCursor> {
        JobCursor::parse(s)
    }
}

impl AsRef<str> for JobCursor {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<JobCursor> for String {
    fn from(c: JobCursor) -> String {
        c.0
    }
}

fn to_hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    let bytes = (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

/// FNV-1a over `bytes` from `seed`.
fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut h = seed;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 128 bits (two FNV-1a runs from different seeds) as 32 hex digits. Detects a cursor used with
/// another query; it is not a security boundary.
fn fingerprint(canonical: &str) -> String {
    format!(
        "{:016x}{:016x}",
        fnv1a(0xcbf2_9ce4_8422_2325, canonical.as_bytes()),
        fnv1a(0x8422_2325_cbf2_9ce4, canonical.as_bytes())
    )
}

/// Append `s` to `buf` length-prefixed, so field boundaries are unambiguous.
fn field(buf: &mut String, s: &str) {
    buf.push_str(&s.len().to_string());
    buf.push(':');
    buf.push_str(s);
}

/// The validated, bound form of a [`JobQuery`] without its cursor and limit.
struct Plan {
    conds: Vec<String>,
    args: Vec<SqlValue>,
    key: String,
    key_ty: KeyTy,
    desc: bool,
    fp: String,
}

fn key_literal(v: &SqlValue) -> String {
    match v {
        SqlValue::Integer(i) => format!("i{i}"),
        SqlValue::Real(f) => format!("r{:016x}", f.to_bits()),
        SqlValue::Text(t) => format!("t{t}"),
        SqlValue::Null => "n".to_string(),
        SqlValue::Blob(_) => "b".to_string(),
    }
}

impl JobTable {
    fn plan_query(&self, q: &JobQuery) -> JobResult<Plan> {
        let cfg = self.config();
        let model = cfg.status_model();
        let mut conds: Vec<String> = Vec::new();
        let mut args: Vec<SqlValue> = Vec::new();
        let mut canon = String::new();
        field(&mut canon, self.name());

        match &q.statuses {
            None => field(&mut canon, "s*"),
            Some(set) => {
                for s in set {
                    if !model.is_known(s) {
                        return Err(JobError::bad_input(format!(
                            "{s} is not a status of this table's model"
                        )));
                    }
                }
                field(&mut canon, "s");
                if set.is_empty() {
                    conds.push("0".to_string());
                } else {
                    let mut marks = Vec::new();
                    for s in set {
                        args.push(SqlValue::Text(s.clone()));
                        marks.push(format!("?{}", args.len()));
                        field(&mut canon, s);
                    }
                    conds.push(format!("status IN ({})", marks.join(",")));
                }
            }
        }

        let mut filters: Vec<String> = Vec::new();
        for f in &q.filters {
            let (col, kind) = match f {
                Filter::Eq(c, _) => (c, "eq"),
                Filter::IsNull(c) => (c, "null"),
                Filter::IsNotNull(c) => (c, "notnull"),
            };
            let c = cfg
                .column_named(col)
                .ok_or_else(|| JobError::bad_input(format!("unknown ext column: {col}")))?;
            let mut item = String::new();
            field(&mut item, kind);
            field(&mut item, &c.name);
            match f {
                Filter::Eq(_, v) => {
                    if v.is_null() {
                        return Err(JobError::bad_input(format!(
                            "ext.{col} equality needs a value; use ext_null for NULL"
                        )));
                    }
                    if c.ty == ColumnType::Json {
                        return Err(JobError::bad_input(format!(
                            "ext.{col} is a JSON column and cannot be compared for equality"
                        )));
                    }
                    let enc = c.encode(v)?;
                    field(&mut item, &key_literal(&enc));
                    args.push(enc);
                    conds.push(format!("{}=?{}", c.name, args.len()));
                }
                Filter::IsNull(_) => conds.push(format!("{} IS NULL", c.name)),
                Filter::IsNotNull(_) => conds.push(format!("{} IS NOT NULL", c.name)),
            }
            filters.push(item);
        }
        filters.sort();
        field(&mut canon, "f");
        for item in &filters {
            field(&mut canon, item);
        }

        field(&mut canon, "a");
        field(&mut canon, &format!("{:?}", q.attempts_below));
        field(&mut canon, &format!("{:?}", q.attempts_at_least));
        if let Some(n) = q.attempts_below {
            args.push(SqlValue::Integer(n));
            conds.push(format!("attempts<?{}", args.len()));
        }
        if let Some(n) = q.attempts_at_least {
            args.push(SqlValue::Integer(n));
            conds.push(format!("attempts>=?{}", args.len()));
        }

        field(&mut canon, "o");
        let (key, key_ty, desc) = match &q.order {
            JobOrder::CreatedAsc => {
                field(&mut canon, "created_asc");
                ("created_at".to_string(), KeyTy::Text, false)
            }
            JobOrder::CreatedDesc => {
                field(&mut canon, "created_desc");
                ("created_at".to_string(), KeyTy::Text, true)
            }
            JobOrder::DueAsc => {
                let col = cfg.due_column_name().ok_or_else(|| {
                    JobError::new(
                        "not_configured",
                        "store has no due column (StoreConfig::due_column)",
                    )
                })?;
                let c = cfg.column_named(col).ok_or_else(|| {
                    JobError::new("config", format!("due column {col} is not declared"))
                })?;
                let ty = KeyTy::of(c.ty).ok_or_else(|| {
                    JobError::new("config", format!("due column {col} is not TEXT or INTEGER"))
                })?;
                field(&mut canon, "due_asc");
                field(&mut canon, &c.name);
                conds.push(format!("{} IS NOT NULL", c.name));
                (c.name.clone(), ty, false)
            }
            JobOrder::CoalesceAsc { column, fallback }
            | JobOrder::CoalesceDesc { column, fallback } => {
                let desc = matches!(q.order, JobOrder::CoalesceDesc { .. });
                let c = cfg
                    .column_named(column)
                    .ok_or_else(|| JobError::bad_input(format!("unknown ext column: {column}")))?;
                let ty = KeyTy::of(c.ty).ok_or_else(|| {
                    JobError::bad_input(format!("ext.{column} is a JSON column and cannot order"))
                })?;
                if fallback == "created_at" {
                    if ty != KeyTy::Text {
                        return Err(JobError::bad_input(format!(
                            "created_at is TEXT; ext.{column} is not, so it cannot be its fallback"
                        )));
                    }
                } else {
                    let f = cfg.column_named(fallback).ok_or_else(|| {
                        JobError::bad_input(format!("unknown ext column: {fallback}"))
                    })?;
                    if f.ty != c.ty {
                        return Err(JobError::bad_input(format!(
                            "ext.{fallback} must be the same type as ext.{column}"
                        )));
                    }
                    if !f.not_null {
                        return Err(JobError::bad_input(format!(
                            "ext.{fallback} must be NOT NULL so the sort key is never NULL"
                        )));
                    }
                }
                field(
                    &mut canon,
                    if desc {
                        "coalesce_desc"
                    } else {
                        "coalesce_asc"
                    },
                );
                field(&mut canon, &c.name);
                field(&mut canon, fallback);
                (format!("COALESCE({}, {fallback})", c.name), ty, desc)
            }
        };

        Ok(Plan {
            conds,
            args,
            key,
            key_ty,
            desc,
            fp: fingerprint(&canon),
        })
    }

    fn where_clause(conds: &[String]) -> String {
        if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        }
    }

    /// One page of the jobs matching `q`, in its order, starting after `q`'s cursor if it has
    /// one. The filters are in the `WHERE` clause, so they apply before the limit; the rows are
    /// ordered by the sort key and then `job_id` (same direction), a total order. At most
    /// `q`'s limit rows (`1..=`[`MAX_QUERY_LIMIT`]); [`JobPage::next`] is set when a further row
    /// matched. Reads only; changes nothing.
    ///
    /// ```text
    /// let mut q = JobQuery::new().status_in(["queued"]).limit(100);
    /// loop {
    ///     let page = table.query(&conn, &q)?;
    ///     show(&page.records);
    ///     match page.next {
    ///         Some(c) => q = q.after(c),
    ///         None => break,
    ///     }
    /// }
    /// ```
    ///
    /// Errors: `bad_input` for an unknown status or column, a value that does not fit its
    /// column, an invalid combination (see [`JobOrder`]) or a limit out of range;
    /// `not_configured` for [`JobOrder::DueAsc`] without a due column; `invalid_cursor` for a
    /// malformed cursor; `cursor_mismatch` for a cursor issued for a different table, filters or
    /// order; `schema` when a row's sort key is NULL or of another type than its column declares
    /// (a row written outside this crate).
    pub fn query(&self, conn: &Connection, q: &JobQuery) -> JobResult<JobPage> {
        if q.limit == 0 || q.limit > MAX_QUERY_LIMIT {
            return Err(JobError::bad_input(format!(
                "limit must be 1..={MAX_QUERY_LIMIT}, not {}",
                q.limit
            )));
        }
        let mut plan = self.plan_query(q)?;
        if let Some(c) = &q.after {
            let pos = c.decode()?;
            if pos.fp != plan.fp {
                return Err(JobError::new(
                    CURSOR_MISMATCH,
                    "the cursor was issued for a different table, filter or order than this query",
                ));
            }
            if pos.key.ty() != plan.key_ty {
                return Err(JobError::new(
                    INVALID_CURSOR,
                    "not a query cursor: sort key of the wrong type",
                ));
            }
            plan.args.push(pos.key.to_sql());
            plan.args.push(SqlValue::Text(pos.job_id));
            let n = plan.args.len();
            plan.conds.push(format!(
                "({}, job_id) {} (?{}, ?{n})",
                plan.key,
                if plan.desc { "<" } else { ">" },
                n - 1
            ));
        }
        let dir = if plan.desc { "DESC" } else { "ASC" };
        plan.args.push(SqlValue::Integer(q.limit as i64 + 1));
        let sql = format!(
            "SELECT {sel}, {key} FROM {t}{wh} ORDER BY {key} {dir}, job_id {dir} LIMIT ?{n}",
            sel = self.select(),
            key = plan.key,
            t = self.name(),
            wh = Self::where_clause(&plan.conds),
            n = plan.args.len(),
        );
        let key_col = NCORE + self.config().columns().len();
        let mut st = conn.prepare(&sql)?;
        let mut rows = st
            .query_map(params_from_iter(plan.args), |r| {
                Ok((self.record_row(r)?, r.get::<_, SqlValue>(key_col)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let more = rows.len() > q.limit;
        rows.truncate(q.limit);
        let mut records = Vec::with_capacity(rows.len());
        let mut last: Option<(KeyVal, String)> = None;
        for (rec, key) in rows {
            let key = KeyVal::from_sql(key, plan.key_ty)?;
            last = Some((key, rec.job.job_id.clone()));
            records.push(rec);
        }
        let next = match (more, last) {
            (true, Some((key, id))) => Some(JobCursor::at(&plan.fp, &key, &id)),
            _ => None,
        };
        Ok(JobPage { records, next })
    }

    /// How many jobs match `q`'s filters. Uncapped: `q`'s limit and cursor are ignored, so the
    /// count is the total over every page of the same query (a [`JobOrder::DueAsc`] query counts
    /// only rows with a due time, like its pages). Validates the filters and order as
    /// [`JobTable::query`] does.
    pub fn count(&self, conn: &Connection, q: &JobQuery) -> JobResult<u64> {
        let plan = self.plan_query(q)?;
        let n: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM {}{}",
                self.name(),
                Self::where_clause(&plan.conds)
            ),
            params_from_iter(plan.args),
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }
}
