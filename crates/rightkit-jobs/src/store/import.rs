//! Atomic import of jobs that already finished: [`JobStore::import_terminal`].
//!
//! A legacy import that does `submit` and then `transition` makes two commits; a crash between
//! them leaves a queued row that a runner would pick up. This inserts the row directly in its
//! terminal state, with its error, result, extension values and artifact rows, in one
//! transaction. The job is never queued and never handed to a handler: there is no queue in the
//! store, `non_terminal_ids` and the runner's recovery do not list it, and no event is written
//! (in particular no `started`).

use std::collections::HashSet;

use rightkit_db::rusqlite::types::Value as SqlValue;
use rightkit_db::rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use rightkit_db::write_txn;
use serde_json::Value;

use super::{artifact_row, format_utc, now, Artifact, JobRecord, JobStore, COMPLETE, FAILED};
use crate::config::{Ext, StatusModel};
use crate::error::{JobError, JobResult, JOB_EXISTS};

/// Most records one [`JobStore::import_terminal_batch`] call accepts. A larger import is sent in
/// several calls (each is atomic on its own; make them idempotent to re-run an interrupted one).
pub const MAX_IMPORT_BATCH: usize = 1000;

/// A finished job to insert as it is. Build it with [`ImportRecord::complete`],
/// [`ImportRecord::failed`] or [`ImportRecord::cancelled`] and the setters; every field is also
/// public to assign.
///
/// The record is inserted exactly as given: no attempt is counted, no transition runs, the
/// attempt generation is 0, and `created_at` / `updated_at` are the values supplied. Import
/// oldest first: rows with the same `created_at` list in insertion order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ImportRecord {
    /// The job id to store the row under (a single path-safe token: no `/`, `\`, `:` or control
    /// character). `None` generates one. An idempotent import recognises an earlier import by
    /// this id, or by `request_id` when there is no id.
    pub id: Option<String>,
    /// The caller's own key (`NewJob::request_id`); unique in the table.
    pub request_id: Option<String>,
    pub kind: String,
    /// The job's payload (`NewJob::request`); `Value::Null` for none.
    pub request: Value,
    /// `complete`, `failed` or `cancelled`. Anything else is refused.
    pub status: String,
    /// Attempts the job used (recorded as is; the hook is not called).
    pub attempts: i64,
    /// A `failed` job needs its error; a `complete` job has none; a `cancelled` job may have one
    /// (a superseded job carries `superseded`).
    pub error: Option<JobError>,
    /// The result of a `complete` job.
    pub result: Option<Value>,
    /// `YYYY-MM-DDTHH:MM:SSZ` (UTC), the format the store writes and sorts by. `None`: the time
    /// of the import, or `updated_at` when that is given.
    pub created_at: Option<String>,
    /// Same format. `None`: `created_at`.
    pub updated_at: Option<String>,
    /// Extension column values, validated against the store's [`crate::StoreConfig`] exactly as
    /// a submit validates them: unknown columns, wrongly typed values and missing required
    /// columns are refused.
    pub ext: Ext,
    /// Artifact rows of the job. An artifact's `job_id` may be left empty or must equal `id`.
    pub artifacts: Vec<Artifact>,
}

impl ImportRecord {
    /// A record in `status`. [`JobStore::import_terminal`] refuses anything but `complete`,
    /// `failed` and `cancelled`.
    pub fn new(kind: impl Into<String>, status: impl Into<String>) -> Self {
        Self {
            id: None,
            request_id: None,
            kind: kind.into(),
            request: Value::Null,
            status: status.into(),
            attempts: 0,
            error: None,
            result: None,
            created_at: None,
            updated_at: None,
            ext: Ext::new(),
            artifacts: Vec::new(),
        }
    }
    /// A `complete` job with its result.
    pub fn complete(kind: impl Into<String>, result: Value) -> Self {
        let mut r = Self::new(kind, COMPLETE);
        r.result = Some(result);
        r
    }
    /// A `failed` job with its error.
    pub fn failed(kind: impl Into<String>, error: JobError) -> Self {
        let mut r = Self::new(kind, FAILED);
        r.error = Some(error);
        r
    }
    /// A `cancelled` job.
    pub fn cancelled(kind: impl Into<String>) -> Self {
        Self::new(kind, super::CANCELLED)
    }
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
    pub fn request(mut self, request: Value) -> Self {
        self.request = request;
        self
    }
    pub fn attempts(mut self, attempts: i64) -> Self {
        self.attempts = attempts;
        self
    }
    /// The error of a `cancelled` job (or replace a `failed` job's).
    pub fn error(mut self, error: JobError) -> Self {
        self.error = Some(error);
        self
    }
    pub fn created_at(mut self, utc: impl Into<String>) -> Self {
        self.created_at = Some(utc.into());
        self
    }
    pub fn updated_at(mut self, utc: impl Into<String>) -> Self {
        self.updated_at = Some(utc.into());
        self
    }
    /// `created_at` from Unix seconds.
    pub fn created_at_unix(self, secs: i64) -> Self {
        self.created_at(format_utc(secs))
    }
    /// `updated_at` from Unix seconds.
    pub fn updated_at_unix(self, secs: i64) -> Self {
        self.updated_at(format_utc(secs))
    }
    /// Set one extension column.
    pub fn set(mut self, column: impl Into<String>, v: impl Into<Value>) -> Self {
        self.ext.insert(column.into(), v.into());
        self
    }
    pub fn ext(mut self, ext: Ext) -> Self {
        self.ext = ext;
        self
    }
    pub fn artifact(mut self, a: Artifact) -> Self {
        self.artifacts.push(a);
        self
    }
}

/// Options for [`JobStore::import_terminal_with`] and [`JobStore::import_terminal_batch`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportOptions {
    /// An existing job that is identical to the record counts as success (nothing is written and
    /// [`Imported::existed`] is true), so an interrupted import can be run again. A job that
    /// exists and differs is still refused with `job_exists`. The record needs an `id` or a
    /// `request_id` to be recognised by. Identical means every field the record states equals
    /// the stored one: kind, request id, payload, status, attempts, error, result, the extension
    /// columns it names, its artifacts, and its timestamps when it gives them.
    pub idempotent: bool,
}

impl ImportOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// See [`ImportOptions::idempotent`] (the field).
    pub fn idempotent(mut self) -> Self {
        self.idempotent = true;
        self
    }
}

/// What one imported record became.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Imported {
    /// The stored job.
    pub record: JobRecord,
    /// An identical job was already stored (idempotent import); nothing was written for it.
    pub existed: bool,
}

/// A record that passed validation, with its extension values encoded.
struct Prepared<'a> {
    rec: &'a ImportRecord,
    names: Vec<String>,
    vals: Vec<SqlValue>,
    /// The extension values as a read gives them back, for the idempotent comparison.
    canon: Vec<(String, Value)>,
    error: Option<Value>,
}

impl JobStore {
    /// Insert one finished job in a single transaction and return it. Refuses a non-terminal
    /// status and any invalid field with `bad_input`, and an id (or request id) that is already
    /// stored with `job_exists` ([`crate::JOB_EXISTS`]); nothing is written then.
    ///
    /// The insert, the extension values, the artifact rows and [`crate::StoreHook::on_import`]
    /// commit together or not at all. The job is never queued or run, no event is written and
    /// none of `on_create`, `on_attempt`, `on_transition` is called. See [`ImportRecord`].
    pub fn import_terminal(&self, rec: &ImportRecord) -> JobResult<JobRecord> {
        let mut out =
            self.import_many(std::slice::from_ref(rec), &ImportOptions::default(), false)?;
        out.pop()
            .map(|i| i.record)
            .ok_or_else(|| JobError::new("db", "import stored no record"))
    }

    /// [`JobStore::import_terminal`] with options; `ImportOptions::new().idempotent()` makes it
    /// safe to run again after an interruption.
    pub fn import_terminal_with(
        &self,
        rec: &ImportRecord,
        o: &ImportOptions,
    ) -> JobResult<Imported> {
        let mut out = self.import_many(std::slice::from_ref(rec), o, false)?;
        out.pop()
            .ok_or_else(|| JobError::new("db", "import stored no record"))
    }

    /// Import up to [`MAX_IMPORT_BATCH`] records in ONE transaction, in the order given (so give
    /// them oldest first). Every record is validated before anything is written, then the whole
    /// batch commits or none of it does: a failure of any record (a duplicate, a hook error, a
    /// database error) leaves the store as it was. The error names the record by position and id.
    /// A batch over the bound is refused (`bad_input`); an empty batch returns an empty list.
    pub fn import_terminal_batch(
        &self,
        recs: &[ImportRecord],
        o: &ImportOptions,
    ) -> JobResult<Vec<Imported>> {
        self.import_many(recs, o, true)
    }

    fn import_many(
        &self,
        recs: &[ImportRecord],
        o: &ImportOptions,
        label: bool,
    ) -> JobResult<Vec<Imported>> {
        if recs.is_empty() {
            return Ok(Vec::new());
        }
        if recs.len() > MAX_IMPORT_BATCH {
            return Err(JobError::bad_input(format!(
                "import batch of {} records exceeds MAX_IMPORT_BATCH ({MAX_IMPORT_BATCH})",
                recs.len()
            )));
        }
        let mut prepared = Vec::with_capacity(recs.len());
        for (i, r) in recs.iter().enumerate() {
            prepared.push(self.prepare_import(r, o).map_err(|e| at(e, i, r, label))?);
        }
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let mut out = Vec::with_capacity(prepared.len());
        for (i, p) in prepared.iter().enumerate() {
            out.push(
                self.import_one(&tx, p, o)
                    .map_err(|e| at(e, i, p.rec, label))?,
            );
        }
        tx.commit()?;
        Ok(out)
    }

    /// Everything that can be checked without the database.
    fn prepare_import<'a>(
        &self,
        rec: &'a ImportRecord,
        o: &ImportOptions,
    ) -> JobResult<Prepared<'a>> {
        if !StatusModel::is_terminal(&rec.status) {
            return Err(JobError::bad_input(format!(
                "an import needs a terminal status (complete, failed or cancelled), not {:?}",
                rec.status
            )));
        }
        if rec.kind.is_empty() {
            return Err(JobError::bad_input("import kind is empty"));
        }
        if let Some(id) = &rec.id {
            check_job_id(id)?;
        }
        if rec.request_id.as_deref() == Some("") {
            return Err(JobError::bad_input("import request_id is empty"));
        }
        if o.idempotent && rec.id.is_none() && rec.request_id.is_none() {
            return Err(JobError::bad_input(
                "an idempotent import needs an id or a request_id to recognise the record by",
            ));
        }
        if rec.attempts < 0 {
            return Err(JobError::bad_input("import attempts is negative"));
        }
        match rec.status.as_str() {
            COMPLETE if rec.error.is_some() => {
                return Err(JobError::bad_input("a complete job carries no error"))
            }
            FAILED if rec.error.is_none() => {
                return Err(JobError::bad_input("a failed job needs its error"))
            }
            _ => {}
        }
        for ts in [&rec.created_at, &rec.updated_at].into_iter().flatten() {
            check_timestamp(ts)?;
        }
        if let (Some(c), Some(u)) = (&rec.created_at, &rec.updated_at) {
            if u < c {
                return Err(JobError::bad_input(
                    "import updated_at is earlier than created_at",
                ));
            }
        }
        let (names, vals) = self.encode_new_ext(&rec.ext)?;
        let mut canon = Vec::with_capacity(names.len());
        for (k, v) in &rec.ext {
            if let Some(c) = self.cfg.column_named(k) {
                canon.push((k.clone(), c.decode(c.encode(v)?)));
            }
        }
        let mut seen = HashSet::new();
        for a in &rec.artifacts {
            if a.artifact_id.is_empty() || a.path.is_empty() {
                return Err(JobError::bad_input(
                    "import artifact needs an id and a path",
                ));
            }
            if a.bytes < 0 {
                return Err(JobError::bad_input(format!(
                    "import artifact {} has negative bytes",
                    a.artifact_id
                )));
            }
            if !a.job_id.is_empty() && rec.id.as_deref() != Some(a.job_id.as_str()) {
                return Err(JobError::bad_input(format!(
                    "import artifact {} names job {}, not the imported job",
                    a.artifact_id, a.job_id
                )));
            }
            if !seen.insert(a.artifact_id.as_str()) {
                return Err(JobError::bad_input(format!(
                    "import artifact id {} is repeated",
                    a.artifact_id
                )));
            }
        }
        Ok(Prepared {
            rec,
            names,
            vals,
            canon,
            error: rec
                .error
                .as_ref()
                .map(|e| e.to_json_with(self.cfg.includes_retryable())),
        })
    }

    /// The stored job an import of `rec` would collide with: the same id, else the same
    /// request id (which is unique in the table).
    fn find_import_target(
        &self,
        tx: &Connection,
        rec: &ImportRecord,
    ) -> JobResult<Option<JobRecord>> {
        if let Some(id) = &rec.id {
            let found = tx
                .query_row(
                    &format!("SELECT {} FROM jobs WHERE job_id=?1", self.select),
                    [id],
                    |r| self.record_row(r),
                )
                .optional()?;
            if found.is_some() {
                return Ok(found);
            }
        }
        if let Some(rid) = &rec.request_id {
            return Ok(tx
                .query_row(
                    &format!("SELECT {} FROM jobs WHERE request_id=?1", self.select),
                    [rid],
                    |r| self.record_row(r),
                )
                .optional()?);
        }
        Ok(None)
    }

    fn import_one(
        &self,
        tx: &Connection,
        p: &Prepared<'_>,
        o: &ImportOptions,
    ) -> JobResult<Imported> {
        let rec = p.rec;
        if let Some(existing) = self.find_import_target(tx, rec)? {
            let held_id = existing.job.job_id.clone();
            if !o.idempotent {
                let by = if rec.id.as_deref() == Some(held_id.as_str()) {
                    "id"
                } else {
                    "request_id"
                };
                return Err(JobError::new(
                    JOB_EXISTS,
                    format!("job {held_id} already exists (same {by}); nothing was imported"),
                ));
            }
            return match self.import_diff(tx, &existing, p)? {
                None => Ok(Imported {
                    record: existing,
                    existed: true,
                }),
                Some(field) => Err(JobError::new(
                    JOB_EXISTS,
                    format!(
                        "job {held_id} already exists and differs from the imported record in {field}"
                    ),
                )),
            };
        }

        let id = rec
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let (created, updated) = resolve_times(rec);
        let mut cols = String::from(
            "job_id, request_id, kind, request_json, status, attempts, error_json, result_json, \
             created_at, updated_at, attempt_gen",
        );
        let mut ph = String::from("?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0");
        let mut all: Vec<SqlValue> = vec![
            id.clone().into(),
            rec.request_id.clone().into(),
            rec.kind.clone().into(),
            rec.request.to_string().into(),
            rec.status.clone().into(),
            rec.attempts.into(),
            p.error.as_ref().map(|v| v.to_string()).into(),
            rec.result.as_ref().map(|v| v.to_string()).into(),
            created.into(),
            updated.clone().into(),
        ];
        for (i, name) in p.names.iter().enumerate() {
            cols.push_str(", ");
            cols.push_str(name);
            ph.push_str(&format!(",?{}", 11 + i));
        }
        all.extend(p.vals.iter().cloned());
        tx.execute(
            &format!("INSERT INTO jobs ({cols}) VALUES ({ph})"),
            params_from_iter(all),
        )?;

        let n = &self.cfg.artifacts;
        for a in &rec.artifacts {
            let owner: Option<String> = tx
                .query_row(
                    &format!("SELECT job_id FROM {} WHERE {}=?1", n.table, n.id_column),
                    [&a.artifact_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(other) = owner {
                return Err(JobError::bad_input(format!(
                    "import artifact {} is already recorded by job {other}",
                    a.artifact_id
                )));
            }
            tx.execute(
                &format!(
                    "INSERT INTO {} ({}, job_id, path, sha256, bytes, mime, created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    n.table, n.id_column
                ),
                params![
                    a.artifact_id,
                    id,
                    a.path,
                    a.sha256,
                    a.bytes,
                    a.mime,
                    updated
                ],
            )?;
        }

        let after = self.load(tx, &id)?;
        if let Some(h) = &self.cfg.hook {
            h.on_import(tx, &after)?;
        }
        Ok(Imported {
            record: after,
            existed: false,
        })
    }

    /// The first field in which the stored job differs from what the record states, or `None`
    /// when they are identical (see [`ImportOptions::idempotent`]).
    fn import_diff(
        &self,
        tx: &Connection,
        ex: &JobRecord,
        p: &Prepared<'_>,
    ) -> JobResult<Option<String>> {
        let (r, j) = (p.rec, &ex.job);
        let differs = |name: &str| Ok(Some(name.to_string()));
        if r.id.as_deref().is_some_and(|id| id != j.job_id) {
            return differs("id");
        }
        if r.request_id != j.request_id {
            return differs("request_id");
        }
        if r.kind != j.kind {
            return differs("kind");
        }
        if r.request != j.request {
            return differs("request");
        }
        if r.status != j.status {
            return differs("status");
        }
        if r.attempts != j.attempts {
            return differs("attempts");
        }
        if p.error != j.error {
            return differs("error");
        }
        if r.result != j.result {
            return differs("result");
        }
        if r.created_at.as_deref().is_some_and(|c| c != j.created_at) {
            return differs("created_at");
        }
        if r.updated_at.as_deref().is_some_and(|u| u != j.updated_at) {
            return differs("updated_at");
        }
        for (k, v) in &p.canon {
            if ex.ext.get(k) != Some(v) {
                return Ok(Some(format!("ext.{k}")));
            }
        }
        let mut have = {
            let mut st = tx.prepare(&self.art_sql("WHERE job_id=?1 ORDER BY created_at, rowid"))?;
            let rows = st
                .query_map([&j.job_id], artifact_row)?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut want: Vec<Artifact> = r
            .artifacts
            .iter()
            .map(|a| Artifact {
                job_id: j.job_id.clone(),
                ..a.clone()
            })
            .collect();
        have.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
        want.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
        if have != want {
            return differs("artifacts");
        }
        Ok(None)
    }
}

/// Name the record in an error from a batch (a single import keeps the plain message).
fn at(mut e: JobError, index: usize, rec: &ImportRecord, label: bool) -> JobError {
    if label {
        let who = rec
            .id
            .as_deref()
            .or(rec.request_id.as_deref())
            .unwrap_or("(no id)");
        e.message = format!("import record {index} ({who}): {}", e.message);
    }
    e
}

/// `(created_at, updated_at)` as stored: what the record states, else one from the other, else
/// the time of the import.
fn resolve_times(rec: &ImportRecord) -> (String, String) {
    match (&rec.created_at, &rec.updated_at) {
        (Some(c), Some(u)) => (c.clone(), u.clone()),
        (Some(c), None) => (c.clone(), c.clone()),
        (None, Some(u)) => (u.clone(), u.clone()),
        (None, None) => {
            let n = now();
            (n.clone(), n)
        }
    }
}

/// A job id is used as a file name by the lock directory and the scratch layout, so it must be a
/// single, portable path component.
fn check_job_id(id: &str) -> JobResult<()> {
    let bad = id.is_empty()
        || id.len() > 255
        || id == "."
        || id == ".."
        || id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'));
    if bad {
        return Err(JobError::bad_input(format!(
            "import id {id:?} must be a single file-name-safe token (no '/', '\\', ':' or control characters)"
        )));
    }
    Ok(())
}

/// `YYYY-MM-DDTHH:MM:SSZ`: the store compares timestamps as text, so only this shape sorts
/// correctly beside the ones it writes itself.
fn check_timestamp(s: &str) -> JobResult<()> {
    let b = s.as_bytes();
    let digits = [0usize, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
    let shaped = b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && digits.iter().all(|&i| b[i].is_ascii_digit());
    let ok = shaped && {
        let part = |from: usize, to: usize| s[from..to].parse::<u32>().unwrap_or(u32::MAX);
        (1..=12).contains(&part(5, 7))
            && (1..=31).contains(&part(8, 10))
            && part(11, 13) < 24
            && part(14, 16) < 60
            && part(17, 19) < 61
    };
    if ok {
        Ok(())
    } else {
        Err(JobError::bad_input(format!(
            "timestamp {s:?} is not YYYY-MM-DDTHH:MM:SSZ (UTC)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_have_the_stores_shape() {
        for ok in [
            "2026-01-02T03:04:05Z",
            "1970-01-01T00:00:00Z",
            "2099-12-31T23:59:59Z",
        ] {
            assert!(check_timestamp(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "2026-01-02 03:04:05",
            "2026-01-02T03:04:05",
            "2026-01-02T03:04:05.123Z",
            "2026-13-02T03:04:05Z",
            "2026-01-32T03:04:05Z",
            "2026-01-02T24:04:05Z",
            "2026-01-02T03:60:05Z",
            "2026-01-02T03:04:0xZ",
            "2026-01-02T03:04:05+00:00",
        ] {
            assert_eq!(
                check_timestamp(bad).unwrap_err().code,
                "bad_input",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn job_ids_are_single_file_name_tokens() {
        for ok in ["job-1", "0d6c2a5e-6f6e-4c3e-9b0b-4f1d4c1a0e11", "a.b_c"] {
            assert!(check_job_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "a\\b", "C:x", "a\nb", "a\0b"] {
            assert_eq!(check_job_id(bad).unwrap_err().code, "bad_input", "{bad:?}");
        }
        assert!(check_job_id(&"x".repeat(256)).is_err());
    }

    #[test]
    fn times_fill_in_from_each_other() {
        let r = ImportRecord::cancelled("k");
        let (c, u) = resolve_times(&r);
        assert_eq!(c, u);
        let r = ImportRecord::cancelled("k").created_at("2026-01-01T00:00:00Z");
        assert_eq!(resolve_times(&r).1, "2026-01-01T00:00:00Z");
        let r = ImportRecord::cancelled("k").updated_at("2026-01-02T00:00:00Z");
        assert_eq!(resolve_times(&r).0, "2026-01-02T00:00:00Z");
    }
}
